// SPDX-License-Identifier: MPL-2.0

//! **The native archive this crate links against, per target — named here so a
//! provisioning script and a build script cannot disagree about it.**
//!
//! `sherpa-onnx-sys`' build script resolves its library directory in three
//! steps, and the order is the whole of the offline story:
//!
//! 1. `SHERPA_ONNX_LIB_DIR` — an already-extracted `lib/` directory. Used as-is.
//! 2. otherwise a cache under the target directory, `sherpa-onnx-prebuilt/`,
//!    which it fills from `SHERPA_ONNX_ARCHIVE_DIR` if that names a directory
//!    **containing the exactly-named archive**,
//! 3. and otherwise by downloading that archive from GitHub releases.
//!
//! Step 3 is the trap Plan 0.3 exists to close, and step 2 is how it is closed
//! without patching anybody's build script:
//! `docs/features/voice/RUNTIME_ARTIFACTS.md`'s script places the verified archives
//! in `third-party/sherpa-onnx/<version>/` and the enabled build exports
//! `SHERPA_ONNX_ARCHIVE_DIR` at it. **The file name has to match exactly**, or
//! the build script decides the local directory is missing the archive and
//! fails — which is the failure we want, rather than a silent download.
//!
//! So the names are declared twice, deliberately: once inside a dependency we
//! do not control, and once here where a test can read them. `NAMES` is the
//! table `fetch-voice-runtime` reads and
//! [`the_archive_names_follow_sherpa_onnx_syss_own_rule`] is what fails when a
//! version bump changes the shape of one.
//!
//! [`the_archive_names_follow_sherpa_onnx_syss_own_rule`]: ../tests/index.html

/// The pinned sherpa-onnx version. **Exact in `Cargo.toml`, not a caret range**,
/// because the archive the build script fetches is named from
/// `CARGO_PKG_VERSION`: a minor bump would look for a differently named file,
/// and a hash pinned against the old name would then be pinning nothing.
pub const SHERPA_ONNX_VERSION: &str = "1.13.6";

/// Where the published archives live. The provisioning script's base URL, and
/// the same one `sherpa-onnx-sys` uses.
pub const RELEASE_BASE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download";

/// **The chunk size is a property of the export, not a runtime knob.**
///
/// Kept as a constant so it is greppable from the place somebody would look for
/// a setter. NVIDIA's model card lists five cache-aware configurations — 80,
/// 160, 320, 560 and 1120 ms — and sherpa publishes one archive for each, all
/// 475.3 MB, produced by
/// `scripts/nemo/nemotron-3.5-asr-streaming-0.6b/export_onnx.py`, which computes
/// `chunk = ms / 80 − 1` and calls `set_default_att_context_size([56, chunk])`
/// **before** writing the graph.
///
/// Two numbering conventions meet here and neither is wrong: the exporter's
/// second element is 1 for the 160 ms package and 6 for the 560 ms one, while
/// NVIDIA's card counts the same configurations as chunk 2 and chunk 7. The
/// benchmark protocol's `[56, 6]` for 560 ms is the exporter's spelling.
///
/// The consequence for Plan 0.3 and 0.4: **candidates 1 and 2 are two downloads
/// and two provisioned model directories.** A harness that took a chunk size as
/// an argument would print a 560 ms result in a row labelled 160 ms and nothing
/// would fail.
pub const CHUNK_IS_AN_EXPORT: &str =
    "att_context_size is fixed at ONNX export; one archive per chunk size";

/// One prebuilt native archive, for one build target.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct NativeArtifact {
    /// The Rust target triple this archive is for.
    pub target: &'static str,
    /// `CARGO_CFG_TARGET_OS` as the build script reads it.
    pub target_os: &'static str,
    /// `CARGO_CFG_TARGET_ARCH` as the build script reads it.
    pub target_arch: &'static str,
    /// The archive's file name, without the version. See [`NativeArtifact::file_name`].
    pub stem: &'static str,
    /// The SHA-256 the provisioning script verifies the download against.
    ///
    /// Taken by downloading each published asset once and hashing it — the
    /// upstream project publishes no checksum file, so **this table is the
    /// checksum file**, and that is worth saying plainly: it pins *what we
    /// measured against*, not what upstream attests to. A re-tagged release
    /// under the same version becomes a hash mismatch here, which is the
    /// failure mode worth having.
    ///
    /// **Empty means unpinned**, and an unpinned artifact is a hole in exactly
    /// the guarantee this table exists for; the provisioning script refuses to
    /// place one rather than placing it unverified.
    pub sha256: &'static str,
    /// The published size in bytes. A cheap pre-check that costs no hashing and
    /// catches the common failure — an HTML error page saved under the
    /// archive's name — before a 120 MB read.
    pub size: u64,
}

impl NativeArtifact {
    /// The exact file name `sherpa-onnx-sys` will look for in
    /// `SHERPA_ONNX_ARCHIVE_DIR`.
    pub fn file_name(&self) -> String {
        format!("sherpa-onnx-v{SHERPA_ONNX_VERSION}-{}.tar.bz2", self.stem)
    }

    /// Where the provisioning script downloads it from: upstream's release.
    pub fn url(&self) -> String {
        format!(
            "{RELEASE_BASE_URL}/v{SHERPA_ONNX_VERSION}/{}",
            self.file_name()
        )
    }

    /// Whether the hash above is a hash.
    pub fn is_pinned(&self) -> bool {
        self.sha256.len() == 64
    }
}

/// **The three targets that link the voice runtime**, and nothing else.
///
/// Windows on ARM is out by construction rather than by choice: no release
/// builds it. `macos-x86_64` is pinned so a build from source on an Intel Mac
/// has its archive, though official macOS releases are Apple silicon only.
/// Linux has been a supported target since 0.1.4, but it ships without voice
/// input, so it has no archive in this table yet.
///
/// **Upstream's TTS-free archives, every one.** Upstream's ordinary static
/// archive includes sherpa-onnx's text-to-speech, which links eSpeak NG and ucd
/// (GPL-3.0) into every binary that links the runtime; Koru uses speech
/// recognition only. Upstream also publishes each archive built with
/// `SHERPA_ONNX_ENABLE_TTS=OFF`, named `…-no-tts-lib`, which has no espeak-ng,
/// ucd or piper_phonemize library at all, and the patched `sherpa-onnx-sys`
/// (`third-party/patches/sherpa-onnx.md`) names only those and links none of
/// the three. ADR-460 records the switch.
///
/// **Static, not shared.** `sherpa-onnx`'s default feature is `static`, which
/// links ten archives including `onnxruntime` directly into our binary. The
/// shared alternative would put a `sherpa-onnx-c-api` DLL and an `onnxruntime`
/// DLL beside every executable in the release zip and into `release-files.json`
/// — a packaging change for a benchmark, which is a trade Plan 0.3 has no reason
/// to make. The `-lib` archives carry the libraries and headers only; the
/// several-hundred-megabyte ones also carry example binaries nothing here runs.
pub const NAMES: [NativeArtifact; 3] = [
    NativeArtifact {
        target: "x86_64-pc-windows-msvc",
        target_os: "windows",
        target_arch: "x86_64",
        stem: "win-x64-static-MT-Release-no-tts-lib",
        sha256: "6c746e0fabc52df40e185662faccb8dc03bcb1fd137394c3e9d3815e78ca9308",
        size: 116_816_761,
    },
    NativeArtifact {
        target: "aarch64-apple-darwin",
        target_os: "macos",
        target_arch: "aarch64",
        stem: "osx-arm64-static-no-tts-lib",
        sha256: "5df455ff1c262347dde37e5608d2a80ff948f9d65984709cf351c4d06edc931b",
        size: 18_717_755,
    },
    NativeArtifact {
        target: "x86_64-apple-darwin",
        target_os: "macos",
        target_arch: "x86_64",
        stem: "osx-x64-static-no-tts-lib",
        sha256: "adcc1de32d99410416a98d4f880ee2e2ef4343fad8b627e58d1cd1a5bd6d16b8",
        size: 18_413_779,
    },
];

/// The artifact for a `(target_os, target_arch)` pair, spelled the way a build
/// script sees them. `None` for a target this product does not ship.
pub fn artifact_for(target_os: &str, target_arch: &str) -> Option<&'static NativeArtifact> {
    NAMES
        .iter()
        .find(|a| a.target_os == target_os && a.target_arch == target_arch)
}

/// The artifact for the host this code is compiled for, if it is one we ship.
///
/// Answered from `cfg!` rather than from a runtime lookup, because the question
/// is about the build and a cross-compiled binary asking at run time would
/// answer for the wrong machine.
pub fn artifact_for_this_build() -> Option<&'static NativeArtifact> {
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
    artifact_for(os, arch)
}

/// The two environment variables `sherpa-onnx-sys` reads, named here so the
/// provisioning documentation and the build cannot drift apart. Both are
/// `rerun-if-env-changed`, so changing either re-runs the build script.
pub mod env {
    /// An already-extracted `lib/` directory. Highest precedence; when it is
    /// set nothing is downloaded and nothing is extracted.
    pub const LIB_DIR: &str = "SHERPA_ONNX_LIB_DIR";
    /// A directory holding the exactly-named `.tar.bz2`. **This is the one the
    /// provisioning script sets**: the build script copies the archive into its
    /// own cache and extracts it, so no network is reached and no extractor of
    /// ours is involved.
    pub const ARCHIVE_DIR: &str = "SHERPA_ONNX_ARCHIVE_DIR";
}

#[cfg(test)]
mod tests;
