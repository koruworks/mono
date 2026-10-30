# SPDX-License-Identifier: MPL-2.0

# Stop the debug images built by this checkout as one supervised set, including
# an optional standalone AI chat launched by run-all or the workspace caption.
# Keeping the array construction in PowerShell avoids cmd.exe flattening an
# array-valued `-File` parameter into positional arguments.
#
# -SharedChannel widens the set to every *debug* checkout's stack on this
# machine: a `koru-*.exe` under any `target\debug`. Debug builds without
# KORU_ROOT all rendezvous on the one `internal-debug` channel, and the channel
# admits one Agent, so a sibling worktree's running Agent answers "already
# running for this channel" to ours and `run-all` never becomes ready. An
# installed Koru is on another channel and is not touched; `koru.bat stop` is
# still the machine-wide hatch.

param(
    [switch] $SharedChannel
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$bin = Join-Path $repo "target\debug"
$images = @(
    (Join-Path $bin "koru.exe"),
    (Join-Path $bin "koru-ai-chat.exe"),
    (Join-Path $bin "koru-agent.exe"),
    (Join-Path $bin "koru-com-notification.exe"),
    # Retired: both are worker roles of koru-agent.exe now and this checkout
    # builds neither. The paths stay because an earlier build of *this* worktree
    # may have left one running, and a stale gateway holding the root is exactly
    # the failure this script exists to clear. A path with no file is a no-op.
    (Join-Path $bin "koru-api-gateway.exe"),
    (Join-Path $bin "koru-session-host.exe"),
    (Join-Path $bin "koru-mcp.exe"),
    (Join-Path $bin "koru-voice-worker.exe"),
    (Join-Path $bin "koru-store-server.exe")
)

if ($SharedChannel) {
    $names = @($images | ForEach-Object { [System.IO.Path]::GetFileNameWithoutExtension($_) })
    $ours = [System.IO.Path]::GetFullPath($bin).TrimEnd('\')
    $foreign = @(
        Get-Process -Name $names -ErrorAction SilentlyContinue | ForEach-Object {
            try {
                $path = [System.IO.Path]::GetFullPath($_.Path)
                $dir = (Split-Path -Parent $path).TrimEnd('\')
                if ($path -match '(?i)\\target\\debug\\koru[^\\]*\.exe$' -and
                    -not [string]::Equals($dir, $ours, [System.StringComparison]::OrdinalIgnoreCase)) {
                    $path
                }
            } catch {
                # A process that withholds its path cannot be proved to be a
                # debug build on this channel, so it is left alone.
            }
        } | Select-Object -Unique
    )
    foreach ($dir in @($foreign | ForEach-Object { Split-Path -Parent $_ } | Select-Object -Unique)) {
        Write-Host "[Koru] another checkout's debug stack holds the shared channel; stopping $dir"
    }
    $images += $foreign
}

& (Join-Path $PSScriptRoot "stop-exact-exe.ps1") -ExecutablePath $images
exit 0
