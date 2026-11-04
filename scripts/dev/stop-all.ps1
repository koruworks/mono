# SPDX-License-Identifier: MPL-2.0

# Stop every development or installed Koru process, regardless of its root or
# worktree. This is intentionally broader than `koru.bat root-stop` and exists
# both as an escape hatch and as the reliable way to release Windows exe locks.

$ErrorActionPreference = "Continue"
$failed = $false

function Get-KoruProcesses([string] $Name) {
    @(Get-Process -Name $Name -ErrorAction SilentlyContinue)
}

function Stop-KoruProcessGroup([string] $Name, [string] $Label, [bool] $CloseWindowsFirst) {
    $processes = @(Get-KoruProcesses $Name)
    if ($processes.Count -eq 0) {
        Write-Host "[Koru] ${Label}: not running"
        return
    }

    $ids = @($processes | ForEach-Object { $_.Id })

    # Give editor windows their ordinary close path first so settings and the
    # last pending snapshot can be written. Services have no interactive window
    # to close; any survivors are stopped below.
    if ($CloseWindowsFirst) {
        foreach ($process in $processes) {
            if (-not $process.HasExited) {
                [void] $process.CloseMainWindow()
            }
        }
        Start-Sleep -Milliseconds 750
    }

    $remaining = @(Get-KoruProcesses $Name | Where-Object { $ids -contains $_.Id })
    if ($remaining.Count -gt 0) {
        try {
            $remaining | Stop-Process -Force -ErrorAction Stop
            $remaining | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
        } catch {
            Write-Error "[Koru] could not stop ${Label}: $($_.Exception.Message)"
            $script:failed = $true
            return
        }
    }

    $stillRunning = @(Get-KoruProcesses $Name | Where-Object { $ids -contains $_.Id })
    if ($stillRunning.Count -gt 0) {
        Write-Error "[Koru] could not stop ${Label} process(es): $($stillRunning.Id -join ', ')"
        $script:failed = $true
        return
    }

    Write-Host "[Koru] ${Label}: stopped $($ids.Count) process(es)"
}

Write-Host "[Koru] stopping every Koru process on this machine..."
Stop-KoruProcessGroup "koru" "Koru" $true
Stop-KoruProcessGroup "koru-agent" "Agent" $false
Stop-KoruProcessGroup "koru-voice-worker" "Voice worker" $false
Stop-KoruProcessGroup "koru-com-notification" "notification COM broker" $false
# **Retired image names, kept on purpose.** The API Gateway and the session
# host are worker roles of koru-agent.exe now, so a current stack has no process
# with either name - but this script stops every Koru on the *machine*, and an
# installed pre-cutover build still runs both. Stopping by process name is what
# reaches them; nothing else here would. The standalone Koru AI window is
# retired the same way: an installed older build may still have it open.
Stop-KoruProcessGroup "koru-ai-chat" "Koru AI window (retired image)" $true
Stop-KoruProcessGroup "koru-session-host" "Session Host (retired image)" $false
Stop-KoruProcessGroup "koru-api-gateway" "API Gateway (retired image)" $false
Stop-KoruProcessGroup "koru-mcp" "MCP" $false
Stop-KoruProcessGroup "koru-store-server" "diagnostic Store host" $false
if ($failed) {
    exit 1
}

Write-Host "[Koru] all Koru processes stopped."
