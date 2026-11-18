// SPDX-License-Identifier: MPL-2.0

//! **Where voice's model bytes live, and the one rule that decides it.**
//!
//! ```text
//! KORU_VOICE_MODELS       <that path>
//! --root given            <root>/voice/models/<id>/<version>/<digest>/
//! otherwise               %LOCALAPPDATA%\Koru\models\<id>\<version>\<digest>\
//! ```
//!
//! The rule is voice's and stays here: which variable overrides it, which
//! subdirectory a `--root` gets. Everything *inside* the cache root — the
//! layout, the four dotted directories, the channel-independent
//! `<local data>` base — is `koru-models`' [`koru_models::ModelPaths`], which
//! this type wraps so that voice's callers can keep naming a model by voice's
//! own `ModelId` and descriptor.
//!
//! **Channel-independent, which is the one deliberate divergence from
//! `UpdatePaths::updates_dir()`.** An update package is per-channel state; a
//! model is immutable content addressed by its digest, and five channels must
//! not each hold 650 MB of identical bytes. `KORU_VOICE_MODELS` exists so that
//! a `--root` test reuses one download rather than paying for isolation in
//! gigabytes — which is why it deliberately points *outside* the profile root
//! it overrides, and why a test asserts that every path this type answers with
//! still lands under whichever root won.

use crate::descriptor::{ModelDescriptor, ModelVersion, Sha256Digest};
use crate::error::{ModelError, ModelErrorCode};
use koru_voice::ModelId;
use std::path::{Path, PathBuf};

// The suite beside this module predates the extraction and names the four
// internal directories through `super::*`.
#[cfg(test)]
use koru_models::paths::{LEASES, LOCKS, PARTS, STAGING};

/// Where voice's model cache is, and every path inside it.
///
/// A resolved value rather than a function, so that a store which was pointed
/// somewhere cannot later ask the world again and get a different answer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ModelPaths {
    inner: koru_models::ModelPaths,
}

impl ModelPaths {
    /// **The whole rule**, and it reads the world: `KORU_VOICE_MODELS` first,
    /// then a `--root` if one was given, then the channel-independent default.
    ///
    /// Fails only when there is no default to fall back to — a machine with
    /// neither `%LOCALAPPDATA%` nor `$HOME` — because a store that guessed a
    /// directory is the failure `guide/contributing/PRINCIPLES.md` names by name.
    pub fn resolve(root: Option<&Path>) -> Result<ModelPaths, ModelError> {
        ModelPaths::decide(
            koru_env::value(koru_env::name::KORU_VOICE_MODELS).map(PathBuf::from),
            root,
            koru_models::paths::local_data_base(),
        )
    }

    /// The rule with the world passed in, so the three cases are testable
    /// without `set_var` — which is a process-global write and races every
    /// other test in the binary.
    pub fn decide(
        overridden: Option<PathBuf>,
        root: Option<&Path>,
        local_data: Option<PathBuf>,
    ) -> Result<ModelPaths, ModelError> {
        if let Some(path) = overridden {
            return Ok(ModelPaths::at(path));
        }
        if let Some(root) = root {
            return Ok(ModelPaths::at(root.join("voice").join("models")));
        }
        let base = local_data.ok_or_else(|| {
            ModelErrorCode::Io.with(
                "no local data directory: set KORU_VOICE_MODELS or pass --root, \
                 because a model store may not guess where to write",
            )
        })?;
        Ok(ModelPaths::at(base.join("Koru").join("models")))
    }

    /// A cache root somebody named exactly — a test's temporary directory, or
    /// the value `KORU_VOICE_MODELS` carried. No `voice/models` is appended:
    /// this is the directory itself.
    pub fn at(root: impl Into<PathBuf>) -> ModelPaths {
        ModelPaths {
            inner: koru_models::ModelPaths::at(root),
        }
    }

    /// The generic paths this wraps — what a [`crate::DiskModelStore`] hands
    /// the store it is built on.
    pub fn generic(&self) -> &koru_models::ModelPaths {
        &self.inner
    }

    /// The cache root. Everything else this type answers is under it, and
    /// `every_resolved_path_lands_under_the_cache_root` is what says so.
    pub fn root(&self) -> &Path {
        self.inner.root()
    }

    /// `<cache>/<id>` — the generic layout, named by voice's `ModelId`. The
    /// two id types share one charset rule (`descriptor/projection_tests.rs`
    /// pins that), so the string is the same path component either way.
    pub fn id_dir(&self, id: &ModelId) -> PathBuf {
        self.inner.root().join(id.as_str())
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
    /// before it removes it.
    pub fn staging_dir(&self) -> PathBuf {
        self.inner.staging_dir()
    }

    /// Where a `.part` survives a restart, keyed by digest.
    pub fn parts_dir(&self) -> PathBuf {
        self.inner.parts_dir()
    }

    /// The archive for one digest, once it is complete.
    pub fn archive_path(&self, digest: &Sha256Digest) -> PathBuf {
        self.inner.archive_path(digest)
    }

    /// The per-digest install lock's file.
    pub fn install_lock(&self, digest: &Sha256Digest) -> PathBuf {
        self.inner.install_lock(digest)
    }

    /// The directory holding one digest's lease files, one per live holder.
    pub fn leases_dir(&self, digest: &Sha256Digest) -> PathBuf {
        self.inner.leases_dir(digest)
    }

    /// Whether a directory entry's name is one of the store's own rather than a
    /// model id.
    pub fn is_internal(name: &str) -> bool {
        koru_models::ModelPaths::is_internal(name)
    }
}

/// Voice's layout is the generic layout: the two `model_dir`s agree.
#[cfg(test)]
mod layout_tests;
#[cfg(test)]
mod tests;
