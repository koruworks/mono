// SPDX-License-Identifier: MPL-2.0

//! **The filesystem [`ModelStore`]: install, lease, load, prune, remove.**
//!
//! Plan 1.1 wrote the three interfaces; this is the implementation behind them,
//! and every decision in it comes from one fact — **the model cache is
//! channel-independent, so it has more than one writer**. A released build and
//! a dev build install, load, prune and remove into the same directory, and
//! neither can see the other's memory. Four mechanisms follow, and none of them
//! is optional:
//!
//! **Atomic publish.** A model is assembled in `.staging/` and reaches its
//! final name by one `rename`. There is no moment at which a concurrent reader
//! can meet a directory that is half extracted, because a directory either has
//! its final name or does not exist. This is also why the signed descriptor is
//! written *into staging* rather than after the rename: the file that says
//! *this is installed* arrives in the same instant as the bytes it describes.
//!
//! **A per-digest install lock.** Two workers reaching for the same 650 MB
//! produce one download and one wait. It is taken blocking, so the second one
//! finds the model installed the moment it wakes rather than being told to come
//! back.
//!
//! **A lease per live holder.** `prune` and `remove` ask the filesystem who is
//! using a model, not their own process. A lease is taken under the *shared*
//! side of the same install lock a delete takes exclusively, which is what
//! closes the window between *it is installed* and *nothing may delete it*.
//!
//! **Crash recovery with no clock.** A lease whose holder died unlocks, and is
//! reclaimed; a staging directory whose `.busy` unlocks is debris, and is
//! swept. Neither asks what time it is, because a heartbeat is wrong on a
//! machine whose clock moved and a stored pid is wrong on one that reused a pid.
//!
//! ## Offline is the ordinary state
//!
//! `.koru-model.json` sits inside every published model directory and carries
//! the **signed** descriptor and the signature that vouched for it. Loading a
//! model therefore needs no catalog, no `HEAD` and no network: `installed()`
//! and `find()` read that file and nothing else. It is also why a key that has
//! since been retired does not break anything already here — verification
//! happened once, at install, against the key that was pinned then, and this
//! file is the record of it (ADR-250).
//!
//! **It is deliberately not re-verified on every read.** Re-verifying would
//! make a rotation retroactively uninstall models, which is precisely what the
//! ADR says must not happen. What *is* checked on every read is cheaper and
//! catches the thing a signature cannot: that the descriptor's own
//! `id`/`version`/`digest` are the three directory names it was found under. A
//! metadata file moved into another model's directory names a different model
//! and is refused.
//!
//! ## The one thing this cannot know
//!
//! **A model archive is a zip.** The descriptor has no `format` field, and
//! adding one is a schema bump and a re-sign of every catalog entry
//! (ADR-250) — not a decision Plan 1.2 may take on its own. So the archive is
//! extracted with `koru_update::extract_zip` under [`ArchiveLimits`], which is
//! the hardened extractor the roadmap names, and anything that is not a zip is
//! [`InstallFailed`](ModelErrorCode::InstallFailed) with a message that says
//! so. Recorded as `docs/features/voice/archive/QUESTIONS.md` Q-V27.

use crate::catalog::CatalogEntry;
use crate::descriptor::{ModelDescriptor, ModelSignature, ModelVersion};
use crate::error::{ModelError, ModelErrorCode};
use crate::lock::{self, InstallLock, Lease, Staging};
use crate::paths::ModelPaths;
use crate::source::{HttpSource, ModelSource, NoNetworkSource};
use crate::store::{InstallProgress, InstallStage, InstalledModel, ModelLease, ModelStore};
use crate::token::ModelId;
use crate::trust::{TrustPolicy, TrustedKeys};
use koru_update::ArchiveLimits;
use koru_voice_consent::ConsentFile;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// What says *this directory is an installed model*, and the only thing that
/// does. Dotted, so [`ModelId`]'s refusal of a leading dot keeps it
/// out of the namespace an archive could write into.
pub const METADATA_FILE: &str = ".koru-model.json";

/// The shape of that file. Its own version, separate from the descriptor's:
/// this one can gain a field — a source URL, a byte count — without anything
/// signed changing, which is the same separation `CATALOG_SCHEMA_VERSION` keeps
/// from `DESCRIPTOR_SCHEMA_VERSION`.
pub const METADATA_SCHEMA_VERSION: u32 = 1;

/// **The signed descriptor, persisted beside the bytes it describes.**
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct InstalledMetadata {
    pub schema_version: u32,
    pub descriptor: ModelDescriptor,
    /// Kept although nothing re-checks it — see the module note. It is what a
    /// future audit tool, or a person with a rotated key and a question, has to
    /// work from, and re-deriving it would need the catalog this file exists to
    /// make unnecessary.
    pub signature: ModelSignature,
    /// Seconds since the epoch, written **once**, at install.
    ///
    /// A clock stamp is only ever safe on a path that runs once: AGENTS.md's
    /// rule is that a field holding the current time can never be part of a
    /// dirty check, and this one is not read by any comparison — only by
    /// [`ModelStore::prune`], which needs an order over builds and cannot get
    /// one from [`ModelVersion`], an opaque label rather than semver.
    pub installed_at_unix: u64,
}

/// Extraction limits. Ten thousand entries is `ArchiveLimits::default()`'s and
/// is right; the expanded size is not — a speech model is already larger
/// than the 1 GiB an update package is capped at, and the cap that matters here
/// is *a model may not be a zip bomb*, not *a model is small*.
const LIMITS: ArchiveLimits = ArchiveLimits {
    max_entries: 10_000,
    max_extracted_size: 8 * 1024 * 1024 * 1024,
};

/// **Which models are on this machine, and how another one gets here.**
pub struct DiskModelStore {
    paths: ModelPaths,
    source: Box<dyn ModelSource>,
    /// **How much a model has to prove**, chosen once when the store is built
    /// rather than argued at each install. See [`crate::TrustPolicy`] — and in
    /// particular why it is a value on the store: a policy passed per call is a
    /// policy a second caller passes differently.
    trust: TrustPolicy,
    /// **Whose consent opens this store.** A feature's store is built naming
    /// its own record — voice's [`koru_voice_consent::VOICE`], the reader's
    /// [`koru_voice_consent::READ`] — and `install` refuses a grant recorded by
    /// any other, however well its licence and size match. A value on the
    /// store for the reason `trust` is: chosen once, where the store is built.
    consent: ConsentFile,
}

impl DiskModelStore {
    /// A store over a cache root the caller resolved, fetching through
    /// `source`, opened by grants from `consent`.
    ///
    /// [`TrustPolicy::SIGNED`], because that is what a store nobody said
    /// anything to should be.
    pub fn new(
        paths: ModelPaths,
        source: Box<dyn ModelSource>,
        consent: ConsentFile,
    ) -> DiskModelStore {
        DiskModelStore::with_trust(paths, source, TrustPolicy::SIGNED, consent)
    }

    /// The same, saying out loud how much a model has to prove. The product
    /// path is this with an [`HttpSource`] and the policy the installing host
    /// chose.
    pub fn with_trust(
        paths: ModelPaths,
        source: Box<dyn ModelSource>,
        trust: TrustPolicy,
        consent: ConsentFile,
    ) -> DiskModelStore {
        DiskModelStore {
            paths,
            source,
            trust,
            consent,
        }
    }

    /// The product path's source: HTTPS, resuming what a previous run left.
    pub fn online(
        paths: ModelPaths,
        trust: TrustPolicy,
        consent: ConsentFile,
    ) -> Result<DiskModelStore, ModelError> {
        Ok(DiskModelStore::with_trust(
            paths,
            Box::new(HttpSource::new()?),
            trust,
            consent,
        ))
    }

    /// A store that will not fetch. Everything a person can already do — list
    /// what is installed, load it, use it — keeps working; only installing
    /// something new is refused, with the code that names the reason.
    ///
    /// [`TrustPolicy::SIGNED`], and it does not matter: a store with no source
    /// never reaches the byte that would need verifying.
    pub fn offline(paths: ModelPaths, consent: ConsentFile) -> DiskModelStore {
        DiskModelStore::new(paths, Box::new(NoNetworkSource), consent)
    }

    pub fn paths(&self) -> &ModelPaths {
        &self.paths
    }

    /// The consent record whose grants open this store.
    pub fn consent(&self) -> ConsentFile {
        self.consent
    }

    /// Read one candidate directory, answering `None` for anything that is not
    /// a complete, self-consistent install. **Never an error**: a walk that
    /// failed because one directory was odd would hide every model beside it.
    fn read_model(&self, dir: &Path) -> Option<InstalledModel> {
        let bytes = std::fs::read(dir.join(METADATA_FILE)).ok()?;
        let metadata: InstalledMetadata = serde_json::from_slice(&bytes).ok()?;
        if metadata.schema_version != METADATA_SCHEMA_VERSION {
            return None;
        }
        metadata.descriptor.validate().ok()?;
        // The three directory names are the model's identity; a metadata file
        // that names another model was moved, copied or written by something
        // that is not this store.
        let expected = metadata.descriptor.path_components();
        let found: Vec<String> = dir
            .components()
            .rev()
            .take(3)
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if found.len() != 3
            || found[0] != expected[2]
            || found[1] != expected[1]
            || found[2] != expected[0]
        {
            return None;
        }
        Some(InstalledModel {
            descriptor: metadata.descriptor,
            root: dir.to_path_buf(),
        })
    }

    /// Every published digest directory under one `<id>/<version>`, sorted so
    /// that two runs answer in the same order.
    fn digests_of(&self, id: &ModelId, version: &ModelVersion) -> Vec<InstalledModel> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir(self.paths.version_dir(id, version)) else {
            return found;
        };
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            if let Some(model) = self.read_model(&dir) {
                found.push(model);
            }
        }
        found
    }

    fn versions_of(&self, id: &ModelId) -> Vec<ModelVersion> {
        let Ok(entries) = std::fs::read_dir(self.paths.id_dir(id)) else {
            return Vec::new();
        };
        let mut versions: Vec<ModelVersion> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| ModelVersion::parse(e.file_name().to_string_lossy().into_owned()).ok())
            .collect();
        versions.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        versions
    }

    /// When a model was installed, for `prune`'s ordering. Zero for one whose
    /// metadata will not parse, which puts it first in line to go.
    fn installed_at(&self, model: &InstalledModel) -> u64 {
        std::fs::read(model.root.join(METADATA_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<InstalledMetadata>(&bytes).ok())
            .map(|metadata| metadata.installed_at_unix)
            .unwrap_or(0)
    }

    /// **Delete one published directory, refusing a leased one.**
    ///
    /// The rename comes first so that a reader never meets a model that is
    /// half gone — the mirror image of publishing. `refuse_leased` is the whole
    /// difference between `remove` and `prune`: an instruction must not
    /// silently do nothing, and housekeeping must not fail because somebody is
    /// using the model.
    fn delete(&self, model: &InstalledModel, refuse_leased: bool) -> Result<bool, ModelError> {
        let digest = &model.descriptor.digest;
        let _lock = InstallLock::exclusive(&self.paths.install_lock(digest))?;
        if lock::live_leases(&self.paths.leases_dir(digest)) > 0 {
            if refuse_leased {
                return Err(ModelErrorCode::Leased.with(format!(
                    "{}/{} is loaded by another process",
                    model.descriptor.id, model.descriptor.version
                )));
            }
            return Ok(false);
        }
        if !model.root.is_dir() {
            return Ok(true);
        }
        let condemned = Staging::create(&self.paths.staging_dir(), &format!("removing-{digest}"))?;
        let target = condemned.dir().to_path_buf();
        match std::fs::rename(&model.root, &target) {
            Ok(()) => drop(condemned), // Drop removes the directory it now owns.
            Err(_) => {
                // A rename can be refused while a virus scanner or an indexer
                // has a handle open. Deleting in place is the fallback, and the
                // window it opens is bounded by the exclusive lock we hold.
                condemned.release();
                std::fs::remove_dir_all(&model.root).map_err(|e| {
                    ModelErrorCode::Io
                        .with(format!("could not remove {}: {e}", model.root.display()))
                })?;
            }
        }
        let _ = std::fs::remove_dir_all(self.paths.leases_dir(digest));
        // Best effort: an empty `<id>/<version>` left behind is untidy and
        // harmless, and failing a delete over it would not be.
        let _ = std::fs::remove_dir(
            self.paths
                .version_dir(&model.descriptor.id, &model.descriptor.version),
        );
        let _ = std::fs::remove_dir(self.paths.id_dir(&model.descriptor.id));
        Ok(true)
    }

    /// Fetch, verify, extract and publish — everything after the signature
    /// check, with the per-digest lock already held.
    fn acquire_and_publish(
        &self,
        entry: &CatalogEntry,
        progress: &mut dyn FnMut(InstallProgress),
        cancel: &AtomicBool,
    ) -> Result<InstalledModel, ModelError> {
        let descriptor = &entry.descriptor;
        let archive = self.paths.archive_path(&descriptor.digest);

        // A complete archive left by a run that died between verifying and
        // publishing. Re-hashing it is minutes; re-downloading it is an hour.
        let already_here = archive.is_file() && descriptor.verify_archive(&archive).is_ok();
        if !already_here {
            progress(InstallProgress {
                stage: InstallStage::Downloading,
                received: 0,
                total: descriptor.size,
            });
            let mut last: Option<ModelError> = None;
            for url in entry.sources() {
                // Reborrowed rather than moved: `&mut dyn FnMut` is not `Copy`,
                // and a mirror loop that consumed the reporter would compile
                // only for the first host.
                match self
                    .source
                    .fetch(url, descriptor, &archive, &mut *progress, cancel)
                {
                    Ok(()) => {
                        last = None;
                        break;
                    }
                    // A cancellation is the caller's answer, not a mirror's
                    // failure, so it never falls through to the next host.
                    Err(error) if error.code == ModelErrorCode::Cancelled => return Err(error),
                    Err(error) => last = Some(error),
                }
            }
            if let Some(error) = last {
                return Err(error);
            }
            progress(InstallProgress {
                stage: InstallStage::Verifying,
                received: 0,
                total: descriptor.size,
            });
            if let Err(error) = descriptor.verify_archive(&archive) {
                // Wrong bytes never survive to poison the next attempt. Keeping
                // them would make every retry fail identically and for ever,
                // with nothing on screen to say why.
                let _ = std::fs::remove_file(&archive);
                return Err(error);
            }
            progress(InstallProgress {
                stage: InstallStage::Verifying,
                received: descriptor.size,
                total: descriptor.size,
            });
        }
        cancelled(cancel)?;

        progress(InstallProgress {
            stage: InstallStage::Unpacking,
            received: 0,
            total: 0,
        });
        let staging = Staging::create(&self.paths.staging_dir(), descriptor.digest.as_str())?;
        extract(&archive, staging.dir())?;
        let metadata = InstalledMetadata {
            schema_version: METADATA_SCHEMA_VERSION,
            descriptor: descriptor.clone(),
            signature: entry.signature.clone(),
            installed_at_unix: now_unix(),
        };
        let written = serde_json::to_vec_pretty(&metadata).map_err(|e| {
            ModelErrorCode::InstallFailed.with(format!("could not render the descriptor: {e}"))
        })?;
        std::fs::write(staging.dir().join(METADATA_FILE), written)?;
        cancelled(cancel)?;

        progress(InstallProgress {
            stage: InstallStage::Publishing,
            received: 0,
            total: 0,
        });
        let final_dir = self.paths.model_dir(descriptor);
        if let Some(parent) = final_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(staging.dir(), &final_dir).map_err(|e| {
            ModelErrorCode::InstallFailed
                .with(format!("could not publish {}: {e}", final_dir.display()))
        })?;
        staging.release();
        let _ = std::fs::remove_file(&archive);
        self.read_model(&final_dir).ok_or_else(|| {
            ModelErrorCode::InstallFailed.with(format!(
                "{} was published and does not read back as installed",
                final_dir.display()
            ))
        })
    }
}

impl ModelStore for DiskModelStore {
    fn root(&self) -> &Path {
        self.paths.root()
    }

    fn installed(&self) -> Result<Vec<InstalledModel>, ModelError> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir(self.paths.root()) else {
            // A cache that does not exist yet holds no models, which is an
            // answer and not a failure — the first thing a settings page asks
            // is this, on a machine where nothing has been installed.
            return Ok(found);
        };
        let mut ids: Vec<ModelId> = entries
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if ModelPaths::is_internal(&name) {
                    return None;
                }
                ModelId::parse(name).ok()
            })
            .collect();
        ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        for id in ids {
            for version in self.versions_of(&id) {
                found.extend(self.digests_of(&id, &version));
            }
        }
        Ok(found)
    }

    fn find(
        &self,
        id: &ModelId,
        version: &ModelVersion,
    ) -> Result<Option<InstalledModel>, ModelError> {
        let mut found = self.digests_of(id, version);
        if found.len() > 1 {
            // Two sets of bytes under one name. `install` refuses to create
            // this, so meeting it means something else did — and answering with
            // one of them arbitrarily is exactly the silent shadowing ADR-250
            // forbids.
            let digests: Vec<&str> = found
                .iter()
                .map(|model| model.descriptor.digest.as_str())
                .collect();
            return Err(ModelErrorCode::IdentityConflict.with(format!(
                "{id}/{version} is installed as {} different digests: {}",
                digests.len(),
                digests.join(", ")
            )));
        }
        Ok(found.pop())
    }

    fn install(
        &self,
        entry: &CatalogEntry,
        keys: &TrustedKeys,
        consent: &koru_voice_consent::Granted,
        progress: &mut dyn FnMut(InstallProgress),
        cancel: &AtomicBool,
    ) -> Result<InstalledModel, ModelError> {
        progress(InstallProgress {
            stage: InstallStage::Resolving,
            received: 0,
            total: 0,
        });
        // **Before the signature, and long before any network.** Holding a
        // token says somebody was asked; this says they were asked about
        // *this*. It is first because the order of a refusal is part of what a
        // person is told: a build with no pinned key answering `no_trust_root`
        // to a download nobody consented to would name the wrong problem.
        let offer = entry.terms();
        if consent.record() != self.consent {
            // Another feature's grant, however well it would otherwise match:
            // somebody who agreed to a speech model has not agreed to a reader.
            return Err(ModelErrorCode::ConsentRequired.with(format!(
                "the grant offered was recorded in {}, and this store installs only under {}",
                consent.record().name(),
                self.consent.name(),
            )));
        }
        if !self.consent.covers(consent, &offer) {
            return Err(ModelErrorCode::ConsentRequired.with(format!(
                "the grant on this machine covers {} up to {} bytes, and {}/{} is {} at {} bytes",
                consent.terms().licence,
                consent.terms().download_bytes,
                entry.id(),
                entry.version(),
                offer.licence,
                offer.download_bytes,
            )));
        }
        // **First, and before any network.** Hashing 650 MB to discover the
        // descriptor was forged is work nobody needed to do, and a build with
        // no pinned key answers `no_trust_root` here rather than after an hour.
        if self.trust.is_digest_only() {
            // **Unconditional, at `warn`, and not behind any flag.** Installing
            // several hundred megabytes of weights that nothing vouched for is
            // rare, consequential and about somebody's own machine, which is
            // exactly the category `docs/contributing/operations/LOGGING.md` says always
            // gets a line. It names the model and the digest and no URL: a
            // mirror is not what was trusted here, and printing one would
            // suggest it was. `consent` names whose store it is — voice's or
            // the reader's — without this crate having to know either.
            tracing::warn!(
                target: "koru::models",
                model = %entry.id(),
                version = %entry.version(),
                digest = %descriptor_digest(entry),
                consent = self.consent.name(),
                "installing a model WITHOUT a signature: this build pins no \
                 model-signing key, so only the catalog's own digest was checked"
            );
            // The signature is skipped and nothing else is. Every field rule
            // the descriptor declares still applies, and the signed size and
            // digest are still checked against the bytes after the fetch — a
            // policy that skipped those would not be digest-only, it would be
            // nothing.
            entry.descriptor.validate()?;
        } else {
            entry.verify(keys)?;
        }
        cancelled(cancel)?;

        let descriptor = &entry.descriptor;
        if let Some(model) = self.find(&descriptor.id, &descriptor.version)? {
            return same_or_conflict(model, descriptor);
        }
        let _lock = InstallLock::exclusive(&self.paths.install_lock(&descriptor.digest))?;
        // Asked again under the lock: the process we waited for may have been
        // installing exactly this, which is what makes two installs one.
        if let Some(model) = self.find(&descriptor.id, &descriptor.version)? {
            return same_or_conflict(model, descriptor);
        }
        lock::sweep_staging(&self.paths.staging_dir());
        self.acquire_and_publish(entry, progress, cancel)
    }

    fn lease(
        &self,
        id: &ModelId,
        version: &ModelVersion,
    ) -> Result<Box<dyn ModelLease>, ModelError> {
        let model = self.find(id, version)?.ok_or_else(|| {
            ModelErrorCode::NotInstalled.with(format!("{id}/{version} is not installed"))
        })?;
        let digest = model.descriptor.digest.clone();
        // Shared, so any number of workers may load one model and none of them
        // may be deleting it. Held only across creating the lease file, which
        // is the window a delete could otherwise slip through.
        let guard = InstallLock::shared(&self.paths.install_lock(&digest))?;
        let model = self.find(id, version)?.ok_or_else(|| {
            ModelErrorCode::NotInstalled.with(format!("{id}/{version} is not installed"))
        })?;
        let held = Lease::take(&self.paths.leases_dir(&digest))?;
        drop(guard);
        Ok(Box::new(DiskLease { model, _held: held }))
    }

    fn remove(&self, id: &ModelId, version: &ModelVersion) -> Result<(), ModelError> {
        let model = self.find(id, version)?.ok_or_else(|| {
            ModelErrorCode::NotInstalled.with(format!("{id}/{version} is not installed"))
        })?;
        self.delete(&model, true)?;
        Ok(())
    }

    fn prune(&self, id: &ModelId, keep: usize) -> Result<Vec<ModelVersion>, ModelError> {
        lock::sweep_staging(&self.paths.staging_dir());
        let mut builds: Vec<(u64, ModelVersion, Vec<InstalledModel>)> = Vec::new();
        for version in self.versions_of(id) {
            let models = self.digests_of(id, &version);
            if models.is_empty() {
                continue;
            }
            let newest = models
                .iter()
                .map(|model| self.installed_at(model))
                .max()
                .unwrap_or(0);
            builds.push((newest, version, models));
        }
        // **Newest by install time, not by version string.** `ModelVersion` is
        // an opaque label — `1.2.0`, `2026-03-int8` — and inventing an order
        // over it would be a comparison the catalog never promised. The name is
        // the tie-break so that two builds installed in one second still order
        // deterministically.
        builds.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.as_str().cmp(a.1.as_str())));

        let mut deleted = Vec::new();
        for (_, version, models) in builds.into_iter().skip(keep) {
            let mut all_gone = true;
            for model in &models {
                // A leased build is kept and is **not** an error: pruning is
                // housekeeping and must not fail because somebody is using it.
                if !self.delete(model, false)? {
                    all_gone = false;
                }
            }
            if all_gone {
                deleted.push(version);
            }
        }
        Ok(deleted)
    }
}

/// The digest, for the one log line that names it. A function rather than an
/// inline field access so the log line stays one expression wide and the
/// borrow does not fight the `warn!` macro's own.
fn descriptor_digest(entry: &CatalogEntry) -> &str {
    entry.descriptor.digest.as_str()
}

/// A lease over an installed model. Everything it does happens in `Drop` — see
/// [`ModelLease`]'s own note about why there is no `release`.
struct DiskLease {
    model: InstalledModel,
    _held: Lease,
}

impl ModelLease for DiskLease {
    fn installed(&self) -> &InstalledModel {
        &self.model
    }
}

/// The same bytes are the same install; different bytes under one name are a
/// hard failure and never a silent replace (ADR-250).
fn same_or_conflict(
    model: InstalledModel,
    descriptor: &ModelDescriptor,
) -> Result<InstalledModel, ModelError> {
    if model.descriptor.digest == descriptor.digest {
        return Ok(model);
    }
    Err(ModelErrorCode::IdentityConflict.with(format!(
        "{}/{} is installed as {} and the catalog offers {}",
        descriptor.id, descriptor.version, model.descriptor.digest, descriptor.digest
    )))
}

fn cancelled(cancel: &AtomicBool) -> Result<(), ModelError> {
    if cancel.load(Ordering::Acquire) {
        return Err(ModelErrorCode::Cancelled.with("the install was cancelled"));
    }
    Ok(())
}

/// `koru_update::extract_zip` under [`LIMITS`] — the hardened extractor the
/// roadmap names, which already refuses a traversal, a symlink, a duplicate
/// path and a zip bomb. See the module note for why zip and nothing else.
fn extract(archive: &Path, destination: &Path) -> Result<(), ModelError> {
    koru_update::extract_zip(archive, destination, LIMITS).map_err(|e| {
        ModelErrorCode::InstallFailed.with(format!(
            "{} could not be extracted as a zip: {e}",
            archive.display()
        ))
    })
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
