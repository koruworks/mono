# SPDX-License-Identifier: MPL-2.0

param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("Prepare", "Inspect", "Grep")]
    [string]$Action,

    # The isolated channel root the procedure runs on (the `--root` of
    # koru.bat root). Its store is <Root>\store.
    [Parameter(Mandatory = $true)]
    [string]$Root,

    # Prepare: the store directory to copy catalog.db from (the real one is
    # %LOCALAPPDATA%\Koru\<channel>\store). The Store that owns it must be
    # stopped first: `koru.bat stop-all`, or quit it from the tray.
    [string]$Source = "",

    # Prepare: also copy <Source>\workspaces\ so the copied catalog opens
    # with its content. Without it the catalog alone is enough for the
    # migration procedure; every workspace reads as absent bytes.
    [switch]$WithWorkspaces,

    # Inspect: the diagnostic host to run. Built by `cargo build -p koru-store-server`.
    [string]$Exe = "target\debug\koru-store-server.exe",

    # Grep: the sentinel names and UUIDs that must appear nowhere.
    [string[]]$Needle = @(),

    # Grep: files to scan besides <Root>\store\logs\*.
    [string[]]$Path = @()
)

# The hands of docs/features/root-catalog/VERIFICATION.md: copy a real
# catalog into an isolated root without opening it, inspect a root catalog
# read-only (`koru-store-server root-catalog`, which never migrates), and
# grep logs and captured bytes for sentinels that must not appear. Nothing
# here starts a Koru process; the person does that with `koru.bat root`.
#
# ASCII only, like every .ps1 here: Windows PowerShell 5.1 reads a BOM-less
# file as ANSI, and a UTF-8 dash arrives as three cp1252 characters.
$ErrorActionPreference = "Stop"

$store = Join-Path $Root "store"

switch ($Action) {
    "Prepare" {
        if ($Source -eq "") { throw "Prepare needs -Source <store directory holding catalog.db>" }
        $catalog = Join-Path $Source "catalog.db"
        if (-not (Test-Path -LiteralPath $catalog)) { throw "no catalog.db under $Source" }
        foreach ($suffix in @("-wal", "-shm")) {
            $side = "$catalog$suffix"
            if ((Test-Path -LiteralPath $side) -and ((Get-Item -LiteralPath $side).Length -gt 0)) {
                throw "$side is not empty: the Store that owns $Source is running or did not shut down cleanly. Stop it (koru.bat stop-all) and retry."
            }
        }
        if (Test-Path -LiteralPath $store) {
            if ((Get-ChildItem -LiteralPath $store -Force | Measure-Object).Count -gt 0) {
                throw "$store is not empty; use a fresh isolated root for each run"
            }
        } else {
            New-Item -ItemType Directory -Path $store -Force | Out-Null
        }
        Copy-Item -LiteralPath $catalog -Destination (Join-Path $store "catalog.db")
        $size = (Get-Item -LiteralPath (Join-Path $store "catalog.db")).Length
        Write-Output "copied catalog.db ($size bytes) to $store"
        if ($WithWorkspaces) {
            $workspaces = Join-Path $Source "workspaces"
            if (Test-Path -LiteralPath $workspaces) {
                Copy-Item -LiteralPath $workspaces -Destination (Join-Path $store "workspaces") -Recurse
                $count = (Get-ChildItem -LiteralPath (Join-Path $store "workspaces") -Directory | Measure-Object).Count
                Write-Output "copied $count workspace director(ies)"
            } else {
                Write-Output "no workspaces\ under $Source; nothing copied"
            }
        }
        Write-Output "next: $PSCommandPath -Action Inspect -Root `"$Root`"  (expect 'catalog is at 12' before the first 13 build opens it)"
    }
    "Inspect" {
        if (-not (Test-Path -LiteralPath $Exe)) { throw "no $Exe; build it with: cargo build -p koru-store-server" }
        & $Exe --root $store root-catalog
        exit $LASTEXITCODE
    }
    "Grep" {
        if ($Needle.Count -eq 0) { throw "Grep needs -Needle <sentinel> [,<sentinel>...]" }
        $files = @()
        $logs = Join-Path $store "logs"
        if (Test-Path -LiteralPath $logs) {
            $files += Get-ChildItem -LiteralPath $logs -File -Recurse | ForEach-Object { $_.FullName }
        }
        foreach ($extra in $Path) {
            if (Test-Path -LiteralPath $extra) {
                $item = Get-Item -LiteralPath $extra
                if ($item.PSIsContainer) {
                    $files += Get-ChildItem -LiteralPath $extra -File -Recurse | ForEach-Object { $_.FullName }
                } else {
                    $files += $item.FullName
                }
            } else {
                Write-Output "missing: $extra"
            }
        }
        if ($files.Count -eq 0) { throw "nothing to scan under $logs or -Path" }
        $hits = 0
        foreach ($file in $files) {
            foreach ($needle in $Needle) {
                $found = Select-String -LiteralPath $file -Pattern ([regex]::Escape($needle)) -CaseSensitive
                foreach ($hit in $found) {
                    $hits++
                    Write-Output ("HIT {0}:{1} contains {2}" -f $file, $hit.LineNumber, $needle)
                }
            }
        }
        Write-Output ("scanned {0} file(s) for {1} needle(s): {2} hit(s)" -f $files.Count, $Needle.Count, $hits)
        if ($hits -gt 0) { exit 1 }
        exit 0
    }
}
