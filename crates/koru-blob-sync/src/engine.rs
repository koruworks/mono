// SPDX-License-Identifier: MPL-2.0

//! Event-driven reconciliation.
//!
//! **Never a timer, and never a full walk per keystroke.** The engine runs on
//! exactly the triggers the plan's §10 lists and on nothing else:
//!
//! | Trigger | Who raises it |
//! | --- | --- |
//! | [`Trigger::Enabled`] | Cloud or peer enablement for the Workspace |
//! | [`Trigger::DocumentsChanged`] | a **durable** document change whose extracted hash set changed |
//! | [`Trigger::MappingChanged`] | a Cloud mapping or peer inventory change |
//! | [`Trigger::PolicyPermissive`] | network policy becoming permissive |
//! | [`Trigger::Recovery`] | foreground or startup recovery |
//! | [`Trigger::Requested`] | an explicit request for a visible missing blob |
//!
//! The third column of §10 is the one that is easy to get wrong, so it is not
//! left to a caller's discipline: [`BlobEngine::document_changed`] holds the
//! per-document hash sets and answers *whether the Workspace's set moved*. A
//! host that calls it on every durable commit — which the desktop does — gets
//! `false` for every commit that is only text, and the engine is never
//! entered at all. That is what makes typing free: not a fast walk, **no**
//! walk.
//!
//! One transfer runs at a time. The link is the bottleneck rather than the
//! parallelism, a queue that drains in order is a queue whose progress a
//! person can read, and one chunk in flight is what makes the memory bound
//! a fact rather than an average.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use koru_device::cloud::CloudId;
use koru_store_core::{DocumentId, WorkspaceId, model::BlobHash};

use crate::{
    control::BlobControlPlane,
    coverage::{
        self, BlobPresence, CoverageReport, CoverageRequest, DocumentSample, SampledDocument,
    },
    download::{self, DownloadOutcome},
    object::BlobObjectPlane,
    ports::{BlobDownloads, BlobPorts, BlobStorePort, TransferError, TransferHold},
    referenced::ReferencedBlob,
    retry::{Backoff, Disposition},
    staging::StagingArea,
    status::{BlobTransferStatus, Direction, TransferPath},
    upload::{self, UploadOutcome},
};

/// Who answers *is the authorization that admitted this pass still the live
/// one*, at the moment coverage is about to be committed.
///
/// A callback rather than a value read once, because the whole point of
/// coverage's fourth step is that the answer may have changed since the sample
/// was taken: a revocation, a replaced permit, a Workspace dropped and
/// re-provisioned under a new incarnation. It answers with the **live Workspace
/// incarnation**, or `None` when this device no longer holds an authorization
/// for the Workspace at all.
pub type LiveAuthorization = Arc<dyn Fn(WorkspaceId) -> Option<CloudId> + Send + Sync>;

/// What [`BlobEngine::release_local`] did, in numbers a person can read.
///
/// `kept` is the count that matters: every one of those is a file this device
/// is the only place for, as far as Cloud has confirmed, and a surface that
/// summed it into "freed" would be reporting a deletion that did not happen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReleaseReport {
    /// Live hashes the canonical walk found.
    pub referenced: u32,
    /// Copies that left this device.
    pub released: u32,
    pub bytes_freed: u64,
    /// Held here and **not** confirmed durable on Cloud at the moment of
    /// asking, or confirmed at a different size. Untouched.
    pub kept: u32,
    /// Referenced and not held here: nothing to free.
    pub not_held: u32,
    /// Set when the control plane stopped answering part way. The counts
    /// above are what happened before it did.
    pub failure_code: Option<String>,
}

/// Why a pass is running. Closed on purpose: a trigger that is not on this
/// list is a trigger §10 did not approve, and adding one is a plan change
/// rather than a call site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// Cloud or peer enablement for this Workspace.
    Enabled,
    /// A durable document change whose extracted hash set changed. Raised by
    /// [`BlobEngine::document_changed`] answering `true`, never by a caller
    /// deciding for itself.
    DocumentsChanged,
    /// A Cloud mapping or peer inventory change: something else now holds, or
    /// no longer holds, bytes this Workspace wants.
    MappingChanged,
    /// Network policy became permissive.
    PolicyPermissive,
    /// Foreground or startup recovery.
    Recovery,
    /// Somebody is looking at a placeholder. Clears the blob's park and puts
    /// it at the front.
    Requested(BlobHash),
}

impl Trigger {
    /// Whether this trigger clears parks.
    ///
    /// A park says "this will not change on its own". Enablement, a mapping
    /// change, policy and an explicit request are each a statement that
    /// something *did* change, so they unpark; a document change and a
    /// recovery pass are not, and re-trying a fenced account on every commit
    /// is how a device learns to hammer a service.
    fn clears_parks(&self) -> bool {
        matches!(
            self,
            Self::Enabled | Self::MappingChanged | Self::PolicyPermissive | Self::Requested(_)
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::DocumentsChanged => "documents_changed",
            Self::MappingChanged => "mapping_changed",
            Self::PolicyPermissive => "policy_permissive",
            Self::Recovery => "recovery",
            Self::Requested(_) => "requested",
        }
    }
}

/// What one pass did. In memory, like the document pass's `Outcome`: a count
/// that survived a restart would start to look like a durable verdict about
/// somebody's picture.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub uploaded: u32,
    pub downloaded: u32,
    /// Referenced, absent here, and **nobody asked**: left on Cloud (D260).
    /// Not a park and not a failure — the ordinary state of a file on a
    /// device that has not opened it.
    pub deferred: u32,
    pub already_durable: u32,
    pub already_local: u32,
    /// Blobs this pass did not carry, with the reason.
    pub parked: Vec<(BlobHash, &'static str)>,
    /// The pass stopped: the refusal was about the session, not one blob.
    pub session_refusal: Option<&'static str>,
    /// …and when that refusal was a **hold**, which one.
    ///
    /// Carried as a value rather than left for a caller to recognise from the
    /// code, because the first thing every caller does with it is decide
    /// whether this is news. A phone on a train reports a metered pause on
    /// every wake; a line that appears whether or not anything happened is a
    /// line people learn to scroll past.
    pub session_hold: Option<TransferHold>,
    /// The pass never ran, because the debounce had not elapsed or nothing
    /// had moved.
    pub skipped: bool,
}

/// **One file, as a surface explaining a selection needs it** (D259 §4).
///
/// The two halves a person actually asks about, kept apart rather than
/// collapsed into one word: what **this device** holds, and what **Cloud**
/// holds. A single "state" would have to choose which of those it meant, and
/// *on this laptop only* and *on Cloud only* are the two answers a person
/// most needs told apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobFileFacts {
    pub hash: BlobHash,
    /// The local record — bytes present **and** this Workspace associated,
    /// with the size, media type and filename that record carries. `None` is
    /// *not here*, or a Store that would not answer.
    pub local: Option<crate::ports::LocalBlob>,
    /// What the last coverage pass found on Cloud. `None` is *nothing has
    /// measured this*, which is not `Missing`.
    pub cloud: Option<crate::coverage::BlobPresence>,
    /// Why blob transfer is holding this Workspace, if it is.
    pub hold: Option<&'static str>,
}

/// **One document's own Cloud state**, as [`BlobEngine::document_coverage`]
/// answers it: whether Cloud holds the document, and how many of the files it
/// references Cloud holds durably.
///
/// Two `Option`s, because each half has its own way of not being known and
/// neither is zero.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentCoverage {
    /// **Cloud holds this document as it is now.** `None` when no coverage
    /// has been computed for the Workspace since the process started — which
    /// is *nobody has measured*, not *no*. `Some(false)` covers every other
    /// way of not being held: never carried, carried at a state the document
    /// has since moved past, or not projected yet.
    pub synced: Option<bool>,
    /// The files this document references. `None` when the read model does
    /// not know its references — not listed, or listed and never projected —
    /// which a surface must not draw as *no files*.
    pub files: Option<DocumentFiles>,
}

/// One document's referenced files, split the way a person reads them.
///
/// **Synced means durable on Cloud with a verified size**, the rule coverage
/// counts by; an upload in flight is not synced. An image is a reference whose
/// recorded media type is `image/*`; everything else — an archive, a PDF, a
/// reference whose type nobody recorded, one whose address will not parse —
/// is a file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DocumentFiles {
    pub images_total: u32,
    pub images_synced: u32,
    pub files_total: u32,
    pub files_synced: u32,
}

/// **One document waiting on bytes Cloud does not hold yet**, as
/// [`BlobEngine::waiting`] answers it.
///
/// The addresses are for the Store to resolve — whose device first held
/// them — and never for a person to read: a surface shows how many there
/// are, not what they are called.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitingDocument {
    pub document: DocumentId,
    /// Each once, however many nodes in the document name it.
    pub blobs: Vec<BlobHash>,
}

/// Why one blob is not being carried, and whether time alone will change it.
#[derive(Clone, Debug)]
struct Park {
    code: &'static str,
    /// `None` means "until a trigger says otherwise". A park with a deadline
    /// is a backoff; a park without one is a statement.
    until: Option<Instant>,
}

#[derive(Default)]
struct WorkspaceState {
    /// Per-document blob sets, so a durable commit that changed only text is
    /// answered without touching the Workspace.
    documents: BTreeMap<DocumentId, BTreeSet<String>>,
    parks: BTreeMap<String, Park>,
    backoff: BTreeMap<String, Backoff>,
    /// Blobs somebody asked for out loud. They go first and they are unparked.
    requested: BTreeSet<String>,
    last_pass: Option<Instant>,
    status: BlobTransferStatus,
    /// **A person pressed pause for this Workspace** (D256).
    ///
    /// In memory, beside the parks and the pass record, because that is what
    /// it is: a statement about what this process should be doing now.
    /// Deliberately not durable — a durable pause would need catalog storage
    /// of its own, and one that outlived a restart with nothing on screen
    /// explaining it is the silent pause §13 has already paid for once. What
    /// a person chooses *durably* is the transport mode.
    paused: bool,
    /// **What Cloud held for each referenced address, as the last coverage
    /// computation read it** (D259 §4).
    ///
    /// [`crate::coverage::compute`] already asks the control plane this, once
    /// per referenced blob, and then throws the answers away having counted
    /// them. Keeping them is what lets a surface explain **one** file —
    /// *this picture is on Cloud, that archive is still going* — with no
    /// second subsystem, no second walk and no request per selection. It is a
    /// projection of the status the engine already has, which is the whole of
    /// what D259 §4 asked for.
    ///
    /// **Not authority, and knowingly a moment old**: it is what the last
    /// coverage pass found, the same vintage as `status.coverage` beside it.
    /// A caller that needs a guarantee reads coverage; a caller that needs a
    /// sentence about one file reads this.
    presence: BTreeMap<String, crate::coverage::BlobPresence>,
    /// **Which documents the last coverage computation found Cloud holding**,
    /// each at the state it judged (the per-document half of
    /// `documents_missing`).
    ///
    /// Kept for [`presence`]'s reason: coverage already decides this per
    /// document and then counts. The state is what makes it safe to read a
    /// moment later — Cloud does not lose a state it holds, so a document read
    /// again at the same state is still held, and one read at any other state
    /// has moved since and is not.
    ///
    /// [`presence`]: Self::presence
    held: BTreeMap<DocumentId, crate::coverage::DocumentState>,
}

/// The engine.
pub struct BlobEngine {
    ports: BlobPorts,
    control: Arc<dyn BlobControlPlane>,
    objects: Arc<dyn BlobObjectPlane>,
    staging: StagingArea,
    state: Mutex<BTreeMap<WorkspaceId, WorkspaceState>>,
    /// One pass per Workspace at a time.
    ///
    /// **Not an optimisation.** Two passes over one Workspace would both find
    /// the same missing blob, both open the same staging file and both append
    /// at what each believed the length was — which is a hole the final hash
    /// would blame on the network. The second caller waits and then finds the
    /// work done, which is also the honest answer to "somebody asked for this
    /// picture while it was already being fetched".
    passes: Mutex<BTreeMap<WorkspaceId, Arc<tokio::sync::Mutex<()>>>>,
    walks: AtomicU64,
    /// The chunk this device uses when it has not been told one. Allocation
    /// returns the real number; this is only the download's first guess, and
    /// it is the contract's own constant rather than a second one.
    chunk: u64,
    debounce: Duration,
}

impl BlobEngine {
    pub fn new(
        ports: BlobPorts,
        control: Arc<dyn BlobControlPlane>,
        objects: Arc<dyn BlobObjectPlane>,
    ) -> Arc<Self> {
        let staging = StagingArea::new(ports.store.staging_root());
        Arc::new(Self {
            ports,
            control,
            objects,
            staging,
            state: Mutex::new(BTreeMap::new()),
            passes: Mutex::new(BTreeMap::new()),
            walks: AtomicU64::new(0),
            chunk: koru_cloud_blob::CHUNK,
            debounce: crate::CHANGE_DEBOUNCE,
        })
    }

    /// A test needs to watch a debounce rather than wait one out.
    pub fn with_debounce(mut self: Arc<Self>, debounce: Duration) -> Arc<Self> {
        if let Some(engine) = Arc::get_mut(&mut self) {
            engine.debounce = debounce;
        }
        self
    }

    pub(crate) fn store(&self) -> &Arc<dyn BlobStorePort> {
        &self.ports.store
    }
    pub(crate) fn control(&self) -> &Arc<dyn BlobControlPlane> {
        &self.control
    }
    pub(crate) fn objects(&self) -> &Arc<dyn BlobObjectPlane> {
        &self.objects
    }
    pub(crate) fn downloads(&self) -> &Arc<dyn BlobDownloads> {
        &self.ports.downloads
    }
    pub(crate) fn staging(&self) -> &StagingArea {
        &self.staging
    }
    pub(crate) fn chunk(&self) -> u64 {
        self.chunk
    }

    pub(crate) fn peers(&self) -> &Arc<dyn crate::ports::PeerBlobs> {
        &self.ports.peers
    }

    /// The last transfer used a direct peer rather than Cloud. A status field
    /// that only ever said `cloud` would be one nobody could use to answer
    /// *"why is my egress bill what it is"*.
    pub(crate) fn used_peer_path(&self, workspace: WorkspaceId) {
        self.tally(workspace, |status| {
            status.path = Some(TransferPath::PeerDirect);
        });
    }

    /// **§6's fourth point**, asked rather than remembered.
    ///
    /// Called at the top of a pass and again inside each allocate and each
    /// download, because the answer can change between them: a restrictive
    /// choice closes admission in memory before its durable write, and the
    /// whole reason for that order is that an in-flight transfer stops.
    pub(crate) async fn admit_cloud(&self, workspace: WorkspaceId) -> Result<(), TransferError> {
        self.ports
            .modes
            .admit(workspace, koru_store_core::TransportPath::Cloud)
            .await
    }

    /// The same question for the peer arm of a download (§6's second point).
    /// A refusal here is not an error: it removes the peer from the candidate
    /// list and Cloud is the next candidate.
    pub(crate) async fn peer_admitted(&self, workspace: WorkspaceId) -> bool {
        self.ports
            .modes
            .admit(workspace, koru_store_core::TransportPath::Peer)
            .await
            .is_ok()
    }

    /// How many canonical walks this engine has run. The number the
    /// "typing performs no full-Workspace walk" test watches.
    /// The Workspaces whose last pass ended on `code` and nothing has run since.
    ///
    /// For the one code that is about the account and not the Workspace: a
    /// pass refused for want of Cloud access stamps every Workspace it ran
    /// over, and only a later pass over that same Workspace lifts the stamp.
    /// On a phone the recovery passes at start ran two seconds before the
    /// first token arrived and stamped all thirteen; the ones whose session
    /// did not come round again kept reading *not connected* while the token
    /// sat installed. Whoever installs the token asks this and runs those
    /// passes.
    pub fn workspaces_pending(&self, code: &str) -> Vec<WorkspaceId> {
        self.state()
            .iter()
            .filter(|(_, entry)| entry.status.pending_reason == Some(code))
            .map(|(workspace, _)| *workspace)
            .collect()
    }

    pub fn walks(&self) -> u64 {
        self.walks.load(Ordering::Relaxed)
    }

    /// **§10's second trigger, with its qualifier enforced.**
    ///
    /// A durable document change arrives here with the hash set that document
    /// now references. The answer is whether the *Workspace's* set moved, and
    /// only a `true` is a reason to reconcile. A commit that changed a
    /// paragraph carries the same set and is answered without a walk, without
    /// a lock on the Store and without a byte on the network.
    pub fn document_changed(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
        hashes: &[BlobHash],
    ) -> bool {
        let next: BTreeSet<String> = hashes.iter().map(|hash| hash.as_str().to_owned()).collect();
        let mut state = self.state();
        let workspace = state.entry(workspace).or_default();
        match workspace.documents.get(&document) {
            Some(previous) if *previous == next => false,
            _ => {
                workspace.documents.insert(document, next);
                true
            }
        }
    }

    /// A document left the Workspace. Its references are no longer this
    /// Workspace's, which is a change like any other.
    pub fn document_removed(&self, workspace: WorkspaceId, document: DocumentId) -> bool {
        let mut state = self.state();
        state
            .entry(workspace)
            .or_default()
            .documents
            .remove(&document)
            .is_some()
    }

    /// Somebody is looking at a placeholder. The blob is unparked and goes to
    /// the front of the next pass.
    pub fn request(&self, workspace: WorkspaceId, hash: &BlobHash) {
        let mut state = self.state();
        let workspace = state.entry(workspace).or_default();
        workspace.parks.remove(hash.as_str());
        workspace.requested.insert(hash.as_str().to_owned());
    }

    pub fn status(&self, workspace: WorkspaceId) -> BlobTransferStatus {
        self.state()
            .get(&workspace)
            .map(|state| state.status.clone())
            .unwrap_or_default()
    }

    /// **A person's pause, per Workspace** (D256).
    ///
    /// Returns whether it changed anything, so a caller can report news
    /// rather than activity.
    ///
    /// *Documents keep syncing.* This is a fact about the shape of the code
    /// rather than a promise in a comment: the document pass lives in another
    /// state machine that never asks this engine anything, so there is no
    /// path by which a blob pause could reach it.
    ///
    /// A transfer already in flight is not torn down. It finishes its current
    /// chunk and the pass ends at the next blob, which is the same bargain
    /// the metered hold keeps and the same one D250 sizes a chunk for.
    pub fn pause(&self, workspace: WorkspaceId) -> bool {
        self.set_paused(workspace, true)
    }

    /// Lift a person's pause. The caller reconciles afterwards if it wants
    /// the work to resume now rather than at the next trigger — this changes
    /// a flag and starts nothing, because a method that silently began a
    /// gigabyte of uploads would be doing something its name does not say.
    pub fn resume(&self, workspace: WorkspaceId) -> bool {
        self.set_paused(workspace, false)
    }

    pub fn paused(&self, workspace: WorkspaceId) -> bool {
        self.state()
            .get(&workspace)
            .is_some_and(|state| state.paused)
    }

    fn set_paused(&self, workspace: WorkspaceId, paused: bool) -> bool {
        let mut state = self.state();
        let entry = state.entry(workspace).or_default();
        if entry.paused == paused {
            return false;
        }
        entry.paused = paused;
        entry.status.paused = paused;
        // The reason a person can read, the moment they press it rather than
        // at the next pass. A surface that had to wait for a pass to learn
        // this would show "uploading" over a Workspace that had stopped.
        if paused {
            entry.status.pending_reason = Some(TransferHold::Paused.as_str());
        } else if entry.status.pending_reason == Some(TransferHold::Paused.as_str()) {
            entry.status.pending_reason = None;
        }
        true
    }

    /// Startup recovery over the staging directory (§10's "startup and
    /// process death").
    ///
    /// Touches only the engine's own names, quarantines one of those it cannot
    /// parse, and removes a partial download this Workspace does not reference
    /// only once it is abandoned — the staging directory is the whole Store
    /// root's, not this Workspace's (see [`crate::staging`]).
    pub async fn recover(&self, workspace: WorkspaceId) -> Result<(), TransferError> {
        let walk = self.walk(workspace).await?;
        let referenced: BTreeSet<String> = walk
            .blobs
            .iter()
            .map(|blob| blob.hash.as_str().to_owned())
            .collect();
        let staging = self.staging.clone();
        let (kept, removed, quarantined) = tokio::task::spawn_blocking(move || {
            staging.sweep(&referenced, crate::staging::ABANDONED_AFTER)
        })
        .await
        .unwrap_or(Err(TransferError::Local(
            koru_store_core::errors::Code::Internal,
        )))?;
        // Report news, not activity: the ordinary startup finds resumable work
        // and nothing else, and a line for that is a line people scroll past.
        if removed > 0 || quarantined > 0 {
            tracing::info!(
                target: "koru_blob_sync",
                event = "cloud.blob.staging_recovered",
                kept,
                removed,
                quarantined,
            );
        }
        Ok(())
    }

    /// **Cloud coverage, over the documents and the pictures they name.**
    ///
    /// The whole of [`crate::coverage`]'s four steps, in that order, and every
    /// step is here
    /// rather than spread across the hosts so both platforms answer the same
    /// question. What the caller brings is the document half — which documents
    /// this Workspace owes and, for each, the local state the document pass
    /// established on Cloud; what this adds is the blob half, bound to the
    /// **same sample** the references were drawn from.
    ///
    /// It runs after the document pass and after reconciliation, never
    /// instead of them: coverage is a report about work that has already
    /// happened, and a report that moved bytes would be a second transfer
    /// engine.
    pub async fn coverage(
        &self,
        request: CoverageRequest,
        authorization: &LiveAuthorization,
    ) -> Result<CoverageReport, TransferError> {
        let workspace = request.workspace;
        // A Local-only Workspace has no Cloud coverage to compute and is not
        // "incomplete" for lacking one: §6's gate is asked before the account,
        // before the sample and before any request, exactly as a pass does.
        self.admit_cloud(workspace).await?;
        let Some(access) = self.ports.access.access(workspace) else {
            return Err(TransferError::NoAccess);
        };
        // 1 — the sample, taken once.
        let sample = self.sample(workspace).await?;
        // 2 — the references, through the one canonical walk.
        let walk = sample.walk();
        // 3 — a durable mapping with a verified size for each of them. The
        // control plane resolves the Workspace's incarnation itself, and
        // step 4 below is what checks it is still the one we sampled under.
        let mut presence: BTreeMap<String, BlobPresence> = BTreeMap::new();
        for blob in &walk.blobs {
            let status = self
                .control
                .status(access.clone(), workspace, blob.hash.clone())
                .await?;
            presence.insert(blob.hash.as_str().to_owned(), presence_of(&status));
        }
        let report = coverage::compute(&request, &sample, &presence);
        // 4 — the boundary. A second sample and a live authorization read; the
        // report is committed only if both still say what they said.
        let again = self.sample(workspace).await?;
        let report = report.settle(&again, authorization(workspace));
        if report.state == coverage::CoverageState::Superseded {
            // Worth a line, and only this line: the pass did real work and
            // published no claim, which is otherwise invisible.
            tracing::debug!(
                target: "koru_blob_sync",
                event = "cloud.blob.coverage_superseded",
                workspace = %workspace,
                sample = report.sample.short(),
            );
        }
        self.tally(workspace, |status| {
            status.workspace = Some(workspace);
            status.coverage = Some(report.clone());
        });
        // **Kept rather than counted and dropped** (D259 §4). The control
        // plane has just answered this, once per referenced blob; a surface
        // that has to explain one selected file would otherwise ask again.
        // Replaced wholesale, so an address the walk no longer returns leaves
        // with it — a stale row for a picture nobody references any more is a
        // sentence about a file that is gone.
        //
        // The per-document verdicts are kept for the same reason, with one
        // exception. A report superseded because the *sample* moved still
        // judged real states — Cloud holds what it held — and a later read
        // compares against those states anyway. One superseded because the
        // *incarnation* moved judged a dataset that has been replaced, and
        // nothing it said about a document is true of the new one.
        let held = match report.state == coverage::CoverageState::Superseded
            && authorization(workspace) != Some(report.incarnation)
        {
            true => BTreeMap::new(),
            false => coverage::held_documents(&request, &sample),
        };
        let mut state = self.state();
        let entry = state.entry(workspace).or_default();
        entry.presence = presence;
        entry.held = held;
        drop(state);
        Ok(report)
    }

    /// **What Cloud holds for one address**, as the last coverage pass read
    /// it (D259 §4).
    ///
    /// `None` means nothing has been measured for this hash — which is not
    /// "missing", and a surface must not read it as one: no coverage pass has
    /// run for this Workspace since the process started, or this address was
    /// not in the walk it ran over.
    pub fn cloud_presence(
        &self,
        workspace: WorkspaceId,
        hash: &BlobHash,
    ) -> Option<crate::coverage::BlobPresence> {
        self.state()
            .get(&workspace)
            .and_then(|state| state.presence.get(hash.as_str()))
            .copied()
    }

    /// **Everything this engine can say about one file** (D259 §4).
    ///
    /// A file is a selectable thing with its own facts, and the pane that
    /// exists to explain a selection was explaining the document instead.
    /// This is that projection, and it is deliberately assembled *here*
    /// rather than by each host: both halves already live behind this type,
    /// and a host that reached for them separately would be a second place
    /// that has to remember a person's pause outranks the pass.
    ///
    /// **One Store lookup and no network.** The Cloud half is what the last
    /// coverage pass recorded — a selection is a pointer move and must not
    /// cost a round trip to a region — so it is knowingly a moment old, in
    /// the same way `status.coverage` is.
    pub async fn file(&self, workspace: WorkspaceId, hash: BlobHash) -> BlobFileFacts {
        let cloud = self.cloud_presence(workspace, &hash);
        let status = self.status(workspace);
        // A Store that will not answer is not evidence that the bytes are
        // absent, and saying "not on this device" because nobody could be
        // asked is the kind of claim this crate exists not to make. The
        // failure reads as *unknown*, which is what `local: None` beside
        // `here` being derived from it means.
        let local = self
            .ports
            .store
            .workspace_blob(workspace, hash.clone())
            .await
            .ok()
            .flatten();
        BlobFileFacts {
            hash,
            local,
            cloud,
            // A person's own pause outranks whatever the newest pass stopped
            // on, the same order every other surface reads them in: it is the
            // one that will still be there when the train arrives, and the
            // one they can clear.
            hold: match status.paused {
                true => Some(TransferHold::Paused.as_str()),
                false => status.pending_reason,
            },
        }
    }

    /// **Which documents are waiting on a file Cloud does not hold yet**, and
    /// which of those files each one references.
    ///
    /// The engine already knows the files: every blob parked on
    /// `object_missing`, which is Cloud's own answer for *nobody has uploaded
    /// these bytes*. What it does not keep is which document named them, and
    /// that is the canonical walk's job — asked here per document
    /// ([`crate::referenced::document_blobs`]) so a document the walk would
    /// skip is skipped here too.
    ///
    /// **Read-only, and it stays that way.** No park is cleared, shortened
    /// or reset: a person expanding a row must not be a trigger, and the
    /// wait an `object_missing` park carries is what stops a device asking
    /// the region every few seconds for bytes nobody has sent. A park whose
    /// wait has run out is still listed — it is the newest answer anyone has
    /// until the next pass asks again.
    ///
    /// A Workspace with no such park answers without a walk, which is every
    /// Workspace that is not waiting on another device.
    pub async fn waiting(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Vec<WaitingDocument>, TransferError> {
        let missing: BTreeSet<String> = self
            .state()
            .get(&workspace)
            .map(|state| {
                state
                    .parks
                    .iter()
                    .filter(|(_, park)| {
                        park.code == koru_cloud_blob::BlobCode::ObjectMissing.as_str()
                    })
                    .map(|(hash, _)| hash.clone())
                    .collect()
            })
            .unwrap_or_default();
        if missing.is_empty() {
            return Ok(Vec::new());
        }
        let sample = self.sample(workspace).await?;
        Ok(sample
            .documents
            .iter()
            .filter_map(|sampled| {
                let blobs: Vec<BlobHash> = crate::referenced::document_blobs(&sampled.content)
                    .intersection(&missing)
                    .filter_map(|hash| BlobHash::parse(hash).ok())
                    .collect();
                (!blobs.is_empty()).then_some(WaitingDocument {
                    document: sampled.document,
                    blobs,
                })
            })
            .collect())
    }

    /// **One document's own Cloud state**: whether Cloud holds it, and how
    /// many of the images and other files it references Cloud holds durably.
    ///
    /// Everything here was already decided by the last coverage computation —
    /// the per-document verdicts and the per-address presence it keeps — and
    /// this only reads it back for one document. The document's references
    /// come from one sample through the canonical walk, so *which files this
    /// document has* is the rule coverage counts by and not a second one, and
    /// that same sample is what tells a document that moved since Cloud got it
    /// from one that did not.
    ///
    /// **Read-only, like [`Self::waiting`].** No park is cleared, no pass is
    /// started and no network call is made: a person looking at a document is
    /// not a trigger. The cost is the sample — one read of the reference
    /// projection — which is why the context column asks only while its
    /// section is showing.
    pub async fn document_coverage(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<DocumentCoverage, TransferError> {
        let sample = self.sample(workspace).await?;
        let sampled = sample
            .documents
            .into_iter()
            .find(|sampled| sampled.document == document);
        // Unprojected or unlisted: its references are unknown, and so is the
        // state that would say whether Cloud has it.
        let (state_now, references) = match sampled {
            Some(SampledDocument {
                state: Some(state),
                content,
                ..
            }) => (
                Some(state),
                Some(crate::referenced::document_references(&content)),
            ),
            _ => (None, None),
        };
        let guard = self.state();
        let entry = guard.get(&workspace);
        let measured = entry.is_some_and(|entry| entry.status.coverage.is_some());
        let synced = measured.then(|| {
            match (
                entry.and_then(|entry| entry.held.get(&document)),
                &state_now,
            ) {
                (Some(held), Some(now)) => held.includes(now) && now.includes(held),
                _ => false,
            }
        });
        let files = references.map(|references| {
            let durable = |hash: &str| {
                matches!(
                    entry.and_then(|entry| entry.presence.get(hash)),
                    Some(BlobPresence::Durable { size }) if koru_cloud_blob::valid_blob_size(hash, *size)
                )
            };
            let mut files = DocumentFiles {
                // Never an address, so never on Cloud: counted, and never
                // synced, for the walk's own reason.
                files_total: references.unreadable,
                ..DocumentFiles::default()
            };
            for (hash, mime) in &references.blobs {
                let synced = u32::from(durable(hash));
                if is_image(mime.as_deref()) {
                    files.images_total += 1;
                    files.images_synced += synced;
                } else {
                    files.files_total += 1;
                    files.files_synced += synced;
                }
            }
            files
        });
        Ok(DocumentCoverage { synced, files })
    }

    /// **Drop this device's copies of every file Koru Cloud holds**, and keep
    /// every reference.
    ///
    /// The one deliberately destructive thing this engine does, and the
    /// whole of its safety is here: a blob's local bytes go only when the
    /// **control plane** answers `durable` for that address, right now, with
    /// a size equal to the bytes held. Not `cloud_presence` — that is what the
    /// last coverage pass read, and its own doc says it is a moment old, which
    /// is fine for a label and wrong for a deletion. A blob Cloud does not
    /// hold, has not finished verifying, or reports at a different size stays
    /// where it is and is counted as kept.
    ///
    /// **Per blob, and it carries on.** The rule is checked at every address
    /// before that address is touched, so a control failure part way leaves
    /// nothing unsafe behind — some copies are gone, the rest are still here
    /// — and the report says how far it got and why it stopped, rather than
    /// a bare error that hides the numbers.
    ///
    /// The documents are untouched and every association stays: the next
    /// read of a released blob answers `BlobNotFound`, exactly as on a device
    /// that never held it, and the bytes come back the way they came the
    /// first time.
    pub async fn release_local(
        &self,
        workspace: WorkspaceId,
    ) -> Result<ReleaseReport, TransferError> {
        // §6's gate first, as every pass asks it: a Local-only Workspace has
        // no Cloud to have copies on, so nothing here may go.
        self.admit_cloud(workspace).await?;
        let Some(access) = self.ports.access.access(workspace) else {
            return Err(TransferError::NoAccess);
        };
        let walk = self.walk(workspace).await?;
        let mut report = ReleaseReport {
            referenced: walk.blobs.len() as u32,
            ..ReleaseReport::default()
        };
        for blob in &walk.blobs {
            let Some(local) = self
                .ports
                .store
                .workspace_blob(workspace, blob.hash.clone())
                .await?
            else {
                report.not_held += 1;
                continue;
            };
            let status = match self
                .control
                .status(access.clone(), workspace, blob.hash.clone())
                .await
            {
                Ok(status) => status,
                Err(error) => {
                    report.failure_code = Some(error.code().to_owned());
                    break;
                }
            };
            let safe = matches!(
                status,
                koru_cloud_blob::wire::BlobStatusV1::Durable { size, .. } if size == local.size
            );
            if !safe {
                report.kept += 1;
                continue;
            }
            let freed = self
                .ports
                .store
                .release_blob(workspace, blob.hash.clone())
                .await?;
            report.released += 1;
            report.bytes_freed = report.bytes_freed.saturating_add(freed);
        }
        // **The count says so at once.** `present` is the last pass's tally,
        // and since D260 the next pass may be a long time coming; a surface
        // reading "3 on this device" beside "removed 3" would be the kind of
        // stale number this feature exists to make honest.
        self.tally(workspace, |status| {
            status.present = status.present.saturating_sub(report.released);
        });
        // News by construction: a person pressed something and bytes left the
        // disk. Counts only — no name, no hash, no title.
        tracing::info!(
            target: "koru_blob_sync",
            event = "cloud.blob.released",
            workspace = %workspace,
            referenced = report.referenced,
            released = report.released,
            bytes_freed = report.bytes_freed,
            kept = report.kept,
            not_held = report.not_held,
            stopped = report.failure_code.as_deref().unwrap_or(""),
        );
        Ok(report)
    }

    /// One bounded pass over one Workspace's referenced blobs.
    pub async fn reconcile(&self, workspace: WorkspaceId, trigger: Trigger) -> Pass {
        // Held for the whole pass. Taken before the debounce is read, so a
        // caller that waited here measures the debounce against the pass that
        // just finished rather than the one that was running when it arrived.
        let gate = self
            .passes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(workspace)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _pass = gate.lock().await;

        // The debounce, and the one trigger it applies to. A burst of durable
        // commits during a paste or an import is one pass; enablement, a
        // mapping change, policy, recovery and an explicit request are all
        // rare and all answered at once.
        if trigger == Trigger::DocumentsChanged {
            let mut state = self.state();
            let entry = state.entry(workspace).or_default();
            if entry
                .last_pass
                .is_some_and(|last| last.elapsed() < self.debounce)
            {
                entry.status.passes_skipped = entry.status.passes_skipped.saturating_add(1);
                return Pass {
                    skipped: true,
                    ..Pass::default()
                };
            }
        }
        if trigger.clears_parks() {
            let mut state = self.state();
            let entry = state.entry(workspace).or_default();
            if let Trigger::Requested(hash) = &trigger {
                // A request is a person looking, and it reopens every park
                // but one: a blob the Cloud has never held is waiting on
                // another device, and looking at it again does not bring
                // that device online. Its wait stands until it runs out;
                // the rare triggers (enablement, a mapping change, policy)
                // still clear everything, because those change the answer.
                let now = Instant::now();
                entry.parks.retain(|_, park| {
                    park.code == koru_cloud_blob::BlobCode::ObjectMissing.as_str()
                        && park.until.is_some_and(|until| now < until)
                });
                let waiting: Vec<String> = entry.parks.keys().cloned().collect();
                entry.backoff.retain(|hash, _| waiting.contains(hash));
                entry.requested.insert(hash.as_str().to_owned());
            } else {
                entry.parks.clear();
                entry.backoff.clear();
            }
        }

        let mut pass = Pass::default();
        // **§6's fourth point, at the session.** Before the account, before the
        // walk and before any network call: a Local-only Workspace does not
        // allocate, does not download, and does not even read its own reference
        // set to find out what it would have carried.
        if let Err(refusal) = self.admit_cloud(workspace).await {
            self.session_stopped(workspace, &refusal, &mut pass);
            return pass;
        }
        let Some(access) = self.ports.access.access(workspace) else {
            self.session_stopped(workspace, &TransferError::NoAccess, &mut pass);
            return pass;
        };
        // **D50, in the one place a transfer is decided.** A hold stops the
        // automatic pass with the reason on the status; an *explicit* request
        // carries on through the holds a person is allowed to waive, and
        // carries only what was asked for. That distinction is the whole of
        // D50's sentence, and it lives here rather than at a platform so both
        // platforms get it from one state machine.
        //
        // **A person's own pause is read first** (D256). Both can be true at
        // once — a paused Workspace on a metered train — and only one code
        // fits on the line. The person's is the one that will still be there
        // when the train arrives, and the one they can clear; reporting the
        // network instead would tell somebody to find better signal for a
        // stop they made themselves.
        let held = self
            .paused(workspace)
            .then_some(TransferHold::Paused)
            .or_else(|| {
                (!self.ports.policy.transfers_permitted(workspace))
                    .then(|| self.ports.policy.hold(workspace))
            });
        let asked = matches!(trigger, Trigger::Requested(_));
        if let Some(hold) = held
            && !(asked && hold.overridable())
        {
            self.session_stopped(workspace, &TransferError::NotPermitted(hold), &mut pass);
            return pass;
        }
        let walk = match self.walk(workspace).await {
            Ok(walk) => walk,
            Err(error) => {
                self.session_stopped(workspace, &error, &mut pass);
                return pass;
            }
        };

        let object_host = access
            .byte_origins
            .origins()
            .next()
            .and_then(|origin| url::Url::parse(origin).ok())
            .and_then(|url| url.host_str().map(str::to_owned));
        {
            let mut state = self.state();
            let entry = state.entry(workspace).or_default();
            entry.last_pass = Some(Instant::now());
            entry.status = BlobTransferStatus {
                workspace: Some(workspace),
                referenced: walk.blobs.len() as u32,
                unreadable_references: walk.unreadable,
                object_host,
                walks: self.walks(),
                passes: entry.status.passes.saturating_add(1),
                passes_skipped: entry.status.passes_skipped,
                last_failure_code: entry.status.last_failure_code,
                // **A pass does not invalidate a coverage claim.** Coverage is
                // committed against its own sample and its own boundary; a
                // reconciliation starting is not evidence that what was
                // committed has stopped being true, and blanking it here would
                // make the Cloud surface flicker to "unknown" on every commit.
                coverage: entry.status.coverage.clone(),
                path: Some(TransferPath::Cloud),
                // Neither of these belongs to a pass. A pause is a statement
                // a person made and a pre-flight is what the enable found;
                // resetting either here would make both flicker once every
                // five seconds on the surface that reads them.
                paused: entry.paused,
                preflight: entry.status.preflight,
                ..BlobTransferStatus::default()
            };
        }

        // What a person explicitly asked for. Read whether or not a hold is
        // on: under a hold it is what may carry on through it, and always it
        // is one of the three things that make an absent file *wanted*
        // (D260) — the others being a document showing it, and a download
        // this process started before it died.
        let requested: BTreeSet<String> = self
            .state()
            .get(&workspace)
            .map(|state| state.requested.clone())
            .unwrap_or_default();

        let ordered = self.ordered(workspace, walk.blobs);
        // **Tell, then start** (D256), and from *this* walk.
        //
        // Only the enable pass pays for it, which is the pass a person is
        // watching and the only one where "what is about to happen" is news.
        // It resolves each referenced blob against the local object store
        // once and hands the answers to the loop below, so the summary costs
        // no walk and no second round of metadata reads — `walks` and the
        // number of `workspace_blob` calls are both unchanged by it.
        let mut resolved: BTreeMap<String, Option<crate::ports::LocalBlob>> = BTreeMap::new();
        if trigger == Trigger::Enabled {
            let mut preflight = crate::status::BlobPreflight {
                files: ordered.len() as u32,
                unreadable: walk.unreadable,
                at_millis: now_millis(),
                ..crate::status::BlobPreflight::default()
            };
            for blob in &ordered {
                let local = match self
                    .ports
                    .store
                    .workspace_blob(workspace, blob.hash.clone())
                    .await
                {
                    Ok(local) => local,
                    // A Store that will not answer for one blob does not get
                    // to turn the summary into a lie about the rest. The pass
                    // carries on and resolves that blob the ordinary way,
                    // where its failure is already handled.
                    Err(_) => continue,
                };
                match &local {
                    Some(local) => {
                        preflight.upload_files = preflight.upload_files.saturating_add(1);
                        preflight.upload_bytes = preflight.upload_bytes.saturating_add(local.size);
                    }
                    // Only what will actually be fetched (D260): a summary
                    // that counted every absent file would promise downloads
                    // the pass is not going to make.
                    None if self.wanted(workspace, &blob.hash, &requested) => {
                        preflight.download_files = preflight.download_files.saturating_add(1);
                    }
                    None => {}
                }
                resolved.insert(blob.hash.as_str().to_owned(), local);
            }
            self.tally(workspace, |status| status.preflight = Some(preflight));
            // Unconditional, and news by construction: enabling Cloud on a
            // Workspace happens once and is the moment gigabytes start
            // moving. Counts and bytes only — no name, no hash, no title.
            tracing::info!(
                target: "koru_blob_sync",
                event = "cloud.blob.preflight",
                workspace = %workspace,
                files = preflight.files,
                upload_files = preflight.upload_files,
                upload_bytes = preflight.upload_bytes,
                download_files = preflight.download_files,
                unreadable = preflight.unreadable,
            );
        }

        for blob in ordered {
            if let Some(hold) = held
                && !requested.contains(blob.hash.as_str())
            {
                pass.parked.push((blob.hash.clone(), hold.as_str()));
                continue;
            }
            if self.parked(workspace, &blob.hash) {
                let code = self.park_code(workspace, &blob.hash);
                pass.parked.push((blob.hash.clone(), code));
                continue;
            }
            let known = resolved.remove(blob.hash.as_str());
            let wanted = self.wanted(workspace, &blob.hash, &requested);
            let outcome = self
                .carry(workspace, &access, &blob, known, wanted, &mut pass)
                .await;
            if let Err(error) = outcome {
                match crate::retry::classify(&error) {
                    // About the session: everything in it waits, and the pass
                    // ends. Walking every remaining blob into the same wall
                    // and then reporting a pass that "finished" is the failure
                    // the document pass already learned not to have.
                    Disposition::ParkSession => {
                        self.session_stopped(workspace, &error, &mut pass);
                        return pass;
                    }
                    // About this blob. It defers and its neighbours reconcile.
                    _ => self.park(workspace, &blob.hash, &error, &mut pass),
                }
            }
        }
        self.finish(workspace, held, &mut pass);
        pass
    }

    // ------------------------------------------------------------- internals

    /// **Whether an absent file is fetched** (D260).
    ///
    /// Three things make it so, and nothing else does: somebody asked for it
    /// (`request`, from a placeholder or a file being opened), a document
    /// showing it is on screen (`watching`), or this process was already
    /// part way through fetching it when it last died — a resume is finishing
    /// an ask, not making a new one. A referenced file none of those is true
    /// for stays on Cloud, whatever the trigger: signing in on a new device
    /// brings the documents and leaves the files where they are until one is
    /// opened, and *Free up space* stays done.
    ///
    /// Uploads are untouched. A device holding bytes still sends them on
    /// every pass; what changed is only which direction a pass moves bytes on
    /// its own.
    fn wanted(
        &self,
        workspace: WorkspaceId,
        hash: &BlobHash,
        requested: &BTreeSet<String>,
    ) -> bool {
        requested.contains(hash.as_str())
            || self.ports.downloads.watching(workspace, hash)
            || self.staging().synced_length(hash).unwrap_or(0) > 0
    }

    fn state(&self) -> std::sync::MutexGuard<'_, BTreeMap<WorkspaceId, WorkspaceState>> {
        // Bookkeeping: a panic somewhere else must not be able to take the
        // blob engine with it, exactly as the document pass's ledger does.
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    async fn walk(&self, workspace: WorkspaceId) -> Result<crate::referenced::Walk, TransferError> {
        Ok(self.sample(workspace).await?.walk())
    }

    /// **Coverage's first step**, and the only read under it.
    ///
    /// Counted as a walk because it is one: the same read the reconciliation
    /// pass runs on, so the "typing performs no full-Workspace walk"
    /// measurement keeps measuring the thing it was written for.
    async fn sample(&self, workspace: WorkspaceId) -> Result<DocumentSample, TransferError> {
        self.walks.fetch_add(1, Ordering::Relaxed);
        self.ports.store.sample_documents(workspace).await
    }

    /// Requested first, then watched, then the rest in address order. A
    /// picture somebody is staring at a placeholder for goes before one
    /// nothing is showing.
    fn ordered(&self, workspace: WorkspaceId, blobs: Vec<ReferencedBlob>) -> Vec<ReferencedBlob> {
        let requested: BTreeSet<String> = self
            .state()
            .get(&workspace)
            .map(|state| state.requested.clone())
            .unwrap_or_default();
        let mut ordered = blobs;
        ordered.sort_by_key(|blob| {
            let rank = if requested.contains(blob.hash.as_str()) {
                0
            } else if self.ports.downloads.watching(workspace, &blob.hash) {
                1
            } else {
                2
            };
            (rank, blob.hash.as_str().to_owned())
        });
        ordered
    }

    /// Carry one blob.
    ///
    /// `known` is the local answer the pre-flight already read, when there
    /// was one — `Some(Some(blob))` this device holds it, `Some(None)` it
    /// does not, `None` nobody has asked yet. It exists so that *tell, then
    /// start* costs one resolution rather than two, not as an optimisation:
    /// a second read would be a second answer, and the summary a person was
    /// shown would then be able to disagree with what the pass did.
    async fn carry(
        &self,
        workspace: WorkspaceId,
        access: &crate::ports::CloudAccessFacts,
        blob: &ReferencedBlob,
        known: Option<Option<crate::ports::LocalBlob>>,
        wanted: bool,
        pass: &mut Pass,
    ) -> Result<(), TransferError> {
        let local = match known {
            Some(local) => local,
            None => {
                self.ports
                    .store
                    .workspace_blob(workspace, blob.hash.clone())
                    .await?
            }
        };
        match local {
            // This device holds the bytes: the blob's job is to reach Cloud.
            Some(mut local) => {
                if local.mime.trim().is_empty()
                    && let Some(mime) = blob.mime.clone()
                {
                    local.mime = mime;
                }
                self.note(workspace, Direction::Upload, 0, local.size);
                match upload::run(self, access, workspace, &local).await? {
                    UploadOutcome::AlreadyDurable => {
                        pass.already_durable += 1;
                        self.tally(workspace, |status| {
                            status.present += 1;
                            status.durable += 1;
                        });
                    }
                    UploadOutcome::Durable { sent } => {
                        pass.uploaded += 1;
                        self.tally(workspace, |status| {
                            status.present += 1;
                            status.durable += 1;
                            status.bytes_complete += sent;
                        });
                    }
                    UploadOutcome::Verifying { sent } => {
                        pass.uploaded += 1;
                        self.tally(workspace, |status| {
                            status.present += 1;
                            status.uploading += 1;
                            status.bytes_complete += sent;
                        });
                    }
                }
            }
            // It does not, and nobody asked: it stays on Cloud (D260). Not
            // a park — nothing is waiting on anything — and not counted
            // present, because it is not.
            None if !wanted => {
                pass.deferred += 1;
            }
            // It does not, and somebody did: the blob's job is to arrive.
            None => {
                self.note(workspace, Direction::Download, 0, 0);
                match download::run(self, access, workspace, &blob.hash).await? {
                    DownloadOutcome::AlreadyLocal => {
                        pass.already_local += 1;
                        self.tally(workspace, |status| status.present += 1);
                    }
                    DownloadOutcome::Arrived { received, total } => {
                        pass.downloaded += 1;
                        self.tally(workspace, |status| {
                            status.present += 1;
                            status.durable += 1;
                            status.bytes_complete += received;
                            status.bytes_total += total;
                        });
                    }
                }
            }
        }
        self.state()
            .entry(workspace)
            .or_default()
            .backoff
            .remove(blob.hash.as_str());
        self.state()
            .entry(workspace)
            .or_default()
            .requested
            .remove(blob.hash.as_str());
        Ok(())
    }

    fn note(&self, workspace: WorkspaceId, direction: Direction, complete: u64, total: u64) {
        let mut state = self.state();
        let status = &mut state.entry(workspace).or_default().status;
        status.direction = Some(direction);
        status.bytes_total = status.bytes_total.saturating_add(total);
        status.bytes_complete = status.bytes_complete.saturating_add(complete);
    }

    fn tally(&self, workspace: WorkspaceId, write: impl FnOnce(&mut BlobTransferStatus)) {
        let mut state = self.state();
        write(&mut state.entry(workspace).or_default().status);
    }

    fn parked(&self, workspace: WorkspaceId, hash: &BlobHash) -> bool {
        self.state()
            .get(&workspace)
            .and_then(|state| state.parks.get(hash.as_str()))
            .is_some_and(|park| park.until.is_none_or(|until| Instant::now() < until))
    }

    fn park_code(&self, workspace: WorkspaceId, hash: &BlobHash) -> &'static str {
        self.state()
            .get(&workspace)
            .and_then(|state| state.parks.get(hash.as_str()))
            .map(|park| park.code)
            .unwrap_or("parked")
    }

    fn park(
        &self,
        workspace: WorkspaceId,
        hash: &BlobHash,
        error: &TransferError,
        pass: &mut Pass,
    ) {
        let code = error.code();
        let retryable = matches!(crate::retry::classify(error), Disposition::Retry);
        let missing = matches!(
            error,
            TransferError::Refused(koru_cloud_blob::BlobCode::ObjectMissing)
        );
        let mut state = self.state();
        let entry = state.entry(workspace).or_default();
        // A retryable fault waits seconds. A blob the Cloud has never held
        // waits minutes, on its own schedule (`retry::MISSING_MIN_BACKOFF`):
        // the answer changes only when another device uploads, and a park
        // with no deadline was reopened by every request for the picture.
        let until = if retryable {
            let wait = entry
                .backoff
                .entry(hash.as_str().to_owned())
                .or_default()
                .failed();
            Some(Instant::now() + wait)
        } else if missing {
            let wait = entry
                .backoff
                .entry(hash.as_str().to_owned())
                .or_default()
                .failed_within(
                    crate::retry::MISSING_MIN_BACKOFF,
                    crate::retry::MISSING_MAX_BACKOFF,
                );
            Some(Instant::now() + wait)
        } else {
            None
        };
        entry
            .parks
            .insert(hash.as_str().to_owned(), Park { code, until });
        entry.status.missing = entry.status.missing.saturating_add(1);
        // **A blob nobody has uploaded yet is a wait, not a failure.** Cloud
        // answering `object_missing` for a blob it has never held is the
        // correct answer while the device holding the bytes is offline, and
        // filing it under "last failure" tells a person their pictures are
        // broken when what is true is that their other laptop is shut. The
        // same rule `session_stopped` already applies to a pause, applied to
        // the one unit code that means the same thing.
        //
        // `pending_reason` still carries it, so nothing is hidden — what is
        // owed is the *reason we are waiting*, which is exactly this code.
        // Seen on hardware: a Workspace referencing six blobs of which Cloud
        // held two read as damaged, and was not.
        if !matches!(
            error,
            TransferError::Refused(koru_cloud_blob::BlobCode::ObjectMissing)
        ) {
            entry.status.last_failure_code = Some(code);
        }
        entry.status.pending_reason = Some(code);
        drop(state);
        // Unconditional: a transfer abandoned with durable progress, a
        // checksum mismatch and a fenced session are all news about somebody's
        // file. The rest is per-blob detail and is at `debug`.
        match error {
            TransferError::Refused(koru_cloud_blob::BlobCode::HashMismatch)
            | TransferError::Origin(_) => tracing::warn!(
                target: "koru_blob_sync",
                event = "cloud.blob.transfer_abandoned",
                workspace = %workspace,
                code,
            ),
            // News about somebody's disk, and the one failure whose remedy is
            // theirs rather than ours. Never silent.
            TransferError::InsufficientStorage => tracing::warn!(
                target: "koru_blob_sync",
                event = "cloud.blob.storage_insufficient",
                workspace = %workspace,
                code,
            ),
            _ => tracing::debug!(
                target: "koru_blob_sync",
                event = "cloud.blob.transfer_deferred",
                workspace = %workspace,
                code,
            ),
        }
        pass.parked.push((hash.clone(), code));
    }

    fn session_stopped(&self, workspace: WorkspaceId, error: &TransferError, pass: &mut Pass) {
        let code = error.code();
        // **A pause is not a failure**, and writing it into the failure field
        // is how a person on a train is told their pictures are broken. The
        // pending reason says what is being waited on; `last_failure_code` is
        // reserved for something that actually went wrong, so a metered pause
        // leaves the previous one alone.
        let paused = matches!(error, TransferError::NotPermitted(hold) if hold.is_pause());
        pass.session_refusal = Some(code);
        if let TransferError::NotPermitted(hold) = error {
            pass.session_hold = Some(*hold);
        }
        let mut state = self.state();
        let entry = state.entry(workspace).or_default();
        entry.status.workspace = Some(workspace);
        entry.status.pending_reason = Some(code);
        if !paused {
            entry.status.last_failure_code = Some(code);
        }
        drop(state);
        if paused {
            // Routine and per-pass: a phone on cellular says this on every
            // wake, and a line a reader could have predicted does not earn
            // its place. The status carries it, always live and free.
            tracing::debug!(
                target: "koru_blob_sync",
                event = "cloud.blob.pass_paused",
                workspace = %workspace,
                code,
            );
            return;
        }
        tracing::warn!(
            target: "koru_blob_sync",
            event = "cloud.blob.pass_refused",
            workspace = %workspace,
            code,
        );
    }

    fn finish(&self, workspace: WorkspaceId, held: Option<TransferHold>, pass: &mut Pass) {
        if !pass.parked.is_empty() {
            let mut reasons = BTreeMap::<&str, usize>::new();
            for (_, code) in &pass.parked {
                *reasons.entry(code).or_default() += 1;
            }
            tracing::info!(target: "koru_blob_sync", event = "cloud.blob.wait_reasons",
                %workspace, ?reasons);
        }
        let mut state = self.state();
        let entry = state.entry(workspace).or_default();
        let oldest = entry
            .parks
            .values()
            .filter_map(|park| park.until)
            .min()
            .map(|until| {
                let wait = until.saturating_duration_since(Instant::now());
                now_millis().saturating_add(wait.as_millis().min(i64::MAX as u128) as i64)
            });
        entry.status.oldest_retry_at_millis = oldest;
        entry.status.missing = pass.parked.len() as u32;
        // A pass that ran *through* a hold — one picture fetched on cellular
        // because somebody asked — still leaves the Workspace paused, and the
        // status has to keep saying so even when the asked-for blob was the
        // only one outstanding. The hold outranks the parks it produced.
        match held {
            Some(hold) => entry.status.pending_reason = Some(hold.as_str()),
            None if pass.parked.is_empty() => entry.status.pending_reason = None,
            // A park that outlived the pass that made it — a blob the Cloud
            // does not hold yet, still inside its wait — was only skipped this
            // time, and skipping it sets no reason. The wait is still owed one.
            None => {
                if entry.status.pending_reason.is_none() {
                    entry.status.pending_reason = pass.parked.first().map(|(_, code)| *code);
                }
            }
        }
    }
}

/// The control plane's answer about one address, as coverage reads it.
///
/// **A verified size or nothing.** `Durable` carries the length the object
/// worker proved before the API claimed durability, and that length is what
/// coverage's third step asks for; a durable answer without one is refused by
/// [`coverage::compute`] rather than being counted as a small file.
fn presence_of(status: &koru_cloud_blob::wire::BlobStatusV1) -> BlobPresence {
    use koru_cloud_blob::wire::BlobStatusV1;
    match status {
        BlobStatusV1::Durable { size, .. } => BlobPresence::Durable { size: *size },
        BlobStatusV1::Uploading { .. } => BlobPresence::Uploading,
        BlobStatusV1::Missing { .. } => BlobPresence::Missing,
    }
}

/// An image is a reference whose recorded media type is `image/*`. The type
/// is metadata a node recorded, so it is compared without regard to case and
/// nothing is sniffed: a reference with no type is a file.
fn is_image(mime: Option<&str>) -> bool {
    mime.and_then(|mime| mime.get(..6))
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("image/"))
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
