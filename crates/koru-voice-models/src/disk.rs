// SPDX-License-Identifier: MPL-2.0

//! **The filesystem [`ModelStore`] for speech models.**
//!
//! Every mechanism here is `koru-models`' [`koru_models::DiskModelStore`] —
//! atomic publish from `.staging/`, the per-digest cross-process install lock,
//! a lease per live holder, crash recovery with no clock, `.koru-model.json`
//! as the record of what was verified — and its module note is where each one
//! is argued. This type is voice's view of it, and adds exactly three things:
//!
//! - **voice's cache root**, from [`ModelPaths::resolve`] —
//!   `KORU_VOICE_MODELS`, then `--root`, then the channel-independent default;
//! - **voice's consent record**: the store underneath is built with
//!   [`koru_voice_consent::VOICE`], so only a grant recorded in
//!   `voice-consent.conf` opens it;
//! - **voice's types**: every descriptor going in is written as tokens, and
//!   every one coming out is read back as `EngineId`, `ModelTier` and
//!   `VoiceLanguage`. A directory whose descriptor speech has no word for is
//!   skipped, as an unreadable one always was — never an error that would
//!   hide the models beside it.
//!
//! The cache on disk — `<id>/<version>/<digest>/`, the dotted directories, the
//! metadata file and its shape — is byte for byte what it was.

use crate::catalog::CatalogEntry;
use crate::descriptor::ModelVersion;
use crate::error::{ModelError, ModelErrorCode};
use crate::paths::ModelPaths;
use crate::source::{ModelSource, Typed};
use crate::store::{InstallProgress, InstalledModel, ModelLease, ModelStore};
use crate::trust::{TrustPolicy, TrustedKeys};
use koru_voice::ModelId;
use std::path::Path;
use std::sync::atomic::AtomicBool;
// The generic store's verbs, which the wrapped store answers.
use koru_models::ModelStore as _;

pub use koru_models::disk::{InstalledMetadata, METADATA_FILE, METADATA_SCHEMA_VERSION};

// The suite beside this module predates the extraction and names these
// through `super::*`.
#[cfg(test)]
use crate::{
    descriptor::{ModelDescriptor, ModelSignature},
    store::InstallStage,
};
#[cfg(test)]
use std::{path::PathBuf, sync::atomic::Ordering};

/// **Which speech models are on this machine, and how another one gets here.**
pub struct DiskModelStore {
    paths: ModelPaths,
    inner: koru_models::DiskModelStore,
}

impl DiskModelStore {
    /// A store over a resolved cache root, fetching through `source`.
    ///
    /// [`TrustPolicy::SIGNED`], because that is what a store nobody said
    /// anything to should be.
    pub fn new(paths: ModelPaths, source: Box<dyn ModelSource>) -> DiskModelStore {
        DiskModelStore::with_trust(paths, source, TrustPolicy::SIGNED)
    }

    /// The same, saying out loud how much a model has to prove.
    pub fn with_trust(
        paths: ModelPaths,
        source: Box<dyn ModelSource>,
        trust: TrustPolicy,
    ) -> DiskModelStore {
        let inner = koru_models::DiskModelStore::with_trust(
            paths.generic().clone(),
            Box::new(Typed(source)),
            trust,
            koru_voice_consent::VOICE,
        );
        DiskModelStore { paths, inner }
    }

    /// The product path: resolve the cache root, and download over HTTPS.
    ///
    /// `root` is the `--root` a rooted process was given, and is ignored when
    /// `KORU_VOICE_MODELS` is set — see [`ModelPaths::resolve`].
    pub fn open(root: Option<&Path>) -> Result<DiskModelStore, ModelError> {
        DiskModelStore::open_with_trust(root, TrustPolicy::SIGNED)
    }

    /// The product path, with the trust policy named by whoever is about to
    /// install something. The one call site that passes anything but
    /// [`TrustPolicy::SIGNED`] is `koru_workspace_host::voice_install`, and it
    /// says why there.
    pub fn open_with_trust(
        root: Option<&Path>,
        trust: TrustPolicy,
    ) -> Result<DiskModelStore, ModelError> {
        let paths = ModelPaths::resolve(root)?;
        let inner = koru_models::DiskModelStore::online(
            paths.generic().clone(),
            trust,
            koru_voice_consent::VOICE,
        )?;
        Ok(DiskModelStore { paths, inner })
    }

    /// A store that will not fetch. Everything a person can already do — list
    /// what is installed, load it, dictate — keeps working; only installing
    /// something new is refused, with the code that names the reason.
    pub fn offline(paths: ModelPaths) -> DiskModelStore {
        let inner = koru_models::DiskModelStore::offline(
            paths.generic().clone(),
            koru_voice_consent::VOICE,
        );
        DiskModelStore { paths, inner }
    }

    pub fn paths(&self) -> &ModelPaths {
        &self.paths
    }
}

/// Voice's `ModelId` as the generic one. The two share one charset rule
/// (`descriptor/projection_tests.rs` pins it), so this cannot refuse a value
/// voice accepted; if it ever did, the answer is *not a model here*.
fn generic_id(id: &ModelId) -> Result<koru_models::ModelId, ModelError> {
    koru_models::ModelId::parse(id.as_str())
}

impl ModelStore for DiskModelStore {
    fn root(&self) -> &Path {
        self.paths.root()
    }

    fn installed(&self) -> Result<Vec<InstalledModel>, ModelError> {
        Ok(self
            .inner
            .installed()?
            .into_iter()
            .filter_map(|model| InstalledModel::from_model(model).ok())
            .collect())
    }

    fn find(
        &self,
        id: &ModelId,
        version: &ModelVersion,
    ) -> Result<Option<InstalledModel>, ModelError> {
        Ok(self
            .inner
            .find(&generic_id(id)?, version)?
            .and_then(|model| InstalledModel::from_model(model).ok()))
    }

    fn install(
        &self,
        entry: &CatalogEntry,
        keys: &TrustedKeys,
        consent: &koru_voice_consent::Granted,
        progress: &mut dyn FnMut(InstallProgress),
        cancel: &AtomicBool,
    ) -> Result<InstalledModel, ModelError> {
        let installed = self
            .inner
            .install(&entry.to_model()?, keys, consent, progress, cancel)?;
        InstalledModel::from_model(installed)
    }

    fn lease(
        &self,
        id: &ModelId,
        version: &ModelVersion,
    ) -> Result<Box<dyn ModelLease>, ModelError> {
        let held = self.inner.lease(&generic_id(id)?, version)?;
        let model = InstalledModel::from_model(held.installed().clone()).map_err(|_| {
            ModelErrorCode::NotInstalled.with(format!("{id}/{version} is not a speech model"))
        })?;
        Ok(Box::new(DiskLease { model, _held: held }))
    }

    fn remove(&self, id: &ModelId, version: &ModelVersion) -> Result<(), ModelError> {
        self.inner.remove(&generic_id(id)?, version)
    }

    fn prune(&self, id: &ModelId, keep: usize) -> Result<Vec<ModelVersion>, ModelError> {
        self.inner.prune(&generic_id(id)?, keep)
    }
}

/// A lease over an installed speech model: voice's view of the model, and the
/// generic lease that keeps it. Everything it does happens when the generic
/// lease drops — see [`ModelLease`]'s note about why there is no `release`.
struct DiskLease {
    model: InstalledModel,
    _held: Box<dyn koru_models::ModelLease>,
}

impl ModelLease for DiskLease {
    fn installed(&self) -> &InstalledModel {
        &self.model
    }
}

#[cfg(test)]
mod tests;
