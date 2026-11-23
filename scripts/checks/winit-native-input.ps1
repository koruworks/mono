# SPDX-License-Identifier: MPL-2.0
# Run the patched dependency's actual colocated unit tests on Windows, or
# compile its tests for an explicitly installed desktop cross target. Cargo
# cannot test a nonmember dependency through the workspace, and upstream's
# standalone manifest resolves unrelated examples and optional desktop stacks.
# This ignored harness retains the relevant upstream runtime dependency tables and
# points directly at the fetched sources; neither they nor the production lock change.
[CmdletBinding()]
param([ValidateSet('x86_64-unknown-linux-gnu', 'aarch64-apple-darwin')][string]$CheckTarget)
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'This test harness selects Windows dependencies.' }
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$source = Join-Path $repo 'third-party/crates/winit'
$harness = Join-Path $repo 'target/winit-native-input-tests'
New-Item -ItemType Directory -Force -Path $harness | Out-Null
$tables = [regex]::Split([IO.File]::ReadAllText((Join-Path $source 'Cargo.toml')), '(?m)(?=^\[)')
if ($CheckTarget) {
    $selected = $tables | Where-Object {
        $_ -notmatch '^\[\[example\]\]' -and $_ -notmatch '^\[\[test\]\]' -and
        $_ -notmatch '^\[[^\r\n]*dev-dependencies'
    }
    # Koru uses the default ab_glyph title font, never crossfont. Avoid resolving
    # an unrelated optional native font stack just to compile the unit tests.
    $manifest = [regex]::Replace(($selected -join ''), '(?ms)^wayland-csd-adwaita-crossfont = \[.*?^\]\r?\n', '')
} else {
    $selected = $tables | Where-Object {
        $_ -match '^\[package\]' -or $_ -match '^\[lib\]' -or
        $_ -match '^\[dependencies\.' -or $_ -match '^\[build-dependencies\.' -or
        ($_ -match '^\[target\.[^\r\n]*target_os = "windows"[^\r\n]*dependencies\.')
    }
    $manifest = ($selected -join '') + @'

[features]
default = ["rwh_06"]
rwh_06 = ["dep:rwh_06"]
rwh_05 = ["dep:rwh_05"]
rwh_04 = ["dep:rwh_04"]
serde = ["dep:serde", "cursor-icon/serde", "smol_str/serde", "dpi/serde"]
'@
}
$manifest = $manifest.Replace('build = "build.rs"', ('build = "' + $source.Replace('\', '/') + '/build.rs"')).Replace('path = "src/lib.rs"', ('path = "' + $source.Replace('\', '/') + '/src/lib.rs"'))
$manifest += [Environment]::NewLine + '[workspace]' + [Environment]::NewLine + 'resolver = "3"' + [Environment]::NewLine
[IO.File]::WriteAllText((Join-Path $harness 'Cargo.toml'), $manifest, [Text.UTF8Encoding]::new($false))
Copy-Item -LiteralPath (Join-Path $repo 'Cargo.lock') -Destination (Join-Path $harness 'Cargo.lock')
$previousJobs = $env:CARGO_BUILD_JOBS
try {
    $env:CARGO_BUILD_JOBS = '2'
    if ($CheckTarget) {
        & cargo check --manifest-path (Join-Path $harness 'Cargo.toml') --target-dir (Join-Path $repo 'target') --tests --offline --target $CheckTarget
    } else {
        & cargo test --manifest-path (Join-Path $harness 'Cargo.toml') --target-dir (Join-Path $repo 'target') --lib --offline
    }
    if ($LASTEXITCODE -ne 0) { throw "winit unit tests failed ($LASTEXITCODE)" }
} finally {
    $env:CARGO_BUILD_JOBS = $previousJobs
}
