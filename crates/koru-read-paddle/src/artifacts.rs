// SPDX-License-Identifier: MPL-2.0

//! **The ONNX Runtime static library the runtime links, and where it comes
//! from: the same archive the voice worker links, verified the same way.**
//!
//! There is one ONNX Runtime in this tree: 1.27.1, as a single static library
//! (`lib/onnxruntime.lib` on Windows) inside the sherpa-onnx 1.13.6 archive
//! `scripts/setup/fetch-voice-runtime.*` already places and checks against the
//! pin in `koru_voice_sherpa::artifacts::NAMES`. `ort-sys` links a static
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
//! **Windows x64 only, for now.** The macOS archives hold the same ONNX
//! Runtime; their library is pinned when a Mac build first links it (the mac
//! branch's catch-up). Linux and Windows on ARM have no archive, so no
//! recognition.

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
    /// The archive's stem as `koru_voice_sherpa::artifacts::NAMES` spells it.
    pub archive_stem: &'static str,
    /// The library's file name inside the archive's `lib/`.
    pub file_name: &'static str,
    /// Its SHA-256, taken from the pinned archive on 2026-11-11.
    pub sha256: &'static str,
    /// Its size in bytes.
    pub size: u64,
}

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
pub const ONNXRUNTIME: [OrtLibrary; 1] = [OrtLibrary {
    target: "x86_64-pc-windows-msvc",
    target_os: "windows",
    target_arch: "x86_64",
    archive_stem: "win-x64-static-MT-Release-no-tts-lib",
    file_name: "onnxruntime.lib",
    sha256: "fc1f5722dd17cbe8c2131cad2b9da6e004392d45bbe71560ff5e54c9dee26f24",
    size: 916_804_662,
}];

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
