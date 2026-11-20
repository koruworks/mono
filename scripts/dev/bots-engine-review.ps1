# SPDX-License-Identifier: MPL-2.0

# Optional isolated review binaries. The ordinary development launcher uses
# the same native Bots engine and needs no feature switch.
param(
    [switch]$BuildOnly,
    [switch]$NoBuild
)

$ErrorActionPreference = 'Stop'
$repo = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$reviewTarget = Join-Path $repo 'target/bots-engine-review'
$app = Join-Path $reviewTarget 'debug/koru.exe'
$agent = Join-Path $reviewTarget 'debug/koru-agent.exe'

Push-Location -LiteralPath $repo
try {
    if (-not $NoBuild) {
        # Do not replace a review executable while a person is using it.
        $running = @(Get-Process koru,koru-agent -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -eq $app -or $_.Path -eq $agent })
        if ($running.Count -gt 0) {
            throw 'Close the Bots review window and its Agent before rebuilding, or use -NoBuild for another fresh root.'
        }
        & cargo build -p koru -p koru-agent -p koru-store-server `
            --target-dir $reviewTarget --locked --offline -j1
        if ($LASTEXITCODE -ne 0) { throw 'The Bots engine review build failed.' }
    }
    if ($BuildOnly) { return }
    if (!(Test-Path -LiteralPath $app) -or !(Test-Path -LiteralPath $agent)) {
        throw 'Build the review binaries first by running this script without -NoBuild.'
    }
    # A new root every time: never reinterpret an older engine schema or the
    # ordinary development root's legacy Bot data as this review's state.
    $reviewRoot = Join-Path $env:TEMP ('koru-bots-review-' + [guid]::NewGuid().ToString('N'))
    Write-Host "[Koru] Bots engine review root: $reviewRoot"
    Start-Process -FilePath $app -ArgumentList @('--root', ('"' + $reviewRoot + '"')) `
        -WorkingDirectory $repo -WindowStyle Hidden
} finally {
    Pop-Location
}
