# SPDX-License-Identifier: MPL-2.0

<#
.SYNOPSIS
Time how long a Koru launch takes to reach a window that takes keystrokes.

.DESCRIPTION
Starts the app, waits for its window, closes it, ends the process and repeats,
then prints the median and spread of three moments measured from just before
the process was created:

  visible     the window is on screen (Koru reveals only after its first frame)
  responsive  the window's thread answers a message, so a keystroke is handled
  foreground  the window has keyboard focus

and, for a Koru build, the in-app "first frame presented in" figure from
koru.log. The gap between that and `visible` is process creation and DLL
loading, which no timer inside the process can see.

Every run is a cold start in the sense that matters day to day: the process
is not running, and the OS file cache is warm. Koru hides to the tray when its
window closes, so each run ends the process after closing the window. Its
Agent is left running, as it is after sign-in; the first launch after an Agent
starts is slow for unrelated reasons, which is what -Warmup discards.

Another app can be timed the same way for comparison: -Exe for a plain
executable, -Aumid for a packaged one (shell:AppsFolder ids, as listed by
Get-StartApps), with -ProcessName when the window belongs to a process other
than the one started.

Run nothing else heavy meanwhile: a build in the background moves every number.

.PARAMETER Exe
The executable to start. Default: this checkout's target\release\koru.exe.

.PARAMETER Channel
Time an installed Koru instead: stable or nightly.

.PARAMETER Aumid
Time a packaged app by its AppUserModelID instead of an executable.

.PARAMETER ProcessName
Name of the process that owns the window, when it is not the one started.

.PARAMETER Arguments
Command line for the app. Called from koru.bat, a value starting with '-' does
not survive the trip; use -NewDocument for the common one.

.PARAMETER NewDocument
Launch as the taskbar's New document does (--new-document).

.PARAMETER Runs
Measured launches. Default 15.

.PARAMETER Warmup
Launches run first and not counted. Default 1.

.PARAMETER MaxP50Ms
Fail (exit 1) when the median `responsive` time is above this.

.PARAMETER CloseRunning
End an already-running copy of the target before starting, instead of refusing.

.EXAMPLE
koru.bat bench

.EXAMPLE
.\scripts\dev\launch-bench.ps1 -Channel nightly -Runs 20 -MaxP50Ms 450

.EXAMPLE
.\scripts\dev\launch-bench.ps1 -Exe "$env:WINDIR\System32\charmap.exe" -Runs 5
#>

param(
    [string] $Exe,
    [ValidateSet('', 'stable', 'nightly')] [string] $Channel = '',
    [string] $Aumid,
    [string] $ProcessName,
    [string] $Arguments = '',
    [switch] $NewDocument,
    [int] $Runs = 15,
    [int] $Warmup = 1,
    [int] $MaxP50Ms = 0,
    [switch] $CloseRunning,
    [int] $GapMs = 1500,
    [int] $TimeoutMs = 15000
)
$ErrorActionPreference = 'Stop'
Add-Type -Path (Join-Path $PSScriptRoot 'launch-bench.cs')

if ($NewDocument) { $Arguments = ("--new-document " + $Arguments).Trim() }
$repo = Split-Path (Split-Path $PSScriptRoot)
if ($Channel) {
    $folder = if ($Channel -eq 'stable') { 'Koru' } else { 'Koru Nightly' }
    $Exe = Join-Path $env:LOCALAPPDATA "Programs\$folder\koru.exe"
} elseif (-not $Exe -and -not $Aumid) {
    $Exe = Join-Path $repo 'target\release\koru.exe'
}
if ($Exe -and -not (Test-Path $Exe)) { throw "No executable at $Exe" }
if (-not $ProcessName -and $Exe) { $ProcessName = [IO.Path]::GetFileNameWithoutExtension($Exe) }
$isKoru = $Exe -and ([IO.Path]::GetFileName($Exe) -ieq 'koru.exe')
$label = if ($Aumid) { $Aumid } else { $Exe }

# The copies of this target that are running: by exact path for an executable,
# so a stable Koru is never mistaken for this checkout's build.
function Get-Running {
    if (-not $ProcessName) { return @() }
    @(Get-Process -Name $ProcessName -ErrorAction SilentlyContinue |
        Where-Object { -not $Exe -or $_.Path -eq (Resolve-Path $Exe).Path })
}

function Stop-Target([System.Diagnostics.Process[]] $procs) {
    foreach ($p in $procs) {
        if ($p.MainWindowHandle -ne 0) { [void]$p.CloseMainWindow() }
    }
    Start-Sleep -Milliseconds 1500
    foreach ($p in $procs) {
        if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
    }
}

# Koru's own figure for the same launch, from whichever koru.log the process
# wrote: its "application started" line names the pid, and the cold-start
# "first frame presented in" line follows it.
function Get-InAppFirstFrame([uint32] $processId, [datetime] $since) {
    $logs = Get-ChildItem (Join-Path $env:LOCALAPPDATA 'koru\*\store\logs\koru.log*') -ErrorAction SilentlyContinue |
        Where-Object { $_.LastWriteTime -ge $since.AddSeconds(-2) }
    foreach ($log in $logs) {
        # Shared read: the Agent and every running Koru hold their logs open,
        # and the figure is a bonus, never a reason to fail a run.
        try {
            $stream = [IO.FileStream]::new($log.FullName, 'Open', 'Read', 'ReadWrite, Delete')
            $reader = [IO.StreamReader]::new($stream)
            try {
                $seen = $false
                while ($null -ne ($line = $reader.ReadLine())) {
                    if (-not $seen) { $seen = $line.Contains("pid=$processId ") -and $line.Contains('application started'); continue }
                    if ($line -match 'first frame presented in (\d+)ms') { return [int]$Matches[1] }
                    if ($line.Contains('application started')) { break }
                }
            } finally { $reader.Dispose() }
        } catch { }
    }
    return $null
}

$running = Get-Running
if ($running.Count) {
    if (-not $CloseRunning) {
        throw "$label is already running (pid $($running.Id -join ', ')). Close it, or pass -CloseRunning."
    }
    Stop-Target $running
}

Write-Host "Timing $label $Arguments  ($Warmup warm-up, $Runs measured)"
$rows = @()
for ($i = 1 - $Warmup; $i -le $Runs; $i++) {
    $started = Get-Date
    $r = [LaunchBench]::Run($Exe, $Arguments, $Aumid, $ProcessName, $TimeoutMs)
    if ($r.Error) { throw "Run $i failed: $($r.Error)" }
    Start-Sleep -Milliseconds 300
    [LaunchBench]::Close($r.Hwnd)
    $proc = Get-Process -Id $r.Pid -ErrorAction SilentlyContinue
    if ($proc) { [void]$proc.WaitForExit(1500); if (-not $proc.HasExited) { Stop-Process -Id $r.Pid -Force } }
    $inApp = if ($isKoru) { Get-InAppFirstFrame $r.Pid $started } else { $null }
    $row = [pscustomobject]@{
        run           = if ($i -le 0) { "warm-up" } else { $i }
        visible_ms    = [math]::Round($r.VisibleMs)
        responsive_ms = [math]::Round($r.ResponsiveMs)
        foreground_ms = if ($r.ForegroundMs -ge 0) { [math]::Round($r.ForegroundMs) } else { $null }
        in_app_ms     = $inApp
    }
    Write-Host ("{0,8}  visible {1,5}  responsive {2,5}  foreground {3,5}  in-app first frame {4,5}" -f `
        $row.run, $row.visible_ms, $row.responsive_ms, $row.foreground_ms, $row.in_app_ms)
    if ($i -ge 1) { $rows += $row }
    Start-Sleep -Milliseconds $GapMs
}

function Summary($name) {
    $v = @($rows | ForEach-Object { $_.$name } | Where-Object { $null -ne $_ } | Sort-Object)
    if (-not $v.Count) { return }
    $at = { param($q) $v[[math]::Min($v.Count - 1, [math]::Floor($q * ($v.Count - 1) + 0.5))] }
    [pscustomobject]@{
        metric = $name; n = $v.Count
        p50 = & $at 0.5; p25 = & $at 0.25; p75 = & $at 0.75; p95 = & $at 0.95
        min = $v[0]; max = $v[-1]
    }
}
$summary = 'visible_ms', 'responsive_ms', 'foreground_ms', 'in_app_ms' | ForEach-Object { Summary $_ }
$summary | Format-Table -AutoSize | Out-String -Width 200 | Write-Host

$csv = Join-Path ([IO.Path]::GetTempPath()) ("koru-launch-bench-{0:yyyyMMdd-HHmmss}.csv" -f (Get-Date))
$rows | Export-Csv -NoTypeInformation $csv
Write-Host "Runs written to $csv"

if ($MaxP50Ms -gt 0) {
    $p50 = ($summary | Where-Object metric -eq 'responsive_ms').p50
    if ($p50 -gt $MaxP50Ms) {
        Write-Host "FAIL: median responsive $p50 ms is above $MaxP50Ms ms"
        exit 1
    }
    Write-Host "OK: median responsive $p50 ms is within $MaxP50Ms ms"
}
