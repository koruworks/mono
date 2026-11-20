// SPDX-License-Identifier: MPL-2.0

//! The phone's half of the blob transfer engine — Plan B5.
//!
//! **One engine, composed once.** `koru-blob-sync` already runs on the
//! desktop, and `koru-store-server` composes it during the startup a phone
//! already runs (`WorkerConfig::mobile` is the embedded shape). So there is
//! nothing here that decides what a transfer does: the referenced-blob walk,
//! the two state machines, the retry table, the staging file and the
//! announcement are all the engine's. What a *platform* owns is three facts,
//! and this module is those three and nothing else:
//!
//! | Fact | Who holds it on a phone |
//! | --- | --- |
//! | the account's reach into the control plane | this process, not another one (D257) |
//! | whether the network may be used right now | Android's connectivity, through [`NetworkPolicy`] |
//! | that a picture arrived | [`Arrivals`], because there is no room registry (D242) |
//!
//! Plus the wake-ups. **A phone holds no timer of its own while backgrounded**
//! (D244), and this module does not add one: the app's existing account poll
//! is the tick, a Cloud pass runs the blob pass after it, and WorkManager's
//! recovery entry point is [`MobileBlobs::recover`] — a one-shot, not a poll.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use koru_blob_sync::{BlobDownloads, BlobPolicy, TransferHold, Trigger};
use koru_contract::mobile_proto as wire;
use koru_store_core::{WorkspaceId, model::BlobHash};
use koru_store_server::blob_sync::BlobSync;

/// How many arrivals one host remembers.
///
/// **A ring, not a queue that grows.** An arrival is read by whatever surface
/// is on screen and then it is history; a phone that pulled two thousand
/// pictures while nobody was looking must not be holding two thousand records
/// because nothing drained them. Sized for a screenful of documents several
/// times over, which is the most any reader has asked for at once.
const ARRIVAL_MEMORY: usize = 256;

/// This device's network policy, as the platform reports it (D50).
///
/// **Device-wide, because it is a fact about the connection** rather than
/// about a Workspace. The four booleans are exactly what Android's
/// `ConnectivityManager` can say plus the person's own allowance, and the
/// order they are read in is the order they matter: no network beats data
/// saver beats metered.
///
/// **It is read only as a [`BlobPolicy`], and there is deliberately no method
/// here that answers anything about documents.** That is the whole of why
/// document sync continues while blob transfers are paused: the document pass
/// has nothing to ask this value, because the only question it answers is the
/// blob port's. D50 says sync and control traffic continue on a metered
/// network and automatic *blob* transfers pause; adding a second reader here
/// is how that would stop being true.
#[derive(Default)]
pub(crate) struct NetworkPolicy {
    metered: AtomicBool,
    allow_on_metered: AtomicBool,
    low_data: AtomicBool,
    offline: AtomicBool,
}

impl NetworkPolicy {
    /// Replace the whole policy. Whole rather than per field: a platform
    /// reports a connection, and half of one connection's facts beside half of
    /// the previous one's is a state this device was never in.
    pub(crate) fn replace(&self, policy: &wire::BlobPolicy) {
        self.metered.store(policy.metered, Ordering::Relaxed);
        self.allow_on_metered
            .store(policy.allow_on_metered, Ordering::Relaxed);
        self.low_data.store(policy.low_data, Ordering::Relaxed);
        self.offline.store(policy.offline, Ordering::Relaxed);
    }

    /// What is holding automatic transfers, or `None` when nothing is.
    pub(crate) fn hold_now(&self) -> Option<TransferHold> {
        if self.offline.load(Ordering::Relaxed) {
            return Some(TransferHold::Offline);
        }
        // The person's allowance answers both of the paid-connection holds:
        // somebody who said "use cellular for pictures" said it about the
        // connection, and reporting low-data at them afterwards would be the
        // switch not working.
        if self.allow_on_metered.load(Ordering::Relaxed) {
            return None;
        }
        if self.low_data.load(Ordering::Relaxed) {
            return Some(TransferHold::LowData);
        }
        self.metered
            .load(Ordering::Relaxed)
            .then_some(TransferHold::Metered)
    }
}

impl BlobPolicy for NetworkPolicy {
    fn transfers_permitted(&self, _workspace: WorkspaceId) -> bool {
        self.hold_now().is_none()
    }

    fn hold(&self, _workspace: WorkspaceId) -> TransferHold {
        // Only asked when `transfers_permitted` said no, and the race between
        // the two reads resolves the conservative way: a policy that became
        // permissive in between is reported as metered for one pass, and the
        // trigger that made it permissive runs the next one.
        self.hold_now().unwrap_or(TransferHold::Metered)
    }
}

/// One picture becoming durable on this device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Arrival {
    pub(crate) workspace: WorkspaceId,
    pub(crate) hash: String,
    pub(crate) sequence: u64,
    pub(crate) at_millis: i64,
}

/// The §11 announcement, on the path a phone has.
///
/// The desktop's implementation wakes the document's room and completes the
/// window's pending `GET`. Android has neither: its surfaces re-read the Store
/// through the shim, so an arrival is something they **ask** about, and this
/// is what they ask. `sequence` is monotonic for the host's lifetime, so a
/// caller that passes back the last one it saw is told exactly what is new —
/// never nothing it has seen, never missing one it has not.
///
/// **Identity, never bytes** (§11). Nothing here is larger than a hash.
#[derive(Default)]
pub(crate) struct Arrivals {
    log: Mutex<std::collections::VecDeque<Arrival>>,
    sequence: AtomicU64,
    /// Hashes the documents this phone currently has projected refer to.
    /// Answers `watching`, which decides **order** and never whether: a
    /// picture somebody is looking at a placeholder for goes first.
    showing: Mutex<std::collections::BTreeSet<String>>,
}

impl Arrivals {
    /// What the open document views reference now. Replaced wholesale on every
    /// projection, because "which pictures is this phone showing" is a fact
    /// about the current views and not an accumulation.
    pub(crate) fn showing(&self, hashes: impl IntoIterator<Item = String>) {
        *self.showing.lock().unwrap_or_else(|e| e.into_inner()) = hashes.into_iter().collect();
    }

    /// Arrivals after `after`, newest last, optionally for one Workspace, with
    /// the newest sequence this host holds.
    ///
    /// The sequence is returned **whether or not it is in the rows**, so a
    /// caller filtering by Workspace still moves forward rather than asking
    /// for the same window until something it cares about arrives.
    pub(crate) fn since(&self, workspace: Option<WorkspaceId>, after: u64) -> (Vec<Arrival>, u64) {
        let log = self.log.lock().unwrap_or_else(|e| e.into_inner());
        let rows = log
            .iter()
            .filter(|arrival| arrival.sequence > after)
            .filter(|arrival| workspace.is_none_or(|wanted| arrival.workspace == wanted))
            .cloned()
            .collect();
        (rows, self.sequence.load(Ordering::Relaxed))
    }
}

impl BlobDownloads for Arrivals {
    fn watching(&self, _workspace: WorkspaceId, hash: &BlobHash) -> bool {
        self.showing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(hash.as_str())
    }

    fn arrived(&self, workspace: WorkspaceId, hash: &BlobHash) {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let mut log = self.log.lock().unwrap_or_else(|e| e.into_inner());
        log.push_back(Arrival {
            workspace,
            hash: hash.as_str().to_owned(),
            sequence,
            at_millis: crate::now_millis(),
        });
        while log.len() > ARRIVAL_MEMORY {
            log.pop_front();
        }
    }
}

/// What the host holds: the engine the Store worker composed, and the two
/// platform facts installed into it.
#[derive(Clone)]
pub(crate) struct MobileBlobs {
    engine: BlobSync,
    policy: Arc<NetworkPolicy>,
    arrivals: Arc<Arrivals>,
}

impl MobileBlobs {
    pub(crate) fn bind_admission(
        &self,
        runtime: &tokio::runtime::Handle,
        permit: &koru_sync_control::cloud::CloudPermitV2,
        holds: koru_network_iroh::cloud::client_io::reconcile::UploadAdmissionHolds,
        live: koru_blob_sync::LiveAuthorization,
    ) {
        self.engine.bind_admission(runtime, permit, holds, live);
    }

    /// Install this platform's halves into the engine the worker composed.
    ///
    /// `None` when the worker has no engine, which means its read model would
    /// not open — a device that cannot answer *which blobs does this Workspace
    /// have* must not reconcile against a guess.
    pub(crate) fn install(worker: &koru_store_server::WorkerHandle) -> Option<Self> {
        let engine = worker.blob_sync()?.clone();
        let policy = Arc::new(NetworkPolicy::default());
        let arrivals = Arc::new(Arrivals::default());
        engine.set_policy(policy.clone() as Arc<dyn BlobPolicy>);
        engine.observe_arrivals(arrivals.clone() as Arc<dyn BlobDownloads>);
        Some(Self {
            engine,
            policy,
            arrivals,
        })
    }

    pub(crate) fn policy(&self) -> &Arc<NetworkPolicy> {
        &self.policy
    }

    pub(crate) fn arrivals(&self) -> &Arc<Arrivals> {
        &self.arrivals
    }

    /// A local object worker this build may send bytes to, beside the
    /// compiled-in per-region set.
    ///
    /// **The in-process counterpart of `KORU_CLOUD_BLOB_BYTE_ORIGINS`**, for
    /// the same reason the account's reach has one: a phone has no
    /// environment, and a control response naming `http://192.168.1.118:8081/`
    /// is refused by a device that was never told about that stack. The rule
    /// is unchanged and stays in `koru-blob-sync` — loopback, private-range
    /// and link-local only, never a public host — so this adds a way to *say*
    /// the value and no way to widen it.
    pub(crate) fn install_development_origins(
        &self,
        raw: &str,
    ) -> Result<(), koru_blob_sync::OriginRefused> {
        self.engine.set_development_origins(raw)
    }

    // The reader the owed activation-reorder test needs (cloud-platform
    // QUESTIONS.md, 2026-09-22); no test calls it until that harness exists.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn development_origins(&self) -> Option<String> {
        self.engine.development_origins()
    }

    /// The account's reach into the blob control plane, in process.
    ///
    /// When the token *arrives* rather than renews, the Workspaces stamped
    /// *not connected* by passes that ran before it get their pass now.
    pub(crate) fn install_access(
        &self,
        runtime: &tokio::runtime::Handle,
        access: koru_sync_control::cloud::CloudBlobAccessV1,
    ) -> Result<(), &'static str> {
        if self.engine.install_blob_access(access)? {
            self.engine.access_arrived(runtime);
        }
        Ok(())
    }

    pub(crate) fn status(&self, workspace: WorkspaceId) -> koru_blob_sync::BlobTransferStatus {
        self.engine.status(workspace)
    }

    /// The same status as the Store's transfer report — **the function the
    /// desktop's `cloud-sync` route calls**, through the service port the
    /// engine already implements, so a phone's progress row and a desktop's
    /// read one shape made one way.
    pub(crate) fn report(
        &self,
        workspace: WorkspaceId,
    ) -> koru_store_wire::internal::BlobTransferReport {
        koru_store_api::state::BlobTransferStatusService::status(&self.engine, workspace)
    }

    /// Which documents wait on files Cloud does not hold, through the same
    /// port the desktop's `cloud-sync/waiting` route asks. **Read-only**: the
    /// engine lists its `object_missing` parks and moves none of them.
    pub(crate) async fn waiting(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Vec<koru_store_api::state::WaitingFiles>, &'static str> {
        koru_store_api::state::BlobTransferStatusService::waiting(&self.engine, workspace).await
    }

    /// The person's network policy changed. A policy that **became**
    /// permissive is §10's fifth trigger and runs a pass at once; one that
    /// became restrictive is not, and the pass that notices it is the next
    /// one — running one now would only park itself.
    ///
    /// **The passes are spawned, not awaited.** Flipping the cellular switch
    /// is a gesture and must return like one; waiting for it would make a
    /// person hold a settings screen open for as long as their pictures took.
    /// The caller is told the policy is installed, which is what it asked.
    pub(crate) fn policy_replaced(
        &self,
        runtime: &tokio::runtime::Handle,
        workspaces: Vec<WorkspaceId>,
        was_held: bool,
    ) {
        if !(was_held && self.policy.hold_now().is_none()) {
            return;
        }
        for workspace in workspaces {
            let engine = self.engine.clone();
            runtime.spawn(async move {
                engine.reconcile(workspace, Trigger::PolicyPermissive).await;
            });
        }
    }

    /// Somebody is looking at a placeholder (§10's sixth trigger, D50's
    /// override). The blob goes to the front and carries through a metered
    /// pause.
    pub(crate) async fn request(&self, workspace: WorkspaceId, hash: BlobHash) {
        self.engine.request(workspace, &hash);
        self.engine
            .reconcile(workspace, Trigger::Requested(hash))
            .await;
    }

    /// Resume whatever this Workspace's transfers were doing: the staging
    /// sweep, then one pass. What WorkManager calls after process death, and
    /// what a Cloud pass calls when it has just established what this
    /// Workspace holds.
    /// **Clear local files**: every copy Cloud holds leaves this device, and
    /// every reference stays. The engine owns the per-file safety rule.
    pub(crate) async fn release(
        &self,
        workspace: WorkspaceId,
    ) -> Result<koru_blob_sync::ReleaseReport, &'static str> {
        self.engine.release_local(workspace).await
    }

    pub(crate) async fn recover(&self, workspace: WorkspaceId) {
        self.engine.recover(workspace).await;
        self.engine.reconcile(workspace, Trigger::Recovery).await;
    }

    /// **After the document pass, never instead of it.** A pass has just
    /// established what this Workspace holds, which is exactly when the
    /// referenced set is worth walking — and it runs after the coordinator has
    /// its report, so a 40 MB picture never delays what the documents have to
    /// say. This is the same order `cloud_driver::run_session` takes on the
    /// desktop.
    pub(crate) async fn after_document_pass(&self, workspace: WorkspaceId) {
        self.engine.recover(workspace).await;
        self.engine
            .reconcile(workspace, Trigger::MappingChanged)
            .await;
    }

    /// **The document half of coverage**, from the pass that established it,
    /// forwarded to the one engine both platforms share — so the phone and the
    /// desktop cannot come to different conclusions about the same Workspace.
    /// The claim itself is computed by the reconciliation that follows.
    pub(crate) fn expect_documents(
        &self,
        workspace: WorkspaceId,
        incarnation: koru_device::cloud::CloudId,
        documents: koru_blob_sync::CoveredDocuments,
        authorization: koru_blob_sync::LiveAuthorization,
    ) {
        self.engine
            .expect_documents(workspace, incarnation, documents, authorization);
    }
}

/// The blob transfer status, as the contract's §10 message.
pub(crate) fn status_message(
    workspace: WorkspaceId,
    status: &koru_blob_sync::BlobTransferStatus,
) -> wire::BlobTransferStatus {
    wire::BlobTransferStatus {
        workspace_id: workspace.to_string(),
        referenced: status.referenced,
        present: status.present,
        durable: status.durable,
        uploading: status.uploading,
        downloading: status.downloading,
        missing: status.missing,
        bytes_complete: status.bytes_complete,
        bytes_total: status.bytes_total,
        pending_reason: status.pending_reason.unwrap_or_default().to_owned(),
        last_failure_code: status.last_failure_code.unwrap_or_default().to_owned(),
        // **Two numbers, not one.** A phone that merged them would tell
        // somebody their notes had not synced when what had not synced was a
        // photograph. `coverage_state` empty means this device has not
        // computed a claim yet, which is not the same as nothing being owed —
        // and `current` is the only field a surface may turn into a tick.
        coverage_state: status
            .coverage
            .as_ref()
            .map(|coverage| coverage.state.as_str().to_owned())
            .unwrap_or_default(),
        current: status.is_current(),
        documents_required: status
            .coverage
            .as_ref()
            .map(|coverage| coverage.documents_required)
            .unwrap_or_default(),
        documents_missing: status
            .coverage
            .as_ref()
            .map(|coverage| coverage.documents_missing)
            .unwrap_or_default(),
        blobs_missing: status
            .coverage
            .as_ref()
            .map(|coverage| coverage.blobs_missing)
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests;
