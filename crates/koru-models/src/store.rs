// SPDX-License-Identifier: MPL-2.0

//! **Install, lease, load — the three interfaces a model's consumer is written
//! against, normative in Rust rather than described in prose.**
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
//! to hold it and the only way to let go is to stop — which means a thread that
//! panics releases its lease, and so does one that returns early down a path
//! nobody wrote a cleanup for.
//!
//! ## None of this may run on a thread that answers a person
//!
//! [`ModelStore::install`] downloads hundreds of megabytes and hashes them;
//! [`ModelStore::lease`] takes a cross-process lock; even
//! [`ModelStore::installed`] walks directories. All of them are *the world*, in
//! the sense [PRINCIPLES](../../../guide/contributing/PRINCIPLES.md) means it,
//! and no frame loop, event loop or request path may call them.
//!
//! ## A feature's typed store
//!
//! `koru-voice-models` keeps a `ModelStore` of its own whose methods speak
//! `EngineId` and voice's `ModelId`, because voice's worker and its tests are
//! written against those words. It is a projection over this one: its disk
//! store wraps [`crate::DiskModelStore`] and maps every descriptor through the
//! tokens. A new consumer can use these traits directly.

use crate::catalog::CatalogEntry;
use crate::descriptor::{ModelDescriptor, ModelVersion};
use crate::error::{ModelError, ModelErrorCode};
use crate::token::{ModelId, Token};
use crate::trust::TrustedKeys;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// A model whose bytes are on this machine, verified, and complete.
///
/// It carries the *signed* descriptor, not a remembered summary of one:
/// [`crate::DiskModelStore`] writes it beside the bytes at install time, as
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
    /// A file inside the model directory, by relative path. See
    /// [`file_inside`].
    pub fn file(&self, relative: &str) -> Result<PathBuf, ModelError> {
        file_inside(&self.root, relative)
    }

    pub fn id(&self) -> &ModelId {
        &self.descriptor.id
    }

    pub fn version(&self) -> &ModelVersion {
        &self.descriptor.version
    }
}

/// **A file inside a model directory, refusing anything that leaves.**
///
/// An absolute path, a drive prefix, a `..`, a bare `.`, a component carrying a
/// separator for the *other* platform — each is [`ModelErrorCode::PathEscape`]
/// rather than something normalised into safety. Engine adapters name their
/// files from strings that came out of a model archive, and an archive is
/// remote input however carefully it was verified: the signature says who
/// published the bytes, not that every name inside them is sensible.
///
/// A free function so a feature's own typed `InstalledModel` applies this rule
/// rather than a copy of it.
pub fn file_inside(root: &Path, relative: &str) -> Result<PathBuf, ModelError> {
    let bad = |why: &str| {
        Err(ModelErrorCode::PathEscape.with(format!("{relative:?} {why}, so it is refused")))
    };
    if relative.is_empty() {
        return bad("is empty");
    }
    if relative.contains('\\') || relative.contains(':') {
        // Rejected on every platform rather than only where it means
        // something, so a traversal cannot be a bug that reproduces on one
        // developer's machine.
        return bad("carries a backslash or a colon");
    }
    let candidate = Path::new(relative);
    for component in candidate.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir => return bad("contains a '.' component"),
            Component::ParentDir => return bad("climbs out with '..'"),
            Component::RootDir | Component::Prefix(_) => return bad("is absolute"),
        }
    }
    Ok(root.join(candidate))
}

/// **Is anything usable installed?** — asked of the disk, every time.
///
/// This is the whole of *installed state*, and the point is that there is
/// nowhere else it could be read from. It is not a field, not a cached `bool`,
/// and deliberately not a neighbour of the consent record: consent is a promise
/// somebody made and survives the bytes, while this is a question about a
/// directory and has to be re-asked after anything that could have emptied it.
///
/// A store that cannot be read at all answers `false` rather than an error:
/// *nothing usable is here* is what a person can act on.
pub fn anything_installed(store: &dyn ModelStore) -> bool {
    store.installed().is_ok_and(|models| !models.is_empty())
}

/// Where an install has got to.
///
/// **Progress is coalesced by whoever emits it** — at most one report per
/// 250 ms or per whole percent. The update path emits one per 64 KiB chunk,
/// which over 650 MB is roughly ten thousand events and is the anti-pattern
/// `docs/contributing/operations/LOGGING.md` names by name. The rule is kept in
/// one place — `progress::Coalescer` — rather than at each emitter.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct InstallProgress {
    pub stage: InstallStage,
    /// Bytes done, in whatever unit the stage counts. Zero outside
    /// [`InstallStage::Downloading`] and [`InstallStage::Verifying`].
    pub received: u64,
    /// Bytes expected. Zero when the stage has no total to speak of.
    pub total: u64,
}

/// The stages of an install, in the order they happen.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum InstallStage {
    /// Checking consent and the signature, and looking for an existing install
    /// of these exact bytes. Before any network.
    Resolving,
    Downloading,
    /// Re-hashing the completed file. A separate stage because it is minutes of
    /// work on a slow disk and a progress bar that stopped at 100% for two
    /// minutes reads as a hang.
    Verifying,
    Unpacking,
    /// The atomic rename into place. Brief, and the only moment at which a
    /// concurrent reader could see a model appear.
    Publishing,
}

/// **Somebody is using this model; do not delete it.**
///
/// Released by dropping — see the module note. [`crate::DiskModelStore`]'s
/// implementation is an exclusively locked file per live holder rather than a
/// flag, because channel-independence means the process that would delete the
/// bytes is not the process that is using them.
pub trait ModelLease: Send {
    /// The model this lease holds open.
    fn installed(&self) -> &InstalledModel;
}

/// **Which models are on this machine, and how another one gets here.**
///
/// A trait rather than a struct so a consumer can be built and tested against a
/// store that never touches a disk, and so a phone — which has a different
/// cache root, no channels and no second writer — can implement the same verbs
/// differently. [`crate::DiskModelStore`] is the desktop's.
///
/// Every method blocks. See the module note about which threads may call them.
pub trait ModelStore: Send + Sync {
    /// The cache root every path this store answers with lives under.
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
    /// The order is normative: check consent covers this offer, verify the
    /// signature over the descriptor (no bytes needed), then fetch, then check
    /// size and digest, then publish atomically from staging. A caller that
    /// already has these bytes gets the existing install back rather than a
    /// second download.
    ///
    /// Meeting the same `id`/`version` already installed with a *different*
    /// digest is [`ModelErrorCode::IdentityConflict`] — a hard failure, never a
    /// silent replace.
    ///
    /// `progress` is called from this thread; `cancel` is polled between
    /// chunks and answers [`ModelErrorCode::Cancelled`].
    ///
    /// **`consent` is a parameter and not a check somewhere else**, so a caller
    /// that has not asked anybody cannot compile. Holding one is still not
    /// enough: an implementation answers [`ModelErrorCode::ConsentRequired`]
    /// when the grant does not reach this entry — another licence, more bytes,
    /// or **a grant recorded by another feature**
    /// ([`ConsentFile::covers`](koru_voice_consent::ConsentFile::covers)).
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
    /// must not fail because somebody is using a model, while removing is an
    /// instruction and must not silently do nothing.
    fn prune(&self, id: &ModelId, keep: usize) -> Result<Vec<ModelVersion>, ModelError>;
}

/// **A model held open for an engine to load.**
///
/// The load interface, and the whole of it: an engine adapter is handed one of
/// these, asks it for a directory or a file, and loads whatever it finds. It
/// never sees the store, a catalog or a URL.
///
/// Dropping it releases the lease.
pub struct LoadedModel {
    lease: Box<dyn ModelLease>,
}

impl LoadedModel {
    /// Lease a model and check it is for the engine that is about to load it.
    ///
    /// **The engine check is the reason this function exists.** `engine` is
    /// inside the signature envelope precisely so that verified bytes cannot be
    /// routed into the wrong native parser (ADR-250), and a check nobody
    /// performs is a field nobody signed.
    pub fn open(
        store: &dyn ModelStore,
        id: &ModelId,
        version: &ModelVersion,
        engine: &Token,
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
    /// [`file_inside`].
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
