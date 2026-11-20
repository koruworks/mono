// SPDX-License-Identifier: MPL-2.0

//! **The ONNX Runtime static library the runtime links, and where it comes
//! from: the same archive the voice worker links, verified the same way.**
//!
//! There is one ONNX Runtime in this tree: 1.27.1, as a single static library
//! (`lib/onnxruntime.lib` on Windows, `lib/libonnxruntime.a` on Linux and
//! macOS) inside
//! the sherpa-onnx 1.13.6 archive `scripts/setup/fetch-voice-runtime.*`
//! already places and checks against the pin in
//! `koru_voice_sherpa::artifacts::NAMES` (or, for a target voice does not link
//! yet, [`READER_ARCHIVES`]). `ort-sys` links a static
//! ONNX Runtime from the directory `ORT_LIB_PATH` names and from nowhere else
//! (its downloading feature is off), so the fetch script, once the archive has
//! verified, also extracts that one library beside it:
//!
//! ```text
//! third-party/sherpa-onnx/1.13.6/
//!   sherpa-onnx-v1.13.6-win-x64-static-MT-Release-no-tts-lib.tar.bz2     verified against its pin
//!   sherpa-onnx-v1.13.6-win-x64-static-MT-Release-no-tts-lib/lib/
//!     onnxruntime.lib                                                     verified against ONNXRUNTIME below
//! ```
//!
//! and the launchers point `ORT_LIB_PATH` at that `lib/`. The library has its
//! own pin here because the build links it, not the archive: an official
//! release checks the bytes it linked (`koru-release`'s `native` module), as it
//! checks sherpa's.
//!
//! On Linux x86_64 and macOS on Apple silicon the layout is the same, under
//! `sherpa-onnx-v1.13.6-linux-x64-static-no-tts-lib/lib/libonnxruntime.a` and
//! `sherpa-onnx-v1.13.6-osx-arm64-static-no-tts-lib/lib/libonnxruntime.a`.
//!
//! **Windows x64, Linux x86_64 and macOS on Apple silicon, for now.** The
//! Apple silicon library was pinned on 2026-11-18. Its objects are built for
//! macOS 13.4 (`LC_BUILD_VERSION` minos), which is why the Mac release's
//! minimum is 13.4. Intel Macs are not pinned: no Intel Mac build ships.
//! Windows on ARM and Linux aarch64 have no TTS-free archive, so no
//! recognition.
//!
//! **Linux x86_64's archive is the reader's alone, for now.** Voice does not
//! link on Linux until its own rung (`docs/features/linux/TECHNICAL.final.md`,
//! D4), so `koru_voice_sherpa::artifacts::NAMES` has no Linux row, and the
//! archive the library comes from is pinned here instead, in
//! [`READER_ARCHIVES`]. Its licensing was verified on 2026-11-17: no
//! `espeak-ng`, `ucd` or `piper_phonemize`, and nothing GPL in what the reader
//! links (`licensing/THIRD-PARTY.md`). When Linux voice lands, its row moves
//! to `NAMES` and this table empties.

use std::path::{Path, PathBuf};

/// The sherpa-onnx version whose archive carries the library. The same as
/// `koru_voice_sherpa::artifacts::SHERPA_ONNX_VERSION`, which a test holds.
pub const SHERPA_ONNX_VERSION: &str = "1.13.6";

/// The ONNX Runtime version inside it, and the C API level `ort` is built
/// for (`api-27`).
pub const ONNX_RUNTIME_VERSION: &str = "1.27.1";

/// The variable `ort-sys`' build script reads: a directory holding the static
/// library. It is `rerun-if-env-changed`.
pub const ORT_LIB_PATH: &str = "ORT_LIB_PATH";

/// One pinned static library inside one pinned archive.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct OrtLibrary {
    /// The Rust target triple.
    pub target: &'static str,
    /// `CARGO_CFG_TARGET_OS`.
    pub target_os: &'static str,
    /// `CARGO_CFG_TARGET_ARCH`.
    pub target_arch: &'static str,
    /// The archive's stem as `koru_voice_sherpa::artifacts::NAMES` or
    /// [`READER_ARCHIVES`] spells it.
    pub archive_stem: &'static str,
    /// The library's file name inside the archive's `lib/`.
    pub file_name: &'static str,
    /// Its SHA-256, taken from the pinned archive (Windows 2026-11-11, Linux
    /// 2026-11-17, macOS 2026-11-18).
    pub sha256: &'static str,
    /// Its size in bytes.
    pub size: u64,
}

/// A pinned sherpa-onnx archive that only the reader takes a library from: a
/// target voice does not link yet. The same columns as
/// `koru_voice_sherpa::artifacts::NativeArtifact`, whose table a test keeps
/// this one disjoint from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ReaderArchive {
    /// The Rust target triple.
    pub target: &'static str,
    /// `CARGO_CFG_TARGET_OS`.
    pub target_os: &'static str,
    /// `CARGO_CFG_TARGET_ARCH`.
    pub target_arch: &'static str,
    /// The archive's file name, without the version.
    pub stem: &'static str,
    /// Its SHA-256, the same as the digest upstream's release lists.
    pub sha256: &'static str,
    /// Its published size in bytes.
    pub size: u64,
}

impl ReaderArchive {
    /// The exact file name, as upstream publishes it and the fetch scripts
    /// place it.
    pub fn file_name(&self) -> String {
        format!("sherpa-onnx-v{SHERPA_ONNX_VERSION}-{}.tar.bz2", self.stem)
    }

    pub fn is_pinned(&self) -> bool {
        self.sha256.len() == 64 && self.size > 0
    }
}

/// **The archives only the reader takes a library from.** The same rows live
/// in `scripts/setup/fetch-voice-runtime.ps1` and `.sh`, beside voice's.
pub const READER_ARCHIVES: [ReaderArchive; 1] = [ReaderArchive {
    target: "x86_64-unknown-linux-gnu",
    target_os: "linux",
    target_arch: "x86_64",
    stem: "linux-x64-static-no-tts-lib",
    sha256: "b6d61f8836ea247cf529ea411e12c9fc35314f0217278a3d61195672a4778e6a",
    size: 21_170_001,
}];

impl OrtLibrary {
    /// The archive's top-level directory, which is its file name without
    /// `.tar.bz2`.
    pub fn archive_dir_name(&self) -> String {
        format!("sherpa-onnx-v{SHERPA_ONNX_VERSION}-{}", self.archive_stem)
    }

    /// Where the fetch script extracts the library, under the repository
    /// `root`: the directory `ORT_LIB_PATH` names.
    pub fn lib_dir(&self, root: &Path) -> PathBuf {
        root.join("third-party")
            .join("sherpa-onnx")
            .join(SHERPA_ONNX_VERSION)
            .join(self.archive_dir_name())
            .join("lib")
    }

    /// The library's path inside the archive, as `tar` names it.
    pub fn member(&self) -> String {
        format!("{}/lib/{}", self.archive_dir_name(), self.file_name)
    }

    pub fn is_pinned(&self) -> bool {
        self.sha256.len() == 64 && self.size > 0
    }
}

/// **The pinned libraries.** The same rows live in
/// `scripts/setup/fetch-voice-runtime.ps1` and `.sh`, and nowhere else.
pub const ONNXRUNTIME: [OrtLibrary; 3] = [
    OrtLibrary {
        target: "x86_64-pc-windows-msvc",
        target_os: "windows",
        target_arch: "x86_64",
        archive_stem: "win-x64-static-MT-Release-no-tts-lib",
        file_name: "onnxruntime.lib",
        sha256: "fc1f5722dd17cbe8c2131cad2b9da6e004392d45bbe71560ff5e54c9dee26f24",
        size: 916_804_662,
    },
    OrtLibrary {
        target: "x86_64-unknown-linux-gnu",
        target_os: "linux",
        target_arch: "x86_64",
        archive_stem: "linux-x64-static-no-tts-lib",
        file_name: "libonnxruntime.a",
        sha256: "9dbbd7bff1db5698d04ae6710a28e995a33d0409adc362f8ff2bb0bf7e8fc2ea",
        size: 109_507_520,
    },
    OrtLibrary {
        target: "aarch64-apple-darwin",
        target_os: "macos",
        target_arch: "aarch64",
        archive_stem: "osx-arm64-static-no-tts-lib",
        file_name: "libonnxruntime.a",
        sha256: "93850f85f3778a52d93f873da67d88c743cc9b43495af3e87e2b8df9b5a3998e",
        size: 69_439_552,
    },
];

/// The library for a `(target_os, target_arch)` pair, or `None` where none is
/// pinned.
pub fn library_for(target_os: &str, target_arch: &str) -> Option<&'static OrtLibrary> {
    ONNXRUNTIME
        .iter()
        .find(|library| library.target_os == target_os && library.target_arch == target_arch)
}

/// The library for the target this code is compiled for. Answered from `cfg!`,
/// because the question is about the build.
pub fn library_for_this_build() -> Option<&'static OrtLibrary> {
    let os = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        return None;
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return None;
    };
    library_for(os, arch)
}

#[cfg(test)]
mod tests;
