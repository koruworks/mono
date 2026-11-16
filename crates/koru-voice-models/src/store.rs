// SPDX-License-Identifier: MPL-2.0

//! **Install, lease, load — in voice's words.**
//!
//! The mechanism behind these interfaces is `koru-models`' — the generic
//! `ModelStore`, `ModelLease` and `LoadedModel`, the disk store with its locks,
//! leases and atomic publish. This module keeps voice's own copies of the three
//! interfaces because voice's worker, its engine adapters and its tests are
//! written against them in voice's types: a model is named by
//! `koru_voice::ModelId`, an engine by `EngineId`, a descriptor by the typed
//! [`ModelDescriptor`]. [`crate::DiskModelStore`] implements voice's
//! [`ModelStore`] by wrapping the generic disk store and mapping every
//! descriptor through the tokens.
//!
//! ## The three, and why they are three
//!
//! **Install** puts verified bytes on the disk and is slow, cancellable and
//! progress-reporting. **Lease** says *somebody is using this*, and exists only
//! because the model cache is channel-independent: a released build and a dev
//! build share one directory, so a process-local "currently loaded" flag cannot
//! see the other one's `prune`. **Load** is the pair of a lease and an engine
//! check, and is what an engine adapter is handed.
//!
//! ## A lease is released by dropping it
//!
//! There is deliberately no `release()`. A lease is a `Box<dyn ModelLease>`
//! whose implementation does its work in `Drop`, so the only way to hold one is
//! to hold it and the only way to let go is to stop — which means an inference
//! thread that panics releases its lease, and so does one that returns early
//! down a path nobody wrote a cleanup for.
//!
//! ## None of this may run on a thread that answers a person
//!
//! [`ModelStore::install`] downloads hundreds of megabytes and hashes them;
//! [`ModelStore::lease`] takes a cross-process lock; even
//! [`ModelStore::installed`] walks directories. All of them are *the world*, in
//! the sense [PRINCIPLES](../../../guide/contributing/PRINCIPLES.md) means it, and the window's
//! frame loop and the agent's event loop may call none of them. The worker's
//! inference thread is where they belong.

use crate::catalog::CatalogEntry;
use crate::descriptor::{ModelDescriptor, ModelVersion};
use crate::error::{ModelError, ModelErrorCode};
use crate::tier::ModelTier;
use crate::trust::TrustedKeys;
use koru_voice::{EngineId, ModelId, Requested};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub use koru_models::store::{InstallProgress, InstallStage};

// The suite beside this module predates the extraction, when
// `session_error` was an inherent method; it is a trait method now and the
// suite names it through `super::*`.
#[cfg(test)]
use crate::error::SessionErrorExt;

/// A speech model whose bytes are on this machine, verified, and complete.
///
/// It carries the *signed* descriptor, not a remembered summary of one:
/// the disk store writes it beside the bytes at install time, as
/// `.koru-model.json`, which is what makes loading a model an offline
/// operation. Nothing here needs a catalog to exist.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InstalledModel {
    pub descriptor: ModelDescriptor,
    /// The directory holding the model's files —
    /// `<cache>/<id>/<version>/<digest>/`. Cache identity is the digest
    /// ([ADR-250](../../../docs/decisions/ADR-226-250.md)), so this path is
    /// unique to these exact bytes.
    pub root: PathBuf,
}

impl InstalledModel {
    /// A file inside the model directory, by relative path.
    ///
    /// **Refuses anything that leaves** — an absolute path, a drive prefix, a
    /// `..`, a bare `.`, a separator for the other platform — with
    /// [`ModelErrorCode::PathEscape`]. The rule is
    /// [`koru_models::file_inside`]'s, applied rather than copied.
    pub fn file(&self, relative: &str) -> Result<PathBuf, ModelError> {
        koru_models::file_inside(&self.root, relative)
    }

    pub fn id(&self) -> &ModelId {
        &self.descriptor.id
    }

    pub fn version(&self) -> &ModelVersion {
        &self.descriptor.version
    }

    /// The speech model a generic installed model is, or a refusal when its
    /// descriptor names something speech has no word for.
    pub(crate) fn from_model(model: koru_models::InstalledModel) -> Result<Self, ModelError> {
        Ok(InstalledModel {
            descriptor: ModelDescriptor::from_model(model.descriptor)?,
            root: model.root,
        })
    }
}

/// **Choose among models that are already here.**
///
/// [`crate::Catalog::choose`]'s rules, applied to installed bytes, because a
/// person who is offline still has to be able to start a dictation. The two are
/// separate functions rather than one generic because they answer different
/// questions at different moments — *what could I install* and *what can I use
/// right now* — and a caller that confused them would offer a download to
/// somebody who already had the model.
pub fn choose_installed<'a>(
    models: &'a [InstalledModel],
    requested: Requested,
    tier: ModelTier,
    engine: Option<&EngineId>,
) -> Option<&'a InstalledModel> {
    models.iter().find(|model| {
        model.descriptor.tier == tier
            && model.descriptor.covers(requested)
            && engine.is_none_or(|wanted| &model.descriptor.engine == wanted)
    })
}

/// **Could a dictation start right now?** — asked of the disk, every time.
///
/// This is the whole of *installed state*, and the point is that there is
/// nowhere else it could be read from. It is not a field, not a cached
/// `bool`, and deliberately not a neighbour of the consent record: consent is
/// a promise somebody made and survives the bytes, while this is a question
/// about a directory and has to be re-asked after anything that could have
/// emptied it — a `prune` from another channel, a person deleting the cache, a
/// disk that was not mounted this morning.
///
/// It walks directories, so it is [the world](../../../guide/contributing/PRINCIPLES.md) in
/// the sense the rest of this module's note means: not from a frame loop.
///
/// A store that cannot be read at all answers `false` rather than an error,
/// which is the same reading the tray already takes.
pub fn anything_installed(store: &dyn ModelStore) -> bool {
    store.installed().is_ok_and(|models| !models.is_empty())
}

/// **Somebody is using this model; do not delete it.**
///
/// Released by dropping — see the module note.
pub trait ModelLease: Send {
    /// The model this lease holds open.
    fn installed(&self) -> &InstalledModel;
}

/// **Which speech models are on this machine, and how another one gets here.**
///
/// A trait rather than a struct so that the worker can be built and tested
/// against a store that never touches a disk, and so that a phone — which has
/// a different cache root, no channels and no second writer — can implement the
/// same verbs differently without the worker noticing.
/// [`crate::DiskModelStore`] is the desktop's.
///
/// Every method blocks. See the module note about which threads may call them.
pub trait ModelStore: Send + Sync {
    /// The cache root every path this store answers with lives under.
    /// A test asserts that; it is the invariant that keeps
    /// `KORU_VOICE_MODELS` from silently writing outside the profile it was
    /// pointed away from.
    fn root(&self) -> &Path;

    /// Every complete, verified model installed here.
    fn installed(&self) -> Result<Vec<InstalledModel>, ModelError>;

    /// One model build, if it is installed. `Ok(None)` rather than
    /// [`ModelErrorCode::NotInstalled`], because *is it here* is a question and
    /// *give it to me* is a demand, and only the second one has a right to
    /// fail.
    fn find(
        &self,
        id: &ModelId,
        version: &ModelVersion,
    ) -> Result<Option<InstalledModel>, ModelError>;

    /// **Fetch, verify and publish a model.**
    ///
    /// The order is normative: check that consent covers this offer, verify
    /// the signature over the descriptor (no bytes needed), then fetch, then
    /// check size and digest, then publish atomically from staging. A caller
    /// that already has these bytes gets the existing install back rather than
    /// a second download.
    ///
    /// Meeting the same `id`/`version` already installed with a *different*
    /// digest is [`ModelErrorCode::IdentityConflict`] — a hard failure, never a
    /// silent replace.
    ///
    /// `progress` is called from this thread; `cancel` is polled between
    /// chunks and answers [`ModelErrorCode::Cancelled`].
    ///
    /// **`consent` is a parameter and not a check somewhere else.** A
    /// [`koru_voice_consent::Granted`] is what the consent step answers with,
    /// it is the only thing that opens this door, and a caller that has not
    /// asked anybody cannot compile. Holding one is still not enough — an
    /// implementation answers [`ModelErrorCode::ConsentRequired`] when the
    /// grant does not reach this entry, because agreeing to 600 MB of
    /// Apache-2.0 weights is not agreeing to whatever the catalog lists next,
    /// and a grant recorded for another feature is not voice's.
    fn install(
        &self,
        entry: &CatalogEntry,
        keys: &TrustedKeys,
        consent: &koru_voice_consent::Granted,
        progress: &mut dyn FnMut(InstallProgress),
        cancel: &AtomicBool,
    ) -> Result<InstalledModel, ModelError>;

    /// Claim a model so that nothing prunes it while it is loaded.
    ///
    /// Fails [`ModelErrorCode::NotInstalled`]. Released by dropping the box.
    fn lease(
        &self,
        id: &ModelId,
        version: &ModelVersion,
    ) -> Result<Box<dyn ModelLease>, ModelError>;

    /// Delete one model build. Refuses [`ModelErrorCode::Leased`] while
    /// anything holds it — including a process on another channel.
    fn remove(&self, id: &ModelId, version: &ModelVersion) -> Result<(), ModelError>;

    /// Keep the newest `keep` builds of `id` and delete the rest, answering
    /// what was deleted.
    ///
    /// **Leased builds are kept and are not an error**, which is the difference
    /// between `prune` and [`ModelStore::remove`]: pruning is housekeeping and
    /// must not fail because somebody is dictating, while removing is an
    /// instruction and must not silently do nothing.
    fn prune(&self, id: &ModelId, keep: usize) -> Result<Vec<ModelVersion>, ModelError>;
}

/// **A model held open for an engine to load.**
///
/// The load interface, and the whole of it: an engine adapter is handed one of
/// these, asks it for a directory or a file, and loads whatever it finds. It
/// never sees the store, never sees a catalog and never sees a URL — which is
/// what lets `koru-voice-sherpa` and `koru-voice-whisper` link this crate
/// without acquiring a downloader.
///
/// Dropping it releases the lease.
pub struct LoadedModel {
    lease: Box<dyn ModelLease>,
}

impl LoadedModel {
    /// Lease a model and check it is for the engine that is about to load it.
    ///
    /// **The engine check is the reason this function exists** rather than
    /// callers using [`ModelStore::lease`] directly. `engine` is inside the
    /// signature envelope precisely so that verified bytes cannot be routed
    /// into the wrong native parser (ADR-250), and a check nobody performs is a
    /// field nobody signed.
    pub fn open(
        store: &dyn ModelStore,
        id: &ModelId,
        version: &ModelVersion,
        engine: &EngineId,
    ) -> Result<LoadedModel, ModelError> {
        let lease = store.lease(id, version)?;
        let found = &lease.installed().descriptor.engine;
        if found != engine {
            return Err(ModelErrorCode::EngineMismatch
                .with(format!("{id}/{version} is for {found}, not {engine}")));
        }
        Ok(LoadedModel { lease })
    }

    /// Wrap a lease that has already been checked. The seam a test — or a
    /// mobile host with one bundled model and no store — uses.
    pub fn from_lease(lease: Box<dyn ModelLease>) -> LoadedModel {
        LoadedModel { lease }
    }

    pub fn installed(&self) -> &InstalledModel {
        self.lease.installed()
    }

    pub fn descriptor(&self) -> &ModelDescriptor {
        &self.lease.installed().descriptor
    }

    /// The directory the model's files are in.
    pub fn root(&self) -> &Path {
        &self.lease.installed().root
    }

    /// A file inside it, refusing anything that would leave. See
    /// [`InstalledModel::file`].
    pub fn file(&self, relative: &str) -> Result<PathBuf, ModelError> {
        self.lease.installed().file(relative)
    }
}

impl std::fmt::Debug for LoadedModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedModel")
            .field("id", &self.installed().id().as_str())
            .field("version", &self.installed().version().as_str())
            .finish()
    }
}

#[cfg(test)]
mod tests;
