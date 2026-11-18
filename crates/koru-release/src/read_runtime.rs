// SPDX-License-Identifier: MPL-2.0

//! **The read worker's recognition in a release: built where its ONNX
//! Runtime library is pinned, linked from the verified library, and checked
//! afterwards as sherpa's archive is.**
//!
//! `koru-read-worker` recognizes only with its `recognition` feature, which
//! links ONNX Runtime 1.27.1 statically from the directory `ORT_LIB_PATH`
//! names. That library is one file inside voice's pinned sherpa-onnx archive,
//! extracted and verified by `scripts/setup/fetch-voice-runtime.*`
//! (`koru_read_paddle::artifacts`). So, for a host with a pinned library
//! (Windows x64 today):
//!
//! - the package build turns the feature on and, unless the environment
//!   already names one, points `ORT_LIB_PATH` at the fetched library
//!   ([`features`], [`lib_dir`]);
//! - the notices walk the same feature, so every crate recognition brings is
//!   attributed (`notices.rs`);
//! - after the build, [`check_this_build`] hashes the library that was linked:
//!   an official release refuses any bytes but the pinned ones, a local one
//!   says what it linked — the rule `native.rs` applies to sherpa's archive.
//!
//! A host with no pinned library (macOS until a Mac build pins it, Linux)
//! builds the worker without recognition: text layers only, and the Store is
//! told `recognition: false`.

use std::path::{Path, PathBuf};

use koru_read_paddle::artifacts::{self, OrtLibrary};

use crate::native::Rule;

/// The package whose feature this is.
pub const PACKAGE: &str = "koru-read-worker";

/// The feature, spelled for a workspace-wide `cargo build` or `cargo metadata`.
pub const FEATURE: &str = "koru-read-worker/recognition";

/// The pinned library for a target triple.
pub fn library_for_triple(triple: &str) -> Option<&'static OrtLibrary> {
    artifacts::ONNXRUNTIME
        .iter()
        .find(|library| library.target == triple)
}

/// The feature to build `packages` for `triples` with: recognition when the
/// read worker is among them and one of the triples has a pinned library.
pub fn features(packages: &[&str], triples: &[String]) -> Option<&'static str> {
    (packages.contains(&PACKAGE)
        && triples
            .iter()
            .any(|triple| library_for_triple(triple).is_some()))
    .then_some(FEATURE)
}

/// [`features`] for a build of `packages` for this host, which is the target
/// `koru-release` always builds.
pub fn features_for_this_build(packages: &[&str]) -> Option<&'static str> {
    let library = artifacts::library_for_this_build()?;
    features(packages, &[library.target.to_string()])
}

/// Where this host's build links ONNX Runtime from: `ORT_LIB_PATH` when the
/// environment sets it, otherwise the fetch script's directory. `None` for a
/// host with no pinned library.
pub fn lib_dir(root: &Path) -> Option<PathBuf> {
    let library = artifacts::library_for_this_build()?;
    Some(
        std::env::var_os(artifacts::ORT_LIB_PATH)
            .map(PathBuf::from)
            .unwrap_or_else(|| library.lib_dir(root)),
    )
}

/// What one release build linked.
#[derive(Debug, PartialEq, Eq)]
pub enum Linked {
    Canonical,
    NotCanonical {
        path: PathBuf,
        sha256: String,
        size: u64,
    },
    Missing(PathBuf),
}

/// What the library in `dir` is, against `library`'s pin.
pub fn linked(library: &OrtLibrary, dir: &Path) -> Result<Linked, String> {
    let path = dir.join(library.file_name);
    if !path.is_file() {
        return Ok(Linked::Missing(path));
    }
    let (size, sha256) =
        koru_update::sha256_file(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    if library.is_pinned() && size == library.size && sha256 == library.sha256 {
        Ok(Linked::Canonical)
    } else {
        Ok(Linked::NotCanonical { path, sha256, size })
    }
}

/// The official-release rule: the pinned library, or a refusal that says what
/// to do.
pub fn require_canonical(library: &OrtLibrary, linked: &Linked) -> Result<(), String> {
    match linked {
        Linked::Canonical => Ok(()),
        Linked::NotCanonical { path, sha256, size } => Err(format!(
            "an official release must link the pinned ONNX Runtime {} {}, and this build linked \
             {} (sha256 {sha256}, {size} bytes; the pin is {}, {} bytes). Unset {} or delete the \
             file, run scripts/setup/fetch-voice-runtime to extract the pinned one, and publish again",
            artifacts::ONNX_RUNTIME_VERSION,
            library.file_name,
            path.display(),
            library.sha256,
            library.size,
            artifacts::ORT_LIB_PATH,
        )),
        Linked::Missing(path) => Err(format!(
            "an official release cannot verify which ONNX Runtime the read worker linked: {} is \
             not there. Run scripts/setup/fetch-voice-runtime, which extracts it from the pinned \
             sherpa-onnx archive, and publish again",
            path.display()
        )),
    }
}

/// One line for a local build that linked something else.
pub fn notice(library: &OrtLibrary, linked: &Linked) -> Option<String> {
    match linked {
        Linked::Canonical => None,
        Linked::NotCanonical { sha256, .. } => Some(format!(
            "this bundle's read worker links a non-canonical {} (sha256 {sha256}); dist-local \
             allows it, and dist-cloud would refuse it",
            library.file_name
        )),
        Linked::Missing(path) => Some(format!(
            "could not tell which ONNX Runtime the read worker links ({} is not there); \
             dist-cloud would refuse it",
            path.display()
        )),
    }
}

/// Check what the host's release build linked. Nothing to check on a host
/// with no pinned library: its worker was built without recognition.
pub fn check_this_build(root: &Path, rule: Rule) -> Result<(), String> {
    let (Some(library), Some(dir)) = (artifacts::library_for_this_build(), lib_dir(root)) else {
        return Ok(());
    };
    let linked = linked(library, &dir)?;
    match rule {
        Rule::Official => {
            require_canonical(library, &linked)?;
            println!(
                "[Koru] ONNX Runtime (read worker): linked the pinned {} (sha256 {})",
                library.file_name, library.sha256
            );
        }
        Rule::Local => {
            if let Some(line) = notice(library, &linked) {
                println!("[Koru] note: {line}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
