# SPDX-License-Identifier: MPL-2.0

<#
.SYNOPSIS
Capture Koru's intermittent UI stall diagnostics.

.DESCRIPTION
Runs a debug build with the startup/stall watchdog and frame timing enabled.
The full console output is written to a timestamped log while remaining visible.
When Koru exits, the script prints the stall and frame summary lines again.

.PARAMETER Profile
Optional profile path to open. Use a copied profile when reproducing against
real content is not required. A supplied path is passed directly to cargo, so
paths containing spaces are supported.

.PARAMETER LogPath
Optional output path. Relative paths are resolved from the directory where the
script was invoked. The default is a timestamped file in the system temp folder.

.PARAMETER NoSearch
Start Koru with --no-search so the capture excludes all search initialization
and background indexing without modifying the existing local index.

.EXAMPLE
.\scripts\dev\capture-stalls.ps1

.EXAMPLE
.\scripts\dev\capture-stalls.ps1 -Profile 'C:\profiles\my-profile'
#>
[CmdletBinding()]
param(
    [string]$Profile,
    [string]$LogPath,
    [switch]$NoSearch
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$invokedFrom = (Get-Location).Path
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'

if ([string]::IsNullOrWhiteSpace($LogPath)) {
    $LogPath = Join-Path ([System.IO.Path]::GetTempPath()) "koru-stall-$stamp.log"
} elseif (-not [System.IO.Path]::IsPathRooted($LogPath)) {
    $LogPath = Join-Path $invokedFrom $LogPath
}
$LogPath = [System.IO.Path]::GetFullPath($LogPath)

$logDirectory = Split-Path -Parent $LogPath
if (-not (Test-Path -LiteralPath $logDirectory)) {
    New-Item -ItemType Directory -Force -Path $logDirectory | Out-Null
}

$hadStartupStats = Test-Path Env:KORU_STARTUP_STATS
$oldStartupStats = $env:KORU_STARTUP_STATS
$hadFrameStats = Test-Path Env:KORU_FRAME_STATS
$oldFrameStats = $env:KORU_FRAME_STATS
$exitCode = 0

Write-Host "koru stall capture"
Write-Host "Log: $LogPath"
Write-Host "Reproduce the pause, then close Koru to see the filtered results."
Write-Host ""

Push-Location $repoRoot
try {
    $env:KORU_STARTUP_STATS = '1'
    $env:KORU_FRAME_STATS = '1'

    # Windows PowerShell 5.1 turns a native program's stderr into a terminating
    # NativeCommandError when ErrorActionPreference is Stop. Cargo writes normal
    # progress to stderr, so relax only around the native launch or the capture
    # can stop before Koru opens.
    $nativeErrorAction = $ErrorActionPreference
    $logWriter = $null
    try {
        $ErrorActionPreference = 'Continue'
        if ([string]::IsNullOrWhiteSpace($Profile)) {
            $nativeCommand = Join-Path $repoRoot 'koru.bat'
            $nativeArgs = @('run')
            if ($NoSearch) {
                $nativeArgs += @('--', '--no-search')
            }
        } else {
            $nativeCommand = 'cargo'
            $nativeArgs = @('run', '--', '--profile', $Profile)
            if ($NoSearch) {
                $nativeArgs += '--no-search'
            }
        }

        # A merged native stream contains ErrorRecord objects under Windows
        # PowerShell 5.1. Tee-Object serializes their formatting metadata and
        # chooses its own file encoding, producing NativeCommandError noise and
        # mojibake. Flatten every item to its displayed text and own the UTF-8
        # writer explicitly while still echoing each line to the console.
        $utf8 = New-Object System.Text.UTF8Encoding($false)
        $logWriter = New-Object System.IO.StreamWriter($LogPath, $false, $utf8)
        $logWriter.AutoFlush = $true
        & $nativeCommand @nativeArgs 2>&1 | ForEach-Object {
            $line = [string]$_
            Write-Host $line
            $logWriter.WriteLine($line)
        }
        $exitCode = $LASTEXITCODE
    } finally {
        if ($null -ne $logWriter) {
            $logWriter.Dispose()
        }
        $ErrorActionPreference = $nativeErrorAction
    }
} finally {
    Pop-Location

    if ($hadStartupStats) {
        $env:KORU_STARTUP_STATS = $oldStartupStats
    } else {
        Remove-Item Env:KORU_STARTUP_STATS -ErrorAction SilentlyContinue
    }

    if ($hadFrameStats) {
        $env:KORU_FRAME_STATS = $oldFrameStats
    } else {
        Remove-Item Env:KORU_FRAME_STATS -ErrorAction SilentlyContinue
    }
}

Write-Host ""
Write-Host "Captured diagnostic lines:"
$diagnostics = Select-String -LiteralPath $LogPath -Pattern 'stall:|frame x'
if ($diagnostics) {
    foreach ($line in $diagnostics) {
        Write-Host $line.Line
    }
} else {
    Write-Host "No instrumented 50ms stall was reported."
    Write-Host "Frame summaries also require 120 rendered frames."
}
Write-Host ""
Write-Host "Full log: $LogPath"

if ($exitCode -ne 0) {
    exit $exitCode
}
