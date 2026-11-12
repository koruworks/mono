// SPDX-License-Identifier: MPL-2.0

//! **The reader's installer: the Store daemon downloads the model that finds
//! words in pictures, after a person agrees** (OCR TECHNICAL D16, §7
//! "Preferences and consent", §9 "The first-time consent", "The reader is
//! turned off").
//!
//! Voice installs from the window (`koru-workspace-host`'s `voice_install.rs`)
//! and its download stops with the last window. The reader's offer appears in
//! three places — one of them the Agent-hosted search window — and its
//! consumer is the Store's coordinator, so one installer here serves every
//! surface and the Settings row, and a download survives a closed window.
//!
//! ## One thread, and every request answered from memory
//!
//! The installer's own thread reads the catalog, writes `read-consent.conf`
//! and the *Not now* flag, downloads, verifies, publishes and removes. A
//! request ([`Installer`] as `koru_store_api`'s `ReaderService`) reads the
//! [`ReaderHub`] and pushes a command; **Stop** flips the running download's
//! cancel flag, which the model store polls between chunks. Nothing on a
//! request path waits on the disk, the network or the Store lock.
//!
//! ## Where things are
//!
//! | what | where |
//! | --- | --- |
//! | the catalog | `KORU_READ_CATALOG`, else [`DEFAULT_CATALOG_URL`] |
//! | the model cache | `KORU_READ_MODELS`, else `<store root>/read-models` for a rooted Store, else `<local data>/Koru/read-models` (channel-independent, as voice's cache is) |
//! | the consent record | `<store root>/reader/read-consent.conf` (`koru_voice_consent::READ`) |
//! | *Not now* | `<store root>/reader/offer.conf` |
//!
//! The consent and the flag are **machine-local**, beside each other, under
//! the Store's own directory. Installed state is asked of the disk, never
//! stored (ADR-251).
//!
//! ## Trust
//!
//! Chosen once, exactly as voice chooses: the pinned model-signing key if one
//! exists, else the guarded digest-only arm, which a stable build refuses. On
//! stable there is no key yet, so the reader is
//! [`ReaderUnavailable::NoTrustRoot`]: no offer is made, no catalog is
//! fetched, and an install answers `reader_unavailable`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use koru_models::{
    Catalog, CatalogEntry, CatalogSource, DiskModelStore, InstalledModel, ModelError,
    ModelErrorCode, ModelPaths, ModelSource, ModelStore, TrustPolicy, TrustedKeys,
};
use koru_store_core::errors::{Code, Error};
use koru_store_wire::reader::{
    CatalogStage, CatalogState, ImportFinished, InstallReader, InstallStage, ReaderState,
};

use super::feed::Inbox;
use super::hub::{Command, ReaderHub, progress, terms_of};

/// The reader's catalog, as `koru-release`'s `read-model publish` puts it on
/// the update origin. Overridden by `KORU_READ_CATALOG`.
pub const DEFAULT_CATALOG_URL: &str = "https://updates.koru.do/read/catalog-v1.json";

/// The catalog entry this build installs: PP-OCR's runtime, at the one tier
/// there is. The descriptor's `engine` is also the reader token written into
/// every reading recognized with it.
pub const READER_ENGINE: &str = "pp-ocr";
pub const READER_TIER: &str = "standard";

/// What the *Not now* flag's file says it is.
const OFFER_FORMAT: &str = "koru-reader-offer";
const OFFER_FILE: &str = "offer.conf";

/// How long a removal waits for the coordinator to let the model go — the
/// worker stopped, its lease dropped — before it tries anyway. A job in
/// progress is let finish; a long scan may take minutes.
const RELEASE_PATIENCE: Duration = Duration::from_secs(600);

/// How long the installer's thread waits for a command before it looks at
/// its stop flag. Woken by commands, never by this.
const IDLE: Duration = Duration::from_millis(500);

/// **Where the installer tells the event bus what happened**: the reader
/// installed, the reader removed (OCR TECHNICAL §10), each beside the log
/// line that says the same. The daemon passes the Store's own catalog-fact
/// sink; a host without an events worker passes nothing, and a failed record
/// is never a failed install.
pub type Announce = Box<dyn Fn(koru_events::Draft) + Send>;

/// **Where the installer reads, writes and fetches**, resolved once.
pub struct InstallerConfig {
    pub models: ModelPaths,
    /// Where `read-consent.conf` and the *Not now* flag live.
    pub state_dir: PathBuf,
    pub catalog: Result<CatalogSource, ModelError>,
    pub trust: TrustPolicy,
    /// This build may install nothing: a stable build with no key.
    pub no_trust_root: bool,
    /// Where archives are fetched from. `None` is the product's HTTPS
    /// downloader, **built on the installer's own thread**: a blocking HTTP
    /// client made (or dropped) inside the daemon's async start-up panics.
    pub source: Option<Box<dyn ModelSource>>,
}

impl InstallerConfig {
    /// **The daemon's own**, for a Store over `store_root` on `channel`.
    pub fn product(store_root: &Path, channel: &str) -> InstallerConfig {
        let (trust, no_trust_root) = trust_from(
            !koru_models::MODEL_SIGNING_KEYS.is_empty(),
            TrustPolicy::digest_only(),
        );
        InstallerConfig {
            models: ModelPaths::at(models_root(
                koru_env::value(koru_env::name::KORU_READ_MODELS).map(PathBuf::from),
                store_root,
                koru_channel::store_root_of(channel),
                koru_models::paths::local_data_base(),
            )),
            state_dir: store_root.join("reader"),
            catalog: CatalogSource::resolve_from(
                koru_env::value(koru_env::name::KORU_READ_CATALOG),
                DEFAULT_CATALOG_URL,
            ),
            trust,
            no_trust_root,
            source: None,
        }
    }
}

/// The product's downloader: HTTPS, resuming a partial download.
fn http_source() -> Box<dyn ModelSource> {
    match koru_models::HttpSource::new() {
        Ok(source) => Box::new(source),
        Err(error) => {
            tracing::warn!(target: "koru_readings", %error, "the reader cannot be downloaded: no HTTP client");
            Box::new(koru_models::NoNetworkSource)
        }
    }
}

/// **The cache root, by the one rule** — `KORU_READ_MODELS`; a rooted Store's
/// `<store root>/read-models`; the channel-independent default. With no local
/// data directory at all, the Store's own directory rather than a guess.
pub fn models_root(
    overridden: Option<PathBuf>,
    store_root: &Path,
    default_store_root: Option<PathBuf>,
    local_data: Option<PathBuf>,
) -> PathBuf {
    if let Some(path) = overridden {
        return path;
    }
    let rooted = default_store_root.as_deref() != Some(store_root);
    match (rooted, local_data) {
        (false, Some(base)) => base.join("Koru").join("read-models"),
        _ => store_root.join("read-models"),
    }
}

/// **How much a model has to prove, chosen once** — voice's rule
/// (`voice_install::trust_policy`): the pinned key when there is one, else the
/// digest-only arm, which a stable build refuses; that refusal is the
/// `no_trust_root` answer, and the store is built `SIGNED` so even a path that
/// forgot to check would be refused by the model store itself.
pub fn trust_from(
    keys_pinned: bool,
    digest_only: Result<TrustPolicy, ModelError>,
) -> (TrustPolicy, bool) {
    if keys_pinned {
        return (TrustPolicy::SIGNED, false);
    }
    match digest_only {
        Ok(policy) => (policy, false),
        Err(_) => (TrustPolicy::SIGNED, true),
    }
}

/// The catalog entry this build installs, if the catalog lists one it
/// understands.
pub fn choose(catalog: &Catalog) -> Option<CatalogEntry> {
    catalog
        .entries
        .iter()
        .find(|entry| {
            entry.understood()
                && entry.descriptor.engine.as_str() == READER_ENGINE
                && entry.descriptor.tier.as_str() == READER_TIER
        })
        .cloned()
}

/// The reader among what is installed, if any: the newest build of the
/// right engine and tier.
fn pick_installed(mut models: Vec<InstalledModel>) -> Option<InstalledModel> {
    models.retain(|model| {
        model.descriptor.engine.as_str() == READER_ENGINE
            && model.descriptor.tier.as_str() == READER_TIER
    });
    models.sort_by(|a, b| {
        (a.descriptor.version.as_str(), a.descriptor.id.as_str())
            .cmp(&(b.descriptor.version.as_str(), b.descriptor.id.as_str()))
    });
    models.pop()
}

// ------------------------------------------------------------ the Not now flag

#[derive(serde::Serialize, serde::Deserialize)]
struct OfferRecord {
    format: String,
    schema_version: u32,
    declined: bool,
}

/// Read the *Not now* flag. **Total**: anything that is not a well-formed
/// record of ours reads as "not declined", so a damaged file can only ever
/// make Koru ask again, never stay quiet.
pub fn read_declined(dir: &Path) -> bool {
    std::fs::read(dir.join(OFFER_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<OfferRecord>(&bytes).ok())
        .is_some_and(|record| record.format == OFFER_FORMAT && record.declined)
}

/// Write it, atomically: a sibling temporary file, then a rename.
pub fn write_declined(dir: &Path, declined: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let bytes = serde_json::to_vec(&OfferRecord {
        format: OFFER_FORMAT.to_owned(),
        schema_version: 1,
        declined,
    })
    .map_err(std::io::Error::other)?;
    let path = dir.join(OFFER_FILE);
    let temporary = dir.join(format!("{OFFER_FILE}.tmp"));
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, &path)
}

// ------------------------------------------------------------- the thread

/// **Start the installer**: its thread, and the service the routes call.
pub fn start(
    config: InstallerConfig,
    hub: Arc<ReaderHub>,
    inbox: Arc<Inbox>,
    announce: Option<Announce>,
    stop: Arc<AtomicBool>,
) -> (Installer, Option<std::thread::JoinHandle<()>>) {
    hub.set_installing_here(config.no_trust_root);
    // The coordinator holds a lease on the model while the worker has it, so
    // another channel's Store cannot remove it from under a running job.
    let leasing = DiskModelStore::offline(config.models.clone(), koru_voice_consent::READ);
    hub.set_leaser(Arc::new(move |model: &InstalledModel| {
        leasing
            .lease(&model.descriptor.id, &model.descriptor.version)
            .ok()
    }));
    let installer = Installer {
        hub: Arc::clone(&hub),
    };
    let handle = std::thread::Builder::new()
        .name("koru-reader-install".into())
        .spawn(move || {
            let mut thread = Worker::new(config, hub, inbox);
            thread.announce = announce;
            thread.open();
            while !stop.load(Ordering::Acquire) && !thread.inbox.is_stopping() {
                if let Some(command) = thread.hub.next_command(IDLE) {
                    thread.run(command);
                }
            }
        })
        .ok();
    (installer, handle)
}

/// The installer thread's state.
pub(crate) struct Worker {
    state_dir: PathBuf,
    catalog: Result<CatalogSource, ModelError>,
    store: DiskModelStore,
    hub: Arc<ReaderHub>,
    inbox: Arc<Inbox>,
    pub(crate) announce: Option<Announce>,
}

impl Worker {
    pub(crate) fn new(config: InstallerConfig, hub: Arc<ReaderHub>, inbox: Arc<Inbox>) -> Worker {
        let InstallerConfig {
            models,
            state_dir,
            catalog,
            trust,
            no_trust_root: _,
            source,
        } = config;
        Worker {
            state_dir,
            catalog,
            store: DiskModelStore::with_trust(
                models,
                source.unwrap_or_else(http_source),
                trust,
                koru_voice_consent::READ,
            ),
            hub,
            inbox,
            announce: None,
        }
    }

    fn announce(&self, event: impl Into<koru_events::Draft>) {
        if let Some(announce) = &self.announce {
            announce(event.into());
        }
    }

    /// At start: what the person last said, and what is on the disk.
    pub(crate) fn open(&mut self) {
        self.hub.set_declined(read_declined(&self.state_dir));
        self.rescan();
    }

    fn rescan(&self) {
        let installed = self.store.installed().ok().and_then(pick_installed);
        self.hub.set_installed(installed);
        // The coordinator follows the model at its next step.
        self.inbox.queue_dirty();
    }

    pub(crate) fn run(&mut self, command: Command) {
        match command {
            Command::FetchCatalog => self.fetch_catalog(),
            Command::Install(entry) => self.install(&entry),
            Command::Remove => self.remove(),
            Command::SaveDeclined(declined) => {
                if let Err(error) = write_declined(&self.state_dir, declined) {
                    tracing::warn!(target: "koru_readings", %error, "the reader's Not now could not be written");
                }
            }
        }
    }

    fn fetch_catalog(&mut self) {
        let now = koru_store_core::model::Timestamp::now().millis();
        let fetched = self
            .catalog
            .as_ref()
            .map_err(Clone::clone)
            .and_then(CatalogSource::fetch);
        match fetched {
            Ok(catalog) => {
                let entry = choose(&catalog);
                if entry.is_none() {
                    tracing::warn!(target: "koru_readings", "the reader's catalog lists no model this build can use");
                }
                self.hub.set_catalog(
                    CatalogState {
                        state: CatalogStage::Ready,
                        code: None,
                    },
                    entry,
                    now,
                );
            }
            Err(error) => {
                tracing::info!(target: "koru_readings", code = error.code.as_str(), "the reader's catalog could not be read");
                self.hub.set_catalog(
                    CatalogState {
                        state: CatalogStage::Failed,
                        code: Some(error.code.as_str().to_owned()),
                    },
                    None,
                    now,
                );
            }
        }
    }

    /// Record the consent, then install. The record is written first, so a
    /// download never happens that nobody agreed to; a record that will not
    /// write is still this person's answer for this session (voice's rule).
    fn install(&mut self, entry: &CatalogEntry) {
        let cancel = self.hub.arm_cancel();
        // A Stop pressed while this was queued.
        if self
            .hub
            .install_progress()
            .is_some_and(|p| p.stage == InstallStage::Stopping)
        {
            self.hub
                .set_progress(Some(progress(InstallStage::Stopped, None)));
            return;
        }
        let path = koru_voice_consent::READ.path_in(&self.state_dir);
        let now = koru_voice_consent::now_unix();
        let granted = match std::fs::create_dir_all(&self.state_dir)
            .and_then(|()| koru_voice_consent::READ.grant(&path, entry.terms(), now))
        {
            Ok(granted) => granted,
            Err(error) => {
                tracing::warn!(target: "koru_readings", %error, "the reader's consent could not be recorded; installing on this session's answer");
                koru_voice_consent::READ.recorded(entry.terms(), now)
            }
        };
        if self.hub.declined() {
            self.hub.set_declined(false);
        }
        if let Err(error) = write_declined(&self.state_dir, false) {
            tracing::warn!(target: "koru_readings", %error, "the reader's Not now could not be cleared");
        }
        let hub = Arc::clone(&self.hub);
        let mut report = |p: koru_models::InstallProgress| {
            let stage = if cancel.load(Ordering::Acquire) {
                InstallStage::Stopping
            } else {
                match p.stage {
                    koru_models::InstallStage::Resolving => InstallStage::Starting,
                    koru_models::InstallStage::Downloading => InstallStage::Downloading,
                    koru_models::InstallStage::Verifying => InstallStage::Verifying,
                    koru_models::InstallStage::Unpacking
                    | koru_models::InstallStage::Publishing => InstallStage::Installing,
                }
            };
            hub.set_progress(Some(koru_store_wire::reader::InstallProgress {
                stage,
                received: p.received,
                total: p.total,
                code: None,
            }));
        };
        let result =
            self.store
                .install(entry, TrustedKeys::pinned(), &granted, &mut report, &cancel);
        match result {
            Ok(installed) => {
                tracing::info!(
                    target: "koru_readings",
                    model = installed.descriptor.id.as_str(),
                    version = installed.descriptor.version.as_str(),
                    "the reader was installed"
                );
                self.announce(koru_events::ReadingReaderInstalled {
                    size_mb_bucket: koru_events::bucket(koru_voice_consent::megabytes(
                        installed.descriptor.size,
                    )),
                });
                self.hub
                    .set_progress(Some(progress(InstallStage::Installed, None)));
                self.rescan();
            }
            Err(error) if error.code == ModelErrorCode::Cancelled => {
                self.hub
                    .set_progress(Some(progress(InstallStage::Stopped, None)));
            }
            Err(error) => {
                tracing::warn!(target: "koru_readings", code = error.code.as_str(), "the reader could not be installed");
                self.hub.set_progress(Some(progress(
                    InstallStage::Failed,
                    Some(error.code.as_str().to_owned()),
                )));
            }
        }
    }

    /// **Remove**: the worker lets the model go, then the bytes are deleted.
    /// Readings already made are documents and stay.
    fn remove(&mut self) {
        let Some(model) = self.hub.installed() else {
            self.hub
                .set_progress(Some(progress(InstallStage::Removed, None)));
            return;
        };
        self.hub.begin_removal();
        self.inbox.queue_dirty();
        if !self.hub.wait_for_release(RELEASE_PATIENCE) {
            tracing::warn!(target: "koru_readings", "the read worker did not let the reader go in time; removing anyway");
        }
        match self
            .store
            .remove(&model.descriptor.id, &model.descriptor.version)
        {
            Ok(()) => {
                tracing::info!(
                    target: "koru_readings",
                    model = model.descriptor.id.as_str(),
                    version = model.descriptor.version.as_str(),
                    "the reader was removed"
                );
                self.announce(koru_events::ReadingReaderRemoved);
                self.hub
                    .set_progress(Some(progress(InstallStage::Removed, None)));
            }
            Err(error) => {
                tracing::warn!(target: "koru_readings", code = error.code.as_str(), "the reader could not be removed");
                self.hub.set_progress(Some(progress(
                    InstallStage::Failed,
                    Some(error.code.as_str().to_owned()),
                )));
            }
        }
        self.rescan();
    }

    #[cfg(test)]
    pub(crate) fn store(&self) -> &DiskModelStore {
        &self.store
    }
}

// ------------------------------------------------------------- the service

/// **What the routes call.** Every method reads the hub or pushes a command.
#[derive(Clone)]
pub struct Installer {
    hub: Arc<ReaderHub>,
}

impl Installer {
    /// The service over `hub` with no installer thread behind it: a host
    /// that installs nothing answers the state (unavailable, saying why) and
    /// refuses an install with `reader_unavailable`. Also what a test drives
    /// a hub through.
    pub(crate) fn idle(hub: Arc<ReaderHub>) -> Installer {
        Installer { hub }
    }

    fn now() -> i64 {
        koru_store_core::model::Timestamp::now().millis()
    }
}

impl koru_store_api::routes::reader::ReaderService for Installer {
    fn state(&self) -> ReaderState {
        self.hub.want_catalog(Self::now());
        self.hub.state()
    }

    fn install(&self, request: &InstallReader) -> Result<ReaderState, Error> {
        if let Some(why) = self.hub.unavailable()
            && why != koru_store_wire::reader::ReaderUnavailable::WillNotLoad
        {
            return Err(Error::new(
                Code::ReaderUnavailable,
                format!("no reader can be used on this computer: {}", why.as_str()),
            ));
        }
        if self.hub.install_running() {
            return Ok(self.hub.state());
        }
        let Some(entry) = self.hub.offer_entry() else {
            self.hub.want_catalog(Self::now());
            return Err(Error::new(
                Code::ReaderTermsChanged,
                "the reader's catalog has not been read; read the state again",
            ));
        };
        if terms_of(&entry) != request.terms {
            return Err(Error::new(
                Code::ReaderTermsChanged,
                "the catalog now offers other terms than the ones shown",
            ));
        }
        tracing::info!(
            target: "koru_readings",
            from = match request.from {
                koru_store_wire::reader::InstallFrom::Offer => "offer",
                koru_store_wire::reader::InstallFrom::Settings => "settings",
            },
            "the reader is being turned on"
        );
        // The person said yes: whatever a Not now said, it is answered. The
        // thread writes the flag with the consent.
        self.hub.set_declined(false);
        self.hub
            .set_progress(Some(progress(InstallStage::Starting, None)));
        self.hub.send(Command::Install(Box::new(entry)));
        Ok(self.hub.state())
    }

    fn stop(&self) -> ReaderState {
        if let Some(progress_now) = self.hub.install_progress()
            && progress_now.stage.is_running()
            && progress_now.stage != InstallStage::Removing
        {
            self.hub.cancel();
            self.hub
                .set_progress(Some(koru_store_wire::reader::InstallProgress {
                    stage: InstallStage::Stopping,
                    ..progress_now
                }));
        }
        self.hub.state()
    }

    fn remove(&self) -> Result<ReaderState, Error> {
        if self.hub.installed().is_none() && !self.hub.install_running() {
            return Ok(self.hub.state());
        }
        if self.hub.install_running() {
            self.hub.cancel();
        }
        self.hub
            .set_progress(Some(progress(InstallStage::Removing, None)));
        self.hub.send(Command::Remove);
        Ok(self.hub.state())
    }

    fn decline(&self) -> Result<ReaderState, Error> {
        if !self.hub.declined() {
            self.hub.set_declined(true);
            self.hub.send(Command::SaveDeclined(true));
        }
        Ok(self.hub.state())
    }

    fn import_finished(&self, report: &ImportFinished) -> Result<ReaderState, Error> {
        if report.pictures_and_scans > 0 && self.hub.declined() {
            self.hub.set_declined(false);
            self.hub.send(Command::SaveDeclined(false));
        }
        Ok(self.hub.state())
    }
}

#[cfg(test)]
pub(crate) mod tests;
