# SPDX-License-Identifier: MPL-2.0

# Place the sherpa-onnx prebuilt native archive this tree links against, and
# verify it against the hash pinned in crates/koru-voice-sherpa/src/artifacts.rs.
#
# Why this exists: sherpa-onnx-sys' build script downloads a matching prebuilt
# archive from GitHub whenever SHERPA_ONNX_LIB_DIR is unset, so an enabled build
# reaches the network and gets whatever the release page is serving that day. It
# also reads SHERPA_ONNX_ARCHIVE_DIR first and copies from there instead of
# downloading. That is the seam this script fills - the same discipline
# assets\fonts\*.ttf follows through koru.bat fonts.
#
#   scripts\setup\fetch-voice-runtime.ps1            this host's target
#   scripts\setup\fetch-voice-runtime.ps1 -All       every pinned target
#   scripts\setup\fetch-voice-runtime.ps1 -Verify    check what is already here
#
# Then, for a benchmark or a release build:
#
#   $env:SHERPA_ONNX_ARCHIVE_DIR = "$PWD\third-party\sherpa-onnx\1.13.6"
#   cargo run -p koru-voice-bench --features sherpa -- ...
#
# whisper.cpp needs no artifact: whisper-rs-sys vendors the sources inside its
# own .crate and builds them with cmake and bindgen. What it needs is CMake, a
# C++ toolchain and libclang on the machine, which this script does not install.
#
# ASCII only. Windows PowerShell 5.1 reads a BOM-less script as ANSI, and a
# UTF-8 dash here arrives as three cp1252 characters, one of which is a curly
# quote that silently opens a string and breaks the parse pages later.

[CmdletBinding()]
param(
    [switch]$All,
    [switch]$Verify
)

$ErrorActionPreference = 'Stop'

$Version = '1.13.6'
$Base = "https://github.com/k2-fsa/sherpa-onnx/releases/download/v$Version"
$Root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$Dest = Join-Path $Root "third-party\sherpa-onnx\$Version"

# The first three rows are the same three columns as
# koru_voice_sherpa::artifacts::NAMES, and a test there fails if the file names
# drift from what sherpa-onnx-sys looks for. The Linux row is the reader's
# alone until Linux voice lands (docs/features/linux/TECHNICAL.final.md, D4),
# pinned in koru_read_paddle::artifacts::READER_ARCHIVES, and a test there
# fails if this copy drifts. It is placed only with -All: this script runs on
# Windows.
#
# All four are upstream's TTS-free ("no-tts") archives: no espeak-ng, ucd or
# piper_phonemize, so no GPL-3.0 eSpeak NG in what links them
# (third-party/patches/sherpa-onnx.md; the Linux one verified 2026-11-17,
# licensing/THIRD-PARTY.md).
$Artifacts = @(
    [pscustomobject]@{
        Name = "sherpa-onnx-v$Version-win-x64-static-MT-Release-no-tts-lib.tar.bz2"
        Sha  = '6c746e0fabc52df40e185662faccb8dc03bcb1fd137394c3e9d3815e78ca9308'
        Size = 116816761
        Host = 'windows-x86_64'
    },
    [pscustomobject]@{
        Name = "sherpa-onnx-v$Version-osx-arm64-static-no-tts-lib.tar.bz2"
        Sha  = '5df455ff1c262347dde37e5608d2a80ff948f9d65984709cf351c4d06edc931b'
        Size = 18717755
        Host = 'macos-aarch64'
    },
    [pscustomobject]@{
        Name = "sherpa-onnx-v$Version-osx-x64-static-no-tts-lib.tar.bz2"
        Sha  = 'adcc1de32d99410416a98d4f880ee2e2ef4343fad8b627e58d1cd1a5bd6d16b8'
        Size = 18413779
        Host = 'macos-x86_64'
    },
    [pscustomobject]@{
        Name = "sherpa-onnx-v$Version-linux-x64-static-no-tts-lib.tar.bz2"
        Sha  = 'b6d61f8836ea247cf529ea411e12c9fc35314f0217278a3d61195672a4778e6a'
        Size = 21170001
        Host = 'linux-x86_64'
    }
)

# **Worktrees copy before they download.** A new worktree takes an archive from
# the main checkout or another worktree that has it, as fonts do, rather than
# downloading it again. Only a copy that matches the pin is taken, so a sibling
# holding a stale or locally built archive is passed over, not copied in to be
# refused.
function Find-SiblingArtifact {
    param([object]$Artifact)

    $ours = [System.IO.Path]::GetFullPath($Root)
    $porcelain = $null
    try { $porcelain = & git -C $Root worktree list --porcelain 2>$null } catch { }
    foreach ($line in @($porcelain)) {
        if ($line -notmatch '^worktree (.+)$') { continue }
        $tree = [System.IO.Path]::GetFullPath($Matches[1])
        if ($tree -eq $ours) { continue }
        $path = Join-Path $tree "third-party\sherpa-onnx\$Version\$($Artifact.Name)"
        if (-not (Test-Path -LiteralPath $path)) { continue }
        if ((Get-Item -LiteralPath $path).Length -ne $Artifact.Size) { continue }
        $have = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
        if ($have -eq $Artifact.Sha) { return $path }
    }
    return $null
}

function Place-Artifact {
    param([object]$Artifact)

    $final = Join-Path $Dest $Artifact.Name

    if ((-not (Test-Path $final)) -and (-not $Verify)) {
        $sibling = Find-SiblingArtifact -Artifact $Artifact
        if ($sibling) {
            if (-not (Test-Path $Dest)) {
                New-Item -ItemType Directory -Force -Path $Dest | Out-Null
            }
            Write-Host "[voice] copying $($Artifact.Name) from $(Split-Path -Parent $sibling)"
            $part = "$final.part"
            Copy-Item -LiteralPath $sibling -Destination $part -Force
            Move-Item -Force $part $final
        }
    }

    if (Test-Path $final) {
        $have = (Get-FileHash -Algorithm SHA256 -Path $final).Hash.ToLowerInvariant()
        if ($have -eq $Artifact.Sha) {
            Write-Host "[voice] $($Artifact.Name) is already here and verified"
            return $true
        }
        Write-Host "[voice] $($Artifact.Name) is here and does NOT match the pinned hash"
        Write-Host "[voice]   want $($Artifact.Sha)"
        Write-Host "[voice]   have $have"
        Write-Host "[voice] refusing to use it; delete it to re-fetch"
        return $false
    }

    if ($Verify) {
        Write-Host "[voice] $($Artifact.Name) is not here"
        return $false
    }

    if (-not (Test-Path $Dest)) {
        New-Item -ItemType Directory -Force -Path $Dest | Out-Null
    }

    # Verified into place, never in place: a download that is interrupted must
    # not leave a file the build script will happily copy.
    $part = "$final.part"
    if (Test-Path $part) { Remove-Item -Force $part }
    Write-Host "[voice] downloading $($Artifact.Name)"
    # Invoke-WebRequest rather than curl.exe: under $ErrorActionPreference =
    # 'Stop' a native command writing to stderr is a terminating error, and a
    # progress line on stderr would kill the script mid-download.
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest -Uri "$Base/$($Artifact.Name)" -OutFile $part -UseBasicParsing

    $size = (Get-Item $part).Length
    if ($size -ne $Artifact.Size) {
        Remove-Item -Force $part
        Write-Host "[voice] $($Artifact.Name) is $size bytes and the pin says $($Artifact.Size)"
        return $false
    }
    $have = (Get-FileHash -Algorithm SHA256 -Path $part).Hash.ToLowerInvariant()
    if ($have -ne $Artifact.Sha) {
        Remove-Item -Force $part
        Write-Host "[voice] $($Artifact.Name) does not match the pinned sha256"
        Write-Host "[voice]   want $($Artifact.Sha)"
        Write-Host "[voice]   have $have"
        return $false
    }
    Move-Item -Force $part $final
    Write-Host "[voice] placed $final"
    return $true
}

# **The ONNX Runtime static library the read worker links**, taken out of the
# archive above once that has verified, and verified itself. ort-sys links a
# static ONNX Runtime only from the directory ORT_LIB_PATH names, so the one
# library is extracted beside the archive, in the archive's own layout, and the
# launchers point ORT_LIB_PATH at its lib\. One ONNX Runtime in the tree, from
# one pinned archive. The same row as koru_read_paddle::artifacts::ONNXRUNTIME,
# and a test there fails if they drift. Apple silicon's comes from the archive
# Mac voice links (its objects are built for macOS 13.4, the Mac release's
# minimum); Intel Macs ship no build, and Windows on ARM and Linux aarch64 have
# no TTS-free archive.
$OrtLibraries = @(
    [pscustomobject]@{
        Archive = "sherpa-onnx-v$Version-win-x64-static-MT-Release-no-tts-lib"
        File    = 'onnxruntime.lib'
        Sha     = 'fc1f5722dd17cbe8c2131cad2b9da6e004392d45bbe71560ff5e54c9dee26f24'
        Size    = 916804662
        Host    = 'windows-x86_64'
    },
    [pscustomobject]@{
        Archive = "sherpa-onnx-v$Version-linux-x64-static-no-tts-lib"
        File    = 'libonnxruntime.a'
        Sha     = '9dbbd7bff1db5698d04ae6710a28e995a33d0409adc362f8ff2bb0bf7e8fc2ea'
        Size    = 109507520
        Host    = 'linux-x86_64'
    },
    [pscustomobject]@{
        Archive = "sherpa-onnx-v$Version-osx-arm64-static-no-tts-lib"
        File    = 'libonnxruntime.a'
        Sha     = '93850f85f3778a52d93f873da67d88c743cc9b43495af3e87e2b8df9b5a3998e'
        Size    = 69439552
        Host    = 'macos-aarch64'
    }
)

function Place-OrtLibrary {
    param([object]$Library)

    $libDir = Join-Path $Dest "$($Library.Archive)\lib"
    $final = Join-Path $libDir $Library.File

    if (Test-Path -LiteralPath $final) {
        $item = Get-Item -LiteralPath $final
        $have = (Get-FileHash -Algorithm SHA256 -LiteralPath $final).Hash.ToLowerInvariant()
        if (($item.Length -eq $Library.Size) -and ($have -eq $Library.Sha)) {
            Write-Host "[read] $($Library.File) is already extracted and verified"
            return $true
        }
        Write-Host "[read] $final does NOT match the pinned hash"
        Write-Host "[read]   want $($Library.Sha)"
        Write-Host "[read]   have $have"
        Write-Host "[read] refusing to use it; delete it to extract it again"
        return $false
    }
    if ($Verify) {
        Write-Host "[read] $($Library.File) is not extracted"
        return $false
    }

    # Extracted into a directory of its own beside the archive, verified, and
    # only then moved into place: an interrupted extraction must not leave a
    # library the build will happily link.
    $archive = Join-Path $Dest "$($Library.Archive).tar.bz2"
    $part = Join-Path $Dest "$($Library.Archive).part"
    if (Test-Path -LiteralPath $part) { Remove-Item -Recurse -Force -LiteralPath $part }
    New-Item -ItemType Directory -Force -Path $part | Out-Null
    Write-Host "[read] extracting $($Library.File) from $($Library.Archive).tar.bz2"
    # Windows' own bsdtar, which reads bzip2; Git's tar on PATH may not.
    & "$env:SystemRoot\System32\tar.exe" -xjf $archive -C $part "$($Library.Archive)/lib/$($Library.File)"
    if ($LASTEXITCODE -ne 0) {
        Remove-Item -Recurse -Force -LiteralPath $part
        Write-Host "[read] tar could not extract $($Library.File)"
        return $false
    }
    $extracted = Join-Path $part "$($Library.Archive)\lib\$($Library.File)"
    $size = (Get-Item -LiteralPath $extracted).Length
    $have = (Get-FileHash -Algorithm SHA256 -LiteralPath $extracted).Hash.ToLowerInvariant()
    if (($size -ne $Library.Size) -or ($have -ne $Library.Sha)) {
        Remove-Item -Recurse -Force -LiteralPath $part
        Write-Host "[read] the extracted $($Library.File) does not match its pin"
        Write-Host "[read]   want $($Library.Sha), $($Library.Size) bytes"
        Write-Host "[read]   have $have, $size bytes"
        return $false
    }
    New-Item -ItemType Directory -Force -Path $libDir | Out-Null
    Move-Item -Force -LiteralPath $extracted -Destination $final
    Remove-Item -Recurse -Force -LiteralPath $part
    Write-Host "[read] placed $final"
    return $true
}

$wanted = 'windows-x86_64'
$ok = $true
foreach ($artifact in $Artifacts) {
    if ((-not $All) -and ($artifact.Host -ne $wanted)) { continue }
    if (-not (Place-Artifact -Artifact $artifact)) { $ok = $false }
}
$ortLibDir = $null
if ($ok) {
    foreach ($library in $OrtLibraries) {
        if ((-not $All) -and ($library.Host -ne $wanted)) { continue }
        if (-not (Place-OrtLibrary -Library $library)) { $ok = $false }
        if ($library.Host -eq $wanted) { $ortLibDir = Join-Path $Dest "$($library.Archive)\lib" }
    }
}

if ($ok) {
    Write-Host ''
    Write-Host "[voice] now:  `$env:SHERPA_ONNX_ARCHIVE_DIR = `"$Dest`""
    if ($ortLibDir) {
        Write-Host "[read]  and:  `$env:ORT_LIB_PATH = `"$ortLibDir`""
    }
    exit 0
}
exit 1
