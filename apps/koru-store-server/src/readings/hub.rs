// SPDX-License-Identifier: MPL-2.0

//! **What the reader's installer, the coordinator, the search answer and the
//! `/v1/readings/reader` routes share — in memory, behind one short lock**
//! (OCR Plan 2.4).
//!
//! ```text
//!  installer thread ──writes──► installed model, catalog offer, progress, Not now
//!  coordinator      ──writes──► per-workspace counts, machine totals, the worker
//!                   ◄─reads───  which model to launch the worker with
//!  search answer    ──reads───► the counts for the query's workspaces, the offer
//!  routes           ──reads───► ReaderState; commands go to the installer
//! ```
//!
//! **A read here is a read of memory.** Every value is published by a thread
//! that answers nobody — the installer after it touched the disk or the
//! network, the coordinator after it recomputed its queue — so the search
//! request path and the routes never wait on a download, a worker, the Store
//! lock or a recompute. The lock is held to copy a few integers.
//!
//! ## Installed state is derived, never stored
//!
//! [`ReaderHub::installed`] is what the installer last found **on the disk**
//! (ADR-251's rule: a flag beside the consent record would answer *installed*
//! for a cache another channel had pruned). It is re-asked at start, after an
//! install and after a removal.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use koru_models::{CatalogEntry, InstalledModel};
use koru_store_core::ids::WorkspaceId;
use koru_store_wire::reader::{
    CatalogStage, CatalogState, InstallProgress, InstallStage, InstalledReader,
    MachineReadingCounts, ReaderState, ReaderTerms, ReaderUnavailable,
};
use koru_store_wire::search::ReadingCounts;

/// One workspace's counts, as the coordinator last recomputed them. The
/// search answer sums these over a query's workspaces; nothing else is
/// consulted on that path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScopeCounts {
    pub total: u64,
    pub unread: u64,
    pub needs_reader: u64,
    pub not_here: u64,
}

/// What the coordinator knows about the worker, for the offer and the state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkerKnowledge {
    /// What the worker's `Status` said about its build, once one was asked.
    pub built: Option<bool>,
    /// The worker could not be started or asked at all.
    pub missing: bool,
    /// The worker is recognizing now: a model given, and its status said so.
    pub recognizing: bool,
    /// The installed model failed to load twice; see
    /// [`ReaderUnavailable::WillNotLoad`].
    pub will_not_load: bool,
}

/// A model the worker is launched with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInUse {
    pub installed: InstalledModel,
}

impl ModelInUse {
    pub fn dir(&self) -> PathBuf {
        self.installed.root.clone()
    }

    /// The token written into a reading recognized with it: the descriptor's
    /// signed `engine` (`pp-ocr`), which is the adapter's reader token.
    pub fn reader(&self) -> Option<koru_read::ReaderId> {
        koru_read::ReaderId::parse(self.installed.descriptor.engine.as_str())
    }
}

/// What the installer thread is asked to do.
#[derive(Clone, Debug)]
pub enum Command {
    /// Read the reader's catalog, to know the offer's terms.
    FetchCatalog,
    /// Record the consent for this entry and install it.
    Install(Box<CatalogEntry>),
    /// Remove the installed model from disk.
    Remove,
    /// Write the *Not now* flag as it now stands.
    SaveDeclined(bool),
}

#[derive(Default)]
struct State {
    // ---- the installer's
    installed: Option<InstalledModel>,
    /// Moves whenever the model the worker should have changes — an install,
    /// a removal starting, a removal that failed and left it.
    model_revision: u64,
    catalog: CatalogState,
    /// When the catalog last failed, epoch millis, so a retry is not a
    /// request per keystroke.
    catalog_failed_at: Option<i64>,
    offer: Option<CatalogEntry>,
    install: Option<InstallProgress>,
    declined: bool,
    /// This build may install no model (a stable build with no key).
    no_trust_root: bool,
    /// The installer runs at all.
    installing_here: bool,
    // ---- the coordinator's
    coordinated: bool,
    counted: bool,
    acked_revision: u64,
    per_workspace: HashMap<WorkspaceId, ScopeCounts>,
    machine: MachineReadingCounts,
    worker: WorkerKnowledge,
    probe_wanted: bool,
}

/// Takes a lease on an installed model, so nothing prunes or removes it while
/// the worker has it — another channel's Store included. `None` when the
/// model is no longer there.
pub type Leaser = dyn Fn(&InstalledModel) -> Option<Box<dyn koru_models::ModelLease>> + Send + Sync;

/// **The shared state.** See the module note.
#[derive(Default)]
pub struct ReaderHub {
    state: Mutex<State>,
    leaser: Mutex<Option<Arc<Leaser>>>,
    /// The running install's stop flag. Replaced at each install, so a
    /// **Stop** reaches the download that is running and no other.
    cancel: Mutex<Arc<AtomicBool>>,
    commands: Mutex<VecDeque<Command>>,
    wake: Condvar,
}

/// How long a failed catalog read is left before another is tried.
pub const CATALOG_RETRY_MILLIS: i64 = 60_000;

impl ReaderHub {
    pub fn new() -> Arc<ReaderHub> {
        Arc::new(ReaderHub::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ------------------------------------------------------------ commands

    pub fn send(&self, command: Command) {
        self.commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(command);
        self.wake.notify_all();
    }

    /// The installer thread's wait for its next command, at most `patience`.
    pub fn next_command(&self, patience: std::time::Duration) -> Option<Command> {
        let queue = self.commands.lock().unwrap_or_else(|e| e.into_inner());
        let (mut queue, _) = self
            .wake
            .wait_timeout_while(queue, patience, |q| q.is_empty())
            .unwrap_or_else(|e| e.into_inner());
        queue.pop_front()
    }

    /// A fresh stop flag for the install about to start.
    pub fn arm_cancel(&self) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        *self.cancel.lock().unwrap_or_else(|e| e.into_inner()) = Arc::clone(&flag);
        flag
    }

    /// **Stop**: the running download sees it between chunks.
    pub fn cancel(&self) {
        self.cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .store(true, Ordering::Release);
    }

    // ----------------------------------------------------------- installer

    pub fn set_leaser(&self, leaser: Arc<Leaser>) {
        *self.leaser.lock().unwrap_or_else(|e| e.into_inner()) = Some(leaser);
    }

    /// A lease on `model` for the coordinator: `Some(Some(lease))`, or
    /// `Some(None)` on a host that leases nothing (a test), or `None` when
    /// the model is gone.
    #[allow(clippy::option_option)]
    pub fn lease(
        &self,
        model: &InstalledModel,
    ) -> Option<Option<Box<dyn koru_models::ModelLease>>> {
        let leaser = self
            .leaser
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match leaser {
            None => Some(None),
            Some(leaser) => leaser(model).map(Some),
        }
    }

    pub fn set_installing_here(&self, no_trust_root: bool) {
        let mut state = self.lock();
        state.installing_here = true;
        state.no_trust_root = no_trust_root;
    }

    /// What the disk says is installed. Moves the model revision when it
    /// differs from what was known.
    pub fn set_installed(&self, installed: Option<InstalledModel>) {
        let mut state = self.lock();
        if state.installed != installed {
            state.installed = installed;
            state.model_revision += 1;
        }
    }

    /// A removal is starting: the worker lets the model go first.
    pub fn begin_removal(&self) {
        let mut state = self.lock();
        state.installed = None;
        state.model_revision += 1;
        state.install = Some(progress(InstallStage::Removing, None));
    }

    pub fn installed(&self) -> Option<InstalledModel> {
        self.lock().installed.clone()
    }

    pub fn set_catalog(&self, catalog: CatalogState, offer: Option<CatalogEntry>, now: i64) {
        let mut state = self.lock();
        state.catalog_failed_at = (catalog.state == CatalogStage::Failed).then_some(now);
        if catalog.state == CatalogStage::Ready || offer.is_some() {
            state.offer = offer;
        }
        state.catalog = catalog;
    }

    pub fn offer_entry(&self) -> Option<CatalogEntry> {
        self.lock().offer.clone()
    }

    pub fn set_progress(&self, install: Option<InstallProgress>) {
        self.lock().install = install;
    }

    pub fn install_progress(&self) -> Option<InstallProgress> {
        self.lock().install.clone()
    }

    pub fn install_running(&self) -> bool {
        self.lock()
            .install
            .as_ref()
            .is_some_and(|p| p.stage.is_running())
    }

    pub fn set_declined(&self, declined: bool) {
        self.lock().declined = declined;
    }

    pub fn declined(&self) -> bool {
        self.lock().declined
    }

    /// Ask for the catalog if nothing has been read, or the last read failed
    /// long enough ago, and nothing says it would be pointless. Returns
    /// whether it asked. Called from a request; only pushes a command.
    pub fn want_catalog(&self, now: i64) -> bool {
        let wanted = {
            let mut state = self.lock();
            let stale = match state.catalog.state {
                CatalogStage::Unread => true,
                CatalogStage::Failed => state
                    .catalog_failed_at
                    .is_none_or(|at| now.saturating_sub(at) >= CATALOG_RETRY_MILLIS),
                CatalogStage::Reading | CatalogStage::Ready => false,
            };
            let pointless = state.installing_here
                && matches!(
                    unavailable(&state),
                    Some(
                        ReaderUnavailable::NoTrustRoot
                            | ReaderUnavailable::NoWorker
                            | ReaderUnavailable::WorkerMissing
                            | ReaderUnavailable::NotBuilt
                    )
                );
            let go = state.installing_here && stale && !pointless;
            if go {
                state.catalog = CatalogState {
                    state: CatalogStage::Reading,
                    code: None,
                };
            }
            go
        };
        if wanted {
            self.send(Command::FetchCatalog);
        }
        wanted
    }

    /// How long the installer waits for the worker to let a model go before
    /// it removes it anyway (and is refused `leased` if it is still held).
    pub fn wait_for_release(&self, patience: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + patience;
        loop {
            {
                let state = self.lock();
                if !state.coordinated || state.acked_revision >= state.model_revision {
                    return true;
                }
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    // --------------------------------------------------------- coordinator

    /// A coordinator runs and will publish counts and follow the model.
    pub fn attach_coordinator(&self) {
        self.lock().coordinated = true;
    }

    /// The model the worker should be launched with, and its revision.
    pub fn wanted_model(&self) -> (u64, Option<ModelInUse>) {
        let state = self.lock();
        (
            state.model_revision,
            state
                .installed
                .clone()
                .map(|installed| ModelInUse { installed }),
        )
    }

    /// The coordinator has launched (or let go of) the model of `revision`.
    pub fn ack_model(&self, revision: u64) {
        let mut state = self.lock();
        state.acked_revision = state.acked_revision.max(revision);
    }

    pub fn publish_counts(
        &self,
        per_workspace: HashMap<WorkspaceId, ScopeCounts>,
        machine: MachineReadingCounts,
    ) {
        let mut state = self.lock();
        state.per_workspace = per_workspace;
        state.machine = machine;
        state.counted = true;
    }

    pub fn publish_worker(&self, worker: WorkerKnowledge) {
        self.lock().worker = worker;
    }

    /// Something asked what this computer can do, and the coordinator has
    /// not asked the worker yet. Taken (and cleared) by the coordinator.
    pub fn take_probe_wanted(&self) -> bool {
        std::mem::take(&mut self.lock().probe_wanted)
    }

    // ------------------------------------------------------------- readers

    /// **The counts for a query's workspaces**, from what the coordinator last
    /// published — never a recompute. `scope` is the resolved list of the
    /// query's workspaces (`None`: every workspace the coordinator counted),
    /// with `excluded` taken out. `None` when nothing has been counted on this
    /// host, which a surface reads as "say nothing".
    pub fn reading_counts(
        &self,
        scope: Option<&[WorkspaceId]>,
        excluded: &[WorkspaceId],
    ) -> Option<ReadingCounts> {
        let state = self.lock();
        if !state.counted {
            return None;
        }
        let mut sum = ScopeCounts::default();
        let mut add = |counts: &ScopeCounts| {
            sum.total += counts.total;
            sum.unread += counts.unread;
            sum.needs_reader += counts.needs_reader;
            sum.not_here += counts.not_here;
        };
        match scope {
            Some(workspaces) => {
                for workspace in workspaces {
                    if excluded.contains(workspace) {
                        continue;
                    }
                    if let Some(counts) = state.per_workspace.get(workspace) {
                        add(counts);
                    }
                }
            }
            None => {
                for (workspace, counts) in &state.per_workspace {
                    if !excluded.contains(workspace) {
                        add(counts);
                    }
                }
            }
        }
        Some(ReadingCounts {
            unread: sum.unread,
            total: sum.total,
            needs_reader: sum.needs_reader,
            not_here: sum.not_here,
            offer: offer(&state, sum.needs_reader),
        })
    }

    /// The whole state, for `GET /v1/readings/reader`.
    pub fn state(&self) -> ReaderState {
        let mut state = self.lock();
        if state.worker.built.is_none() && !state.worker.missing && state.coordinated {
            state.probe_wanted = true;
        }
        ReaderState {
            installed: state.installed.as_ref().map(|model| InstalledReader {
                model: model.descriptor.id.as_str().to_owned(),
                version: model.descriptor.version.as_str().to_owned(),
                download_bytes: model.descriptor.size,
                licence: model.descriptor.licence.as_str().to_owned(),
                attribution: model.descriptor.attribution.clone(),
            }),
            unavailable: unavailable(&state),
            offer: state.offer.as_ref().map(terms_of),
            catalog: state.catalog.clone(),
            install: state.install.clone(),
            recognizing: state.installed.is_some() && state.worker.recognizing,
            offer_declined: state.declined,
            counts: state.machine.clone(),
        }
    }

    /// Why no reader can be used here, if nothing can.
    pub fn unavailable(&self) -> Option<ReaderUnavailable> {
        unavailable(&self.lock())
    }
}

/// The terms a person is shown, from a catalog entry's signed descriptor.
pub fn terms_of(entry: &CatalogEntry) -> ReaderTerms {
    let terms = entry.terms();
    ReaderTerms {
        model: terms.model,
        version: terms.version,
        licence: terms.licence,
        attribution: terms.attribution,
        download_bytes: terms.download_bytes,
        languages: entry
            .descriptor
            .languages
            .iter()
            .map(|language| language.as_str().to_owned())
            .collect(),
    }
}

pub fn progress(stage: InstallStage, code: Option<String>) -> InstallProgress {
    InstallProgress {
        stage,
        received: 0,
        total: 0,
        code,
    }
}

fn unavailable(state: &State) -> Option<ReaderUnavailable> {
    if !state.installing_here || !state.coordinated {
        return Some(ReaderUnavailable::NoWorker);
    }
    if state.no_trust_root {
        return Some(ReaderUnavailable::NoTrustRoot);
    }
    if state.worker.missing {
        return Some(ReaderUnavailable::WorkerMissing);
    }
    if state.worker.built == Some(false) {
        return Some(ReaderUnavailable::NotBuilt);
    }
    if state.installed.is_some() && state.worker.will_not_load {
        return Some(ReaderUnavailable::WillNotLoad);
    }
    if state.installed.is_none()
        && state.catalog.state == CatalogStage::Ready
        && state.offer.is_none()
    {
        return Some(ReaderUnavailable::NoModel);
    }
    None
}

/// **Whether a surface should offer the reader now** (TECHNICAL §10): it is
/// not installed, something in scope waits for it on this computer, the
/// person has not said *Not now* since the last import that brought a
/// picture, and nothing says this computer could not use it.
fn offer(state: &State, needs_reader: u64) -> bool {
    state.installed.is_none() && needs_reader > 0 && !state.declined && unavailable(state).is_none()
}

#[cfg(test)]
mod tests;
