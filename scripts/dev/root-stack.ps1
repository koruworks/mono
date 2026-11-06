# SPDX-License-Identifier: MPL-2.0

param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("Start", "Stop", "Open")]
    [string]$Action,

    [AllowEmptyString()]
    [string]$Root = ""
)

# The shipped launcher deliberately does NOT mirror this script, and nobody
# should later "align" the two. What is below - start the Agent, wait for its
# supervised API Gateway, then ask that Agent to launch Koru - belongs in
# koru.exe, and
# it is already there: it dials the Agent, spawns one beside itself if nothing
# answers, waits a bounded three seconds, and degrades rather than dying. A
# package's koru.bat is four lines that hand the arguments to koru.exe and get
# out of the way.
#
# This one is orchestration in PowerShell precisely because it can be:
# $LASTEXITCODE says what happened and Start-Process can hide a
# console-subsystem debug binary. cmd.exe has neither, so the same shape written
# as a .bat would be a fixed sleep and a hope. It is a development tool for
# running an isolated stack on one root, not a preview of how Koru starts for a
# user.
#
# ASCII only, like every .ps1 here: Windows PowerShell 5.1 reads a BOM-less file
# as ANSI, and a UTF-8 dash arrives as three cp1252 characters, one of which is a
# curly quote that silently opens a string.
$ErrorActionPreference = "Stop"

function Import-DotenvFile([string] $Path, [string] $Label) {
    # Every name in the file, not a list this script keeps in step by hand.
    # The cherry-picking version is why a person who set a value once had it
    # last exactly as long as that terminal did.
    if (-not (Test-Path -LiteralPath $Path)) { return 0 }
    $count = 0
    foreach ($line in Get-Content -LiteralPath $Path) {
        $text = $line.Trim()
        if ($text.Length -eq 0 -or $text.StartsWith("#")) { continue }
        $split = $text.IndexOf("=")
        if ($split -lt 1) { continue }
        $name = $text.Substring(0, $split).Trim()
        if ($name -notmatch '^[A-Za-z_][A-Za-z0-9_]*$') { continue }
        $value = $text.Substring($split + 1).Trim()
        if ($value.Length -ge 2 -and
            (($value[0] -eq '"' -and $value[$value.Length - 1] -eq '"') -or
             ($value[0] -eq "'" -and $value[$value.Length - 1] -eq "'"))) {
            $value = $value.Substring(1, $value.Length - 2)
        }
        # **Anything already in this process wins.** A value typed in the shell
        # before launching is a deliberate one-off, and a file that overrode it
        # would make that impossible to do.
        #
        # **But a disagreement is announced.** Shell-wins is also how somebody
        # ends up on production with a localhost support URL: `setup-envs` and
        # a stray `set` both outlive the command that wrote them, and the file
        # then applies four of its five lines in silence. The override still
        # stands; it just stops being invisible. Identical values say nothing,
        # or every launch would carry noise nobody reads.
        $existing = [Environment]::GetEnvironmentVariable($name, "Process")
        if (-not [string]::IsNullOrWhiteSpace($existing)) {
            if ($existing -cne $value -and -not [string]::IsNullOrWhiteSpace($value)) {
                Write-Warning "This shell overrides $name : $existing (the file says $value). Open a new terminal to use the file."
            }
            continue
        }
        if (-not [string]::IsNullOrWhiteSpace($value)) {
            [Environment]::SetEnvironmentVariable($name, $value, "Process")
            $count = $count + 1
        }
    }
    if ($count -gt 0) { Write-Host "[Koru] ${Label}: $count settings" }
    return $count
}

function Use-SharedDevelopmentDictionaries([string] $Repo) {
    $configured = [Environment]::GetEnvironmentVariable("KORU_DICTIONARIES", "Process")
    if (-not [string]::IsNullOrWhiteSpace($configured)) {
        Write-Host "[Koru] spell dictionaries: configured by KORU_DICTIONARIES"
        return
    }

    # Channel roots isolate authored data and process rendezvous. Dictionaries
    # are immutable development resources, so every disposable root in this
    # worktree may read one ignored cache instead of downloading its own copy.
    $shared = Join-Path $Repo ".dev-data\dictionaries"
    $complete = (Test-Path -LiteralPath (Join-Path $shared "manifest.toml")) -and
        (Test-Path -LiteralPath (Join-Path $shared "en-US\dictionary.aff")) -and
        (Test-Path -LiteralPath (Join-Path $shared "en-US\dictionary.dic"))
    if ($complete) {
        [Environment]::SetEnvironmentVariable("KORU_DICTIONARIES", $shared, "Process")
        Write-Host "[Koru] spell dictionaries: shared worktree cache"
        return
    }

    Write-Warning "Spell check has no development dictionaries. Run: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\setup\fetch-dictionaries.ps1 -Dest .dev-data\dictionaries"
}

# What `koru-agent status` printed, or an empty string if it could not be run.
#
# **Read rather than exit-coded.** `koru-agent status` answers "koru-agent is
# not running" and exits 0, because not running is an answer rather than a
# failure - so an exit code says only that the executable started. The two
# readiness questions below are answered from the text.
function Get-AgentStatus([string] $Executable, [string] $RootPath) {
    $previous = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        return (& $Executable --root $RootPath status 2>$null | Out-String)
    } catch {
        return ""
    } finally {
        $ErrorActionPreference = $previous
    }
}

function Invoke-NativeBestEffort([string] $Executable, [string[]] $Arguments) {
    # Exact-root stop is idempotent: an Agent that is not running answers
    # "not running" and succeeds, so this is safe to call before a start.
    $previous = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        & $Executable @Arguments *> $null
    } catch {
        # There may be nothing left to answer; the operation is already done.
    } finally {
        $ErrorActionPreference = $previous
    }
}
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)

Push-Location -LiteralPath $repo
try {
    if ([string]::IsNullOrWhiteSpace($Root)) {
        if (-not [string]::IsNullOrWhiteSpace($env:KORU_ROOT)) {
            $Root = $env:KORU_ROOT
        } else {
            $Root = Join-Path $repo ".dev-data\root-mode"
        }
    }
    # Resolve once and hand the exact same spelling to every process.
    # Endpoint isolation hashes the text, so equivalent-looking spellings are
    # intentionally not allowed to drift between launches.
    $Root = [System.IO.Path]::GetFullPath($Root)
    $bin = Join-Path $repo "target\debug"
    $agent = Join-Path $bin "koru-agent.exe"
    $app = Join-Path $bin "koru.exe"

    # **One ask, because there is one process to ask.** The API Gateway and the
    # session host are roles of koru-agent.exe now, supervised children ended by
    # the Agent's Job Object, so stopping the Agent stops the stack. There is no
    # second executable left to ask, and no `koru-api-gateway.exe` to ask it of.
    function Stop-RootServices {
        if (Test-Path -LiteralPath $agent) {
            Invoke-NativeBestEffort $agent @("--root", $Root, "stop")
        }
    }

    if ($Action -eq "Open") {
        if (-not (Test-Path -LiteralPath $agent)) {
            throw "Koru Agent is not built; run koru.bat root `"$Root`" first"
        }
        $status = Get-AgentStatus $agent $Root
        if ($status -notmatch '(?m)^running, version ') {
            throw "No Koru Agent is running for $Root; run koru.bat root `"$Root`" first"
        }
        & $agent --root $Root open
        if ($LASTEXITCODE -ne 0) {
            throw "Koru Agent could not open the workspace window (exit $LASTEXITCODE)"
        }
        exit 0
    }

    if ($Action -eq "Stop") {
        Stop-RootServices
        Write-Host "[Koru] stopped root services: $Root"
        Write-Host "[Koru] close any Koru windows on that root separately."
        exit 0
    }

    # Two files, narrowest last, and the shell beats both.
    #
    # The repository one, `.env.production` - the same file `koru.bat run`
    # reads - names the Koru Cloud this root signs in to; it survives a reboot,
    # a fresh worktree and a new channel root. Whoever runs a Cloud writes
    # their own (CONTRIBUTING.md); anybody else has none. The root one, the
    # root's own `.env.local`, is for what genuinely differs between roots --
    # the LAN address a second machine or a phone must be told, rather than
    # the loopback a single machine uses.
    #
    # **No repository file is no Cloud, said in one line** -- koru.bat's rule.
    # The Cloud's own names are cleared after both files, so neither a stale
    # shell value nor a root's own file can quietly become the Cloud a root
    # with no desktop file talks to. With a file, everything applies unchanged.
    $desktopFile = ".env.production"
    $desktopPath = Join-Path $repo $desktopFile
    $cloudConfigured = Test-Path -LiteralPath $desktopPath
    if ($cloudConfigured) {
        Write-Host "[Koru] configuration: $desktopFile"
        Import-DotenvFile $desktopPath "desktop settings" | Out-Null
    }
    Import-DotenvFile (Join-Path $Root ".env.local") "root settings" | Out-Null

    if (-not $cloudConfigured) {
        foreach ($name in @("KORU_PUBLIC_API_ORIGIN", "KORU_PUBLIC_API_ISSUER", "KORU_PUBLIC_API_CREDENTIAL_ROLLOUT")) {
            [Environment]::SetEnvironmentVariable($name, $null, "Process")
        }
        Write-Host "[Koru] no ${desktopFile}: starting with Koru Cloud not configured; CONTRIBUTING.md says how to set one up."
    } elseif ([string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable("KORU_PUBLIC_API_ORIGIN", "Process"))) {
        # **No guessing an API here.** Answering a missing origin by pointing
        # this root at some default would be the wrong-API-for-an-afternoon
        # failure wearing a helpful face. With no origin at all this root has
        # no Cloud, which is a valid choice (an empty file says so), and the
        # Agent says the same when it starts.
        Write-Warning "Koru account sign-in is unavailable in this root: $desktopFile names no KORU_PUBLIC_API_ORIGIN."
    } else {
        Write-Host "[Koru] account client: $([Environment]::GetEnvironmentVariable('KORU_PUBLIC_API_ORIGIN', 'Process'))"
    }
    Use-SharedDevelopmentDictionaries $repo
    Write-Host "[Koru] isolated root: $Root"
    # A window keeps the Gateway endpoint it discovered at launch. Leaving one
    # alive while restarting Agent can make the newly spawned Koru hand its
    # activation to that stale window; the new child then exits, Agent sees no
    # owned window, and its supervised Gateway shuts down. Stop this worktree's
    # debug Koru image before the services so the replacement window must
    # discover the replacement Gateway. Other worktrees and installed builds
    # have different executable paths and remain untouched.
    & (Join-Path $PSScriptRoot "stop-exact-exe.ps1") -ExecutablePath @($app)
    Stop-RootServices
    Start-Sleep -Seconds 1

    # **Two packages, not four.** koru-agent.exe hosts the API Gateway and the
    # session host as worker roles of its own image, so building it builds them;
    # there is no sibling to keep in step any more.
    & cargo build -p koru-agent -p koru
    if ($LASTEXITCODE -ne 0) {
        throw "the root stack did not build (close any Koru window using this debug binary and retry)"
    }

    # Start-Process joins ArgumentList into a Windows command line. Quote the
    # value explicitly so a root below a user folder with spaces stays one arg.
    # A trailing backslash escapes the closing quote under Windows' native
    # argv rules (`C:\` is the important case), so double trailing slashes.
    $escapedRoot = $Root -replace '(\\+)$', '$1$1'
    $rootArg = '"' + $escapedRoot.Replace('"', '\"') + '"'
    $agentProcess = Start-Process `
        -FilePath $agent `
        -ArgumentList @("--root", $rootArg, "--tray") `
        -WorkingDirectory $repo `
        -WindowStyle Hidden `
        -PassThru

    # **Both questions come out of one answer now.** `koru-agent status` prints
    # whether the Agent is running and, on its own line, whether a gateway is
    # serving on this root - the second probed directly against the gateway's
    # rendezvous rather than taken from what the Agent believes about its child,
    # so a wedged supervisor cannot report a stack that is not there. The
    # command it replaces, `koru-api-gateway --root <root> status`, went with
    # the executable.
    $agentReady = $false
    $gatewayReady = $false
    for ($attempt = 0; $attempt -lt 200; $attempt++) {
        Start-Sleep -Milliseconds 100
        $status = Get-AgentStatus $agent $Root
        $agentReady = $status -match '(?m)^running, version '
        $gatewayReady = $status -match '(?m)^\s*gateway:\s+serving'
        if ($agentReady -and $gatewayReady) {
            break
        }
        if ($agentProcess.HasExited) {
            break
        }
    }
    if (-not $agentReady) {
        throw "Koru Agent did not become ready for $Root"
    }
    if (-not $gatewayReady) {
        throw "Koru API Gateway did not become ready for $Root"
    }

    Write-Host "[Koru] API Gateway ready, Agent pid $($agentProcess.Id)"
    Write-Host "[Koru] rooted Agent global shortcuts are enabled; Windows arbitrates conflicts"
    Write-Host "[Koru] stop services with: koru.bat root-stop `"$Root`""
    Write-Host "[Koru] asking the Agent to launch Koru..."
    # The Agent spawns the window, with the Agent's own root. workspace.destroy
    # and workspace.migrate need the Agent's vouch, which it gives any process
    # running the koru.exe beside it (ADR-267).
    & $agent --root $Root open
    if ($LASTEXITCODE -ne 0) {
        throw "Koru Agent could not open the workspace window (exit $LASTEXITCODE)"
    }
} catch {
    Write-Error $_
    exit 1
} finally {
    Pop-Location
}
