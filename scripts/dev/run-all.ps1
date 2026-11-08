# SPDX-License-Identifier: MPL-2.0

# Start this worktree's complete debug stack after `koru.bat` has stopped its
# exact-path images - and, on the shared internal-debug channel, every other
# checkout's debug stack (`stop-worktree.ps1 -SharedChannel`). Agent supervises
# API Gateway exactly as it does in the product; Store is an in-gateway worker,
# never a separately launched daemon.
# The services stay up while Koru is open; closing it or pressing Ctrl+C runs
# the exact-path stop helper.

param(
    [switch] $Mcp,
    # Open the Koru workspace surface against this explicitly named profile.
    [string] $Profile,
    # Run Koru with the stall diagnostics on: the same environment
    # capture-stalls.ps1 sets (KORU_STARTUP_STATS, KORU_FRAME_STATS) plus the
    # KORU_JANK dispatch watch. Koru inherits Agent's console so startup/frame
    # counters stay visible, while dispatch stalls remain in KORU_JANK_LOG.
    [switch] $Performance
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$bin = Join-Path $repo "target\debug"
$agent = Join-Path $bin "koru-agent.exe"
$app = Join-Path $bin "koru.exe"
$mcpBridge = Join-Path $bin "koru-mcp.exe"

$root = $null
if (-not [string]::IsNullOrWhiteSpace($env:KORU_ROOT)) {
    $root = [System.IO.Path]::GetFullPath($env:KORU_ROOT)
    Write-Host "[Koru] channel root: $root"
}
$effectiveGatewayRoot = if ($null -ne $root) {
    Join-Path $root "store"
} else {
    Join-Path $env:LOCALAPPDATA "Koru\internal-debug\store"
}
$exitCode = 0
$failure = $null
$cancelHandlerInstalled = $false
$script:payloadLogJob = $null
$script:lastReadyProbe = $null
$readinessAttempts = 150
$appProcess = $null

# PowerShell normally treats Ctrl+C as pipeline cancellation. Install a small
# process-wide handler so the script can turn it into an orderly trip through
# `finally`, where the same exact-path shutdown as `koru.bat stop-all` runs.
Add-Type -TypeDefinition @"
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;

public static class KoruRunAllCancellation
{
    public static volatile bool Requested;
    private static readonly HandlerRoutine Handler = OnControl;

    private delegate bool HandlerRoutine(uint controlType);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetConsoleCtrlHandler(HandlerRoutine handler, bool add);

    public static void Install()
    {
        Requested = false;
        if (!SetConsoleCtrlHandler(Handler, true))
        {
            throw new Win32Exception(Marshal.GetLastWin32Error());
        }
    }

    public static void Remove()
    {
        SetConsoleCtrlHandler(Handler, false);
    }

    private static bool OnControl(uint controlType)
    {
        // CTRL_C_EVENT and CTRL_BREAK_EVENT. Returning true keeps PowerShell's
        // own handler from terminating the process before `finally` can run.
        if (controlType == 0 || controlType == 1)
        {
            Requested = true;
            return true;
        }
        return false;
    }
}
"@
[KoruRunAllCancellation]::Install()
$cancelHandlerInstalled = $true

function Quote-NativeArgument([string] $Value) {
    # Start-Process joins ArgumentList into one Windows command line. Preserve
    # spaces and double trailing slashes so one cannot escape the closing quote.
    $escaped = $Value -replace '(\\+)$', '$1$1'
    '"' + $escaped.Replace('"', '\"') + '"'
}

function Assert-NotCancelled {
    if ([KoruRunAllCancellation]::Requested) {
        throw [System.OperationCanceledException]::new("Ctrl+C received")
    }
}

function Test-NativeReady([string] $Executable, [string[]] $Arguments) {
    # Windows PowerShell promotes a native program's stderr to an ErrorRecord.
    # A normal "not listening yet" probe must therefore run under Continue;
    # otherwise the script's Stop policy aborts the readiness loop on its first
    # race with process startup instead of observing the non-zero exit code.
    $previous = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        $output = @(& $Executable @Arguments 2>&1)
        $script:lastReadyProbe = ($output | ForEach-Object { $_.ToString() }) -join [Environment]::NewLine
        return $LASTEXITCODE -eq 0
    } finally {
        $ErrorActionPreference = $previous
    }
}

function Test-EnvironmentSwitch([string] $Value) {
    if ([string]::IsNullOrWhiteSpace($Value)) {
        return $false
    }
    @("0", "false", "no", "off") -notcontains $Value.Trim().ToLowerInvariant()
}

function Receive-PayloadLog {
    if ($null -eq $script:payloadLogJob) {
        return
    }
    Receive-Job -Job $script:payloadLogJob | ForEach-Object { Write-Host $_ }
}

function Get-WorktreeUiProcesses {
    $expected = @(
        [System.IO.Path]::GetFullPath($app)
    )
    @(
        Get-Process -Name "koru" -ErrorAction SilentlyContinue |
            ForEach-Object {
                try {
                    if ($expected -contains [System.IO.Path]::GetFullPath($_.Path)) {
                        $_
                    }
                } catch {
                    # A process whose image path cannot be read cannot be proved
                    # to belong to this worktree and is left untouched.
                }
            }
    )
}

function Test-AgentReady([string] $Executable, [string[]] $Arguments) {
    # `koru-agent status` treats "not running" as a successful diagnostic, so
    # its exit code alone is not readiness. The first line of a live response
    # is deliberately stable and contains no private state.
    (Test-NativeReady $Executable $Arguments) -and
        $script:lastReadyProbe.StartsWith("running, version ")
}

function Get-LegacyDebugStores {
    @(
        Get-Process -Name "koru-store" -ErrorAction SilentlyContinue | ForEach-Object {
            try {
                $path = [System.IO.Path]::GetFullPath($_.Path)
                # Debug builds share `internal-debug`. A legacy Store from a
                # sibling worktree therefore owns the same default root claim
                # this worktree's embedded Store worker must acquire.
                if ($path -match '(?i)\\target\\debug\\koru-store\.exe$') {
                    [pscustomobject]@{ Id = $_.Id; Path = $path }
                }
            } catch {
                # A process that withholds its path cannot be identified as a
                # legacy debug Store safely.
            }
        }
    )
}

function Write-LegacyStoreConflict($Stores) {
    Write-Host "[Koru] a legacy standalone Store already owns the shared internal-debug root claim:"
    foreach ($process in $Stores) {
        Write-Host "       pid $($process.Id): $($process.Path)"
    }
    Write-Host "       run-all is not launching koru-store.exe; Store now runs inside the gateway."
    Write-Host "       The retained claim prevents both implementations writing the same databases."
    Write-Host "       Close the MCP/development host that launched the legacy Store, or use"
    Write-Host "       'koru.bat stop' when stopping every Koru build is intended."
}

function Write-ForeignServiceHint {
    $legacyStores = @(Get-LegacyDebugStores)
    if ($legacyStores.Count -gt 0) {
        Write-LegacyStoreConflict $legacyStores
        return
    }
    # The API Gateway and the session host are roles of koru-agent.exe, so a
    # foreign gateway now shows up as a foreign koru-agent. The retired image
    # names stay on this list because an installed pre-cutover build still runs
    # them and they still hold this worktree's root.
    $ours = @($agent) | ForEach-Object { [System.IO.Path]::GetFullPath($_) }
    $foreign = @(
        Get-Process -Name "koru-agent", "koru-api-gateway", "koru-session-host", "koru-store", "koru-store-server" `
            -ErrorAction SilentlyContinue | ForEach-Object {
                try {
                    $path = [System.IO.Path]::GetFullPath($_.Path)
                    if ($ours -notcontains $path) {
                        [pscustomobject]@{ Id = $_.Id; Path = $path }
                    }
                } catch {
                    # A process that withholds its path cannot be proved to be
                    # this worktree or the competing owner.
                }
            }
    )
    if ($foreign.Count -eq 0) {
        return
    }
    Write-Host "[Koru] another Koru service may own the shared development endpoint:"
    foreach ($process in $foreign) {
        Write-Host "       pid $($process.Id): $($process.Path)"
    }
    Write-Host "       Close the other development stack, use 'koru.bat stop' to stop all Koru builds,"
    Write-Host "       or set KORU_ROOT to run this worktree on an isolated root."
}

Push-Location -LiteralPath $repo
try {
    if ($null -eq $root) {
        $legacyStores = @(Get-LegacyDebugStores)
        if ($legacyStores.Count -gt 0) {
            Write-LegacyStoreConflict $legacyStores
            throw "the embedded Store cannot acquire its root while a legacy Store owns it"
        }
    }
    # Agent discovers its supervised and window siblings beside its own
    # executable.
    $packages = @(
        "-p", "koru-agent",
        "-p", "koru",
        "-p", "koru-voice-worker"
    )
    if ($Mcp) {
        $packages += @("-p", "koru-mcp")
    }
    # Every product worker carries both runtimes. The launcher has already
    # verified sherpa's pinned native archive and the Whisper build tools.
    Write-Host "[koru] building the voice worker with Whisper and Sherpa"
    & cargo build @packages
    Assert-NotCancelled
    if ($LASTEXITCODE -ne 0) {
        throw "the worktree stack did not build"
    }

    if ($Performance) {
        # Set diagnostics before Agent starts: Agent is the security boundary
        # that launches Koru, and the child inherits this environment through
        # it. Setting these after Agent startup would not instrument the
        # supervised workspace window.
        $env:KORU_STARTUP_STATS = '1'
        $env:KORU_FRAME_STATS = '1'
        $env:KORU_JANK = '1'
        if ([string]::IsNullOrWhiteSpace($env:KORU_JANK_LOG)) {
            $env:KORU_JANK_LOG = Join-Path $repo 'koru-jank.log'
        }
    }

    [string[]] $agentStartArgs = if ($null -ne $root) {
        @("--root", (Quote-NativeArgument $root), "--console", "--tray")
    } else {
        @("--console", "--tray")
    }
    [string[]] $agentStatusArgs = if ($null -ne $root) {
        @("--root", $root, "status")
    } else {
        @("status")
    }
    $agentLaunch = @{
        FilePath = $agent
        WorkingDirectory = $repo
        NoNewWindow = $true
        PassThru = $true
    }
    if ($agentStartArgs.Count -gt 0) {
        $agentLaunch.ArgumentList = $agentStartArgs
    }
    $agentProcess = Start-Process @agentLaunch

    $agentReady = $false
    for ($attempt = 0; $attempt -lt $readinessAttempts; $attempt++) {
        Start-Sleep -Milliseconds 100
        Assert-NotCancelled
        if ($agentProcess.HasExited) {
            break
        }
        if ((Test-AgentReady $agent $agentStatusArgs) -and -not $agentProcess.HasExited) {
            $agentReady = $true
            break
        }
    }
    if (-not $agentReady) {
        throw "Koru Agent did not become ready: $script:lastReadyProbe"
    }

    # **Asked of the Agent, and probed rather than believed.** The gateway is
    # a worker role of koru-agent.exe now, so there is no second executable to
    # run `status` against; `koru-agent status` prints a `gateway:` line that it
    # gets by dialling the gateway's own rendezvous, not by reporting what the
    # supervisor thinks about its child.
    $gatewayReady = $false
    for ($attempt = 0; $attempt -lt $readinessAttempts; $attempt++) {
        Start-Sleep -Milliseconds 100
        Assert-NotCancelled
        if ($agentProcess.HasExited) {
            break
        }
        if ((Test-NativeReady $agent $agentStatusArgs) -and
            $script:lastReadyProbe -match '(?m)^\s*gateway:\s+serving') {
            $gatewayReady = $true
            break
        }
    }
    if (-not $gatewayReady) {
        Write-ForeignServiceHint
        throw "Koru API Gateway did not become ready: $script:lastReadyProbe"
    }

    if (Test-EnvironmentSwitch $env:KORU_AI_PAYLOAD_DEBUG) {
        # koru-log rotates on UTC dates; using the local date silently follows
        # yesterday's file for users west of UTC during evening testing.
        $payloadDate = (Get-Date).ToUniversalTime().ToString("yyyy-MM-dd")
        $payloadLog = Join-Path $effectiveGatewayRoot (
            "logs\koru-api-gateway.log.{0}" -f $payloadDate
        )
        # The supervised Gateway is intentionally file-only. Follow just the
        # explicit, one-line request/response records so opting into this diagnostic
        # makes them visible in the run-all terminal without dumping the rest
        # of the Gateway's retained log.
        $script:payloadLogJob = Start-Job -ScriptBlock {
            param([string] $Path)
            while (-not (Test-Path -LiteralPath $Path)) {
                Start-Sleep -Milliseconds 100
            }
            Get-Content -LiteralPath $Path -Tail 0 -Wait |
                Where-Object {
                    $_ -like "*KORU_AI_PAYLOAD:*" -or
                    $_ -like "*KORU_AI_RESPONSE:*"
                }
        } -ArgumentList $payloadLog
        Write-Host "[Koru] AI payload debug on; streaming complete request/response JSON"
        Write-Host "[Koru] AI payload log: $payloadLog"
    }

    if ($Mcp) {
        [string[]] $mcpDoctorArgs = if ($null -ne $root) {
            @("doctor", "--root", $root)
        } else {
            @("doctor")
        }
        & $mcpBridge @mcpDoctorArgs
        if ($LASTEXITCODE -ne 0) {
            throw "the MCP bridge could not reach the API Gateway"
        }
        Write-Host "[Koru] MCP bridge ready for a stdio host to launch"
    }

    Write-Host "[Koru] API Gateway ready, Agent pid $($agentProcess.Id)"
    Write-Host "[Koru] asking Agent to launch Koru; Ctrl+C stops this worktree's complete stack"
    [string[]] $appStartArgs = @("open", "--inherit-stdio")
    if ($null -ne $root) {
        $appStartArgs += @("--root", $root)
    }
    if (-not [string]::IsNullOrWhiteSpace($Profile)) {
        $appStartArgs += @("--profile", $Profile)
    }
    if ($Performance) {
        Write-Host "[Koru] performance capture on: stalls -> $($env:KORU_JANK_LOG)"
        Write-Host "[Koru] startup and frame counters will print in this console"
    }
    $appOutput = @(& $agent @appStartArgs)
    if ($LASTEXITCODE -ne 0) {
        throw "Koru Agent could not open the workspace window (exit $LASTEXITCODE)"
    }
    foreach ($line in $appOutput) { Write-Host $line }
    $pidLine = $appOutput | Select-String -Pattern '^opened Koru, pid ([0-9]+)$' | Select-Object -First 1
    if ($null -eq $pidLine) {
        throw "Koru Agent restored an existing window after run-all had stopped this worktree; cannot monitor the new session"
    }
    $appPid = [int] $pidLine.Matches[0].Groups[1].Value
    $appProcess = [System.Diagnostics.Process]::GetProcessById($appPid)

    $appExitReported = $false
    $processFailure = $null
    while ($true) {
        Receive-PayloadLog
        if ([KoruRunAllCancellation]::Requested) {
            Write-Host "`n[Koru] Ctrl+C received; stopping this worktree's complete stack..."
            break
        }
        if (-not $appExitReported -and $appProcess.HasExited) {
            $appExitReported = $true
            $appExitCode = $appProcess.ExitCode
            if ($appExitCode -ne 0) {
                $processFailure = "Koru exited with code $appExitCode"
                if ((Get-WorktreeUiProcesses).Count -gt 0) {
                    Write-Warning "$processFailure; services remain available to another open Koru UI"
                }
            } elseif ((Get-WorktreeUiProcesses).Count -gt 0) {
                Write-Host "[Koru] workspace window closed; services remain available to another open Koru UI"
            }
        }
        if ((Get-WorktreeUiProcesses).Count -eq 0) {
            break
        }
        Start-Sleep -Milliseconds 100
    }
    if (-not [KoruRunAllCancellation]::Requested -and $null -ne $processFailure) {
        throw $processFailure
    }
} catch {
    if ($_.Exception -is [System.OperationCanceledException]) {
        Write-Host "`n[Koru] Ctrl+C received; stopping this worktree's complete stack..."
    } else {
        $failure = $_
        $exitCode = 1
    }
} finally {
    try {
        & (Join-Path $PSScriptRoot "stop-worktree.ps1")
    } catch {
        if ($null -eq $failure) {
            $failure = $_
            $exitCode = 1
        } else {
            Write-Warning "Koru cleanup also failed: $_"
        }
    }
    if ($null -ne $script:payloadLogJob) {
        Receive-PayloadLog
        Stop-Job -Job $script:payloadLogJob -ErrorAction SilentlyContinue
        Remove-Job -Job $script:payloadLogJob -Force -ErrorAction SilentlyContinue
        $script:payloadLogJob = $null
    }
    # Keep swallowing repeated Ctrl+C presses until exact-path shutdown has
    # reached its quiet period; otherwise a second press could interrupt it.
    if ($cancelHandlerInstalled) {
        [KoruRunAllCancellation]::Remove()
    }
    Pop-Location
}

if ($Performance) {
    Write-Host ""
    Write-Host "Startup/frame diagnostics were printed live in this console."
    Write-Host "Dispatch stalls (KORU_JANK): $($env:KORU_JANK_LOG)"
}

if ($null -ne $failure) {
    Write-Error $failure -ErrorAction Continue
}
exit $exitCode
