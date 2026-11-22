// SPDX-License-Identifier: MPL-2.0

//! **Where the bytes live inside a cache root somebody else chose.**
//!
//! ```text
//! <root>/<id>/<version>/<digest>/     a published model
//! <root>/.staging/                    assembled here, renamed into place
//! <root>/.parts/<digest>.archive      a download, resumable across restarts
//! <root>/.locks/<digest>.install      one install per set of bytes
//! <root>/.leases/<digest>/            one file per live holder
//! ```
//!
//! **The root is the caller's.** Which directory a feature's models live in —
//! voice reads `KORU_VOICE_MODELS`, then `--root`, then a channel-independent
//! default — is a decision about that feature's variables and that feature's
//! disk budget, and a crate that read them would know which feature it was.
//! [`ModelPaths::at`] takes the answer; [`local_data_base`] is the one piece of
//! the default every feature shares.
//!
//! ## Four directories nothing installs into
//!
//! `.staging`, `.parts`, `.locks` and `.leases` sit beside the model ids. They
//! begin with a dot, and [`ModelId`] refuses a leading dot — so an id can never
//! collide with one of them, and the walk in
//! [`crate::DiskModelStore`] skips every dotted entry with one rule rather than
//! four names.

use crate::descriptor::{ModelDescriptor, ModelVersion, Sha256Digest};
use crate::token::ModelId;
use std::path::{Path, PathBuf};

/// Where a model cache is, and every path inside it.
///
/// A resolved value rather than a function, so that a store which was pointed
/// somewhere cannot later ask the world again and get a different answer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ModelPaths {
    root: PathBuf,
}

/// The staging directory's name, and the three beside it. Dotted, so no
/// [`ModelId`] can name one.
pub const STAGING: &str = ".staging";
pub const PARTS: &str = ".parts";
pub const LOCKS: &str = ".locks";
pub const LEASES: &str = ".leases";

impl ModelPaths {
    /// A cache root somebody named exactly. Nothing is appended: this is the
    /// directory itself.
    pub fn at(root: impl Into<PathBuf>) -> ModelPaths {
        ModelPaths { root: root.into() }
    }

    /// The cache root. Everything else this type answers is under it.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn id_dir(&self, id: &ModelId) -> PathBuf {
        self.root.join(id.as_str())
    }

    pub fn version_dir(&self, id: &ModelId, version: &ModelVersion) -> PathBuf {
        self.id_dir(id).join(version.as_str())
    }

    /// `<cache>/<id>/<version>/<digest>/` — where a published model lives.
    pub fn model_dir(&self, descriptor: &ModelDescriptor) -> PathBuf {
        self.version_dir(&descriptor.id, &descriptor.version)
            .join(descriptor.digest.as_str())
    }

    /// Where a download is assembled and where a delete moves a directory
    /// before it removes it — so a concurrent reader never meets a model that
    /// is half written or half gone.
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join(STAGING)
    }

    /// Where a `.part` survives a restart. **Keyed by digest**, not by URL or
    /// by process id: the same bytes resumed from a different mirror are still
    /// the same bytes.
    pub fn parts_dir(&self) -> PathBuf {
        self.root.join(PARTS)
    }

    /// The archive for one digest, once it is complete.
    pub fn archive_path(&self, digest: &Sha256Digest) -> PathBuf {
        self.parts_dir().join(format!("{digest}.archive"))
    }

    /// The per-digest install lock's file. One lock per set of bytes rather
    /// than one for the cache, because two people installing two different
    /// models have nothing to say to each other.
    pub fn install_lock(&self, digest: &Sha256Digest) -> PathBuf {
        self.root.join(LOCKS).join(format!("{digest}.install"))
    }

    /// The directory holding one digest's lease files, one per live holder.
    pub fn leases_dir(&self, digest: &Sha256Digest) -> PathBuf {
        self.root.join(LEASES).join(digest.as_str())
    }

    /// Whether a directory entry's name is one of ours rather than a model id.
    /// One rule, not four names — see the module note.
    pub fn is_internal(name: &str) -> bool {
        name.starts_with('.')
    }
}

/// `%LOCALAPPDATA%` on Windows, `~/Library/Application Support` on macOS,
/// `$XDG_DATA_HOME` (or `~/.local/share`) elsewhere — or `None` on a machine
/// with none of them, where a store must refuse rather than guess.
///
/// **Channel-independent**, which is the point: a model is immutable content
/// addressed by its digest, and five channels must not each hold hundreds of
/// megabytes of identical bytes. `koru_channel` knows this rule too and cannot
/// be borrowed — every answer it gives carries a channel segment, and this
/// crate must never know which channel it is in. The duplicated body is
/// recorded as a drift hazard in `docs/features/voice/archive/QUESTIONS.md`
/// Q-V26.
pub fn local_data_base() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        })
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".local").join("share"))
            })
    }
}

#[cfg(test)]
mod tests;
