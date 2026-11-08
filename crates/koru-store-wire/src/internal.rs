// SPDX-License-Identifier: MPL-2.0

//! **Ours, not a customer's.** The `/v1/debug/*` and `/v1/metrics` reports.
//!
//! These are counters for diagnosing a store, and they change whenever
//! something new is worth seeing. They are typed — so our own server and our
//! own client break together when a field is renamed — but they carry **no
//! compatibility promise** to anyone else, and the split between a public API
//! document and an internal one exists to say so out loud. Until that split
//! lands they are still published in `GET /v1/openapi.json`, which is item 2 of
//! [TECHDEBT_API_DOC](../../../docs/contributing/techdebt/API_DOC.md).
//!
//! **Metadata only. Never content.** Every field here is a count, a size, a
//! version, an instant or an id. No document text, no title, no filename, no
//! query text, no blob bytes. That is not a politeness: these are the reports
//! most likely to end up pasted into a bug report, a support thread or a log
//! aggregator, and the one thing that must never travel that way is what
//! somebody wrote.
//!
//! Adding a field to any of these is safe for an older reader, because serde
//! ignores what it does not know. Renaming or removing one is what breaks, and
//! that is the drift worth being told about.

use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_core::model::Lineage;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `GET /v1/debug/store`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StoreReport {
    pub channel: String,
    pub pid: u32,
    /// **One opaque string.** Not a path a client may take apart: knowing where
    /// a database is is not the same as being allowed to open it, and making
    /// the physical layout visible makes it a compatibility surface.
    pub data_directory: String,
    pub started_at: i64,
    pub uptime_ms: i64,
    pub api_version: u32,
    pub catalog_schema_version: i64,
    pub workspace_schema_version: i64,
    pub crdt_schema_version: i64,
    pub bundle_format_version: u32,
    /// The store's own Loro replica id — a replica, never a person or a machine.
    ///
    /// A string because a `u64` loses precision in JSON's number type, and a
    /// peer id that silently rounds is worse than one that is awkward to read.
    pub peer_id: String,
    pub workspaces_known: usize,
    pub workspaces_open: usize,
    pub documents_open: usize,
    pub rooms: Vec<RoomReport>,
    /// Anything salvaged at open. Developer text; never customer content.
    pub recoveries: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RoomReport {
    pub room: String,
    pub subscribers: usize,
}

/// `GET /v1/debug/workspaces/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceReport {
    pub workspace: WorkspaceId,
    pub database_bytes: u64,
    pub documents: usize,
    pub blobs: usize,
    pub blob_bytes: u64,
    pub lineage: Vec<Lineage>,
    /// The manifest document this workspace's contents live behind.
    pub manifest_document: DocumentId,
    /// The catalog row's deletion state, when the workspace is in the trash.
    #[serde(default)]
    pub deleted_at_millis: Option<i64>,
    /// The manifest's own deletion state — `active`, `restored`, or `unset` —
    /// which is the truth the catalog row projects. A row that disagrees
    /// with it is stale; a manifest that disagrees with a peer's is a sync
    /// gap.
    #[serde(default)]
    pub manifest_deletion: Option<String>,
    /// Shared appearance from the catalog row — icon and color token, when
    /// the workspace has them. `None` when the workspace was never styled
    /// (or was duplicated, which starts plain by design).
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    /// What blob transfer is doing for this workspace right now, when this
    /// build has an engine. `None` on a build with no Cloud blob path.
    #[serde(default)]
    pub blob_transfer: Option<BlobTransferReport>,
}

/// One workspace's blob transfer state (the blob plan's §15).
///
/// **Ids, counts and codes only.** Never a filename, never a title, never a
/// capability, never a byte. The one string that is not a code is the object
/// host, and it is there because *"the upload is stuck"* has two very
/// different causes once there are two services.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BlobTransferReport {
    /// Live hashes the canonical referenced-blob walk found.
    pub referenced: u32,
    /// Of those, the ones this device holds and this workspace may read.
    pub present: u32,
    /// Of those, the ones Cloud confirmed durable in the last pass.
    pub durable: u32,
    pub uploading: u32,
    pub downloading: u32,
    /// Referenced, asked for, and parked by the last pass — a file that could
    /// not arrive. A file nobody asked for is not counted: it stays on Cloud
    /// until it is opened (D260).
    pub missing: u32,
    pub bytes_complete: u64,
    pub bytes_total: u64,
    /// The stable code the pass is waiting on, from the blob contract's §7.
    #[serde(default)]
    pub pending_reason: Option<String>,
    #[serde(default)]
    pub last_failure_code: Option<String>,
    /// `upload` or `download`.
    #[serde(default)]
    pub direction: Option<String>,
    /// **Which path a transfer used** — `cloud` or `peer_direct`.
    #[serde(default)]
    pub path: Option<String>,
    /// **Which service answered.** The object plane's host, no scheme and no
    /// path.
    #[serde(default)]
    pub object_host: Option<String>,
    #[serde(default)]
    pub oldest_retry_at_millis: Option<i64>,
    /// References the walk could not turn into an address. A coverage number
    /// that hid these would claim a completeness it has not got.
    pub unreadable_references: u32,
    /// How many canonical walks this engine has run, and how many passes ran
    /// or were skipped because nothing had moved. The ratio is the answer to
    /// *"is this thing walking on every commit"*.
    pub walks: u64,
    pub passes: u64,
    pub passes_skipped: u64,
    /// **The last committed coverage claim**, documents and pictures counted
    /// separately. `None` means this store has not computed one for the
    /// workspace since it started — which is not the same as *nothing is
    /// missing*.
    #[serde(default)]
    pub coverage: Option<BlobCoverageReport>,
    /// **A person paused blob transfer for this workspace** (D256). Documents
    /// are untouched by it.
    #[serde(default)]
    pub paused: bool,
    /// What the last enable found before it started moving anything. `None`
    /// until Cloud has been enabled for this workspace since the store
    /// started — which is not the same as zero.
    #[serde(default)]
    pub preflight: Option<BlobPreflightReport>,
}

/// **What clearing local files did**, in numbers a person can read (rung 2's
/// space reclaim).
///
/// `kept` is the count that matters: every one of those is a file this device
/// may be the only place for, as far as Cloud has confirmed, and a surface
/// that folded it into "freed" would report a deletion that did not happen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BlobReleaseReport {
    /// Live hashes the canonical walk found.
    pub referenced: u32,
    /// Copies that left this device.
    pub released: u32,
    pub bytes_freed: u64,
    /// Held here and not confirmed durable on Koru Cloud at the moment of
    /// asking. Untouched.
    pub kept: u32,
    /// Referenced and not held here: nothing to free.
    pub not_held: u32,
    /// The stable code the control plane stopped on, when it did. The counts
    /// above are what happened before it stopped.
    #[serde(default)]
    pub failure_code: Option<String>,
}

/// Whether Cloud holds this workspace's documents **and** the pictures they
/// reference, as two numbers rather than one.
///
/// The separation is the whole point: a missing picture and a missing document
/// are different problems with different remedies, and one undifferentiated
/// "not current" is what makes a person restart an application that was
/// working.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BlobCoverageReport {
    /// `complete`, `incomplete` or `superseded`. The last one means the
    /// workspace or its authorization moved between the sample and the
    /// boundary, and **nothing is claimed**.
    pub state: String,
    /// The one sentence a surface may derive from this report.
    pub current: bool,
    pub documents_required: u32,
    /// Documents Cloud does not hold the sampled state of.
    pub documents_missing: u32,
    pub blobs_referenced: u32,
    pub blobs_durable: u32,
    /// Referenced, with an upload session in flight. Neither durable nor
    /// missing, because *wait* and *something is wrong* are different
    /// sentences.
    pub blobs_uploading: u32,
    /// Referenced, and Cloud has nothing.
    pub blobs_missing: u32,
    pub unreadable_references: u32,
    /// The sample this claim was computed against, shortened. An identity to
    /// compare two reports with, never authority for anything.
    pub sample: String,
}

/// **What was about to move, measured before the first byte did** (D256).
///
/// *Tell, then start.* Enabling Cloud on a workspace of several years can be
/// gigabytes on the first afternoon; this is the sentence a person reads
/// first, and it is the reconciliation walk's own answer rather than a second
/// sweep.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BlobPreflightReport {
    /// Every live hash the walk found.
    pub files: u32,
    /// Of those, the ones this device holds — the ones about to be uploaded.
    pub upload_files: u32,
    /// What those weigh, exactly: their sizes are local facts.
    pub upload_bytes: u64,
    /// Of those, the ones this device will fetch. Counted without a size,
    /// because a blob this device has never seen has no local size and a
    /// guessed number is worse than an absent one.
    pub download_files: u32,
    /// References the walk could not turn into an address, so the summary
    /// cannot claim a completeness the walk did not have.
    pub unreadable: u32,
    /// When it was taken, epoch milliseconds.
    pub at_millis: i64,
}

/// `GET /v1/debug/documents/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DocumentReport {
    pub document: DocumentId,
    pub workspace: WorkspaceId,
    pub kind: String,
    /// Hex, because a version vector is bytes and JSON has no type for them.
    pub version_vector: String,
    pub frontiers: String,
    pub latest_checkpoint: Option<u64>,
    pub checkpoints: usize,
    /// Updates the newest checkpoint does not cover — the replay a restart
    /// would do, and the number that says whether checkpointing is keeping up.
    pub pending_updates: u64,
    /// The manifest's declared CRDT schema, when this document is one. A
    /// value newer than this build understands is why a document is
    /// read-only.
    #[serde(default)]
    pub schema_version: Option<i64>,
    /// When the document is in the workspace's trash — hidden from one
    /// device's listing while another opens it with an In Trash strip.
    #[serde(default)]
    pub trashed_at_millis: Option<i64>,
    pub subscribers: usize,
    pub updated_at: i64,
}

/// `GET /v1/workspaces/{id}/documents/{document}/cloud-sync`.
///
/// **What a Cloud pass did to one open document**, answered in one round trip
/// so a person can see it without reading the gateway log. Everything here is
/// a time, a count, a stable code or a device's display name; nothing names a
/// title, a heading or a byte of what somebody wrote.
///
/// Every field is optional in the honest sense: `None` means *this store has
/// no fact*, never zero. A surface that cannot source a line omits it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CloudSyncStatus {
    pub workspace: Option<WorkspaceId>,
    pub document: Option<DocumentId>,
    /// Whether a live Cloud permit covers this workspace's dataset right now,
    /// and what it is doing. `None` when the Agent has installed no Cloud
    /// context at all — which is what being signed out looks like from here.
    #[serde(default)]
    pub sync: Option<CloudSyncState>,
    /// The newest facts the gateway remembers about passes over *this*
    /// document. `None` until one has run since the store started.
    #[serde(default)]
    pub document_activity: Option<CloudDocumentActivity>,
    /// The newest facts about the whole dataset's last pass.
    #[serde(default)]
    pub pass: Option<CloudPassActivity>,
    /// When this document last changed locally, and which replica path
    /// supplied that change.
    #[serde(default)]
    pub local: Option<CloudLocalChange>,
    /// **What the pictures are doing** (D256's third answer).
    ///
    /// The Cloud sync tab is the one place a person reads sync truth, so the
    /// blob half arrives with the document half rather than through a second
    /// poll that would show a second moment and call it one. It is about the
    /// **workspace**, not this document: which blobs a document references is
    /// a question about its content and the store deliberately cannot answer
    /// it (ADR-80).
    ///
    /// `None` on a build with no blob engine — which is not zero.
    #[serde(default)]
    pub blobs: Option<BlobTransferReport>,
    /// **The one file the request asked about** (D259 §4).
    ///
    /// A file is a selectable thing with its own facts, and the pane that
    /// exists to explain a selection was explaining the document instead. It
    /// arrives here rather than through a route of its own for the reason
    /// [`Self::blobs`] does: two polls are two moments, and a surface showing
    /// them side by side would be calling them one.
    ///
    /// `None` when the request named no blob, when this build has no blob
    /// engine, or when the address will not parse.
    #[serde(default)]
    pub file: Option<BlobFileReport>,
}

/// `GET /v1/workspaces/{id}/documents/{document}/cloud-sync/coverage`.
///
/// **One document's own Cloud state**, and nothing about the workspace around
/// it: whether Cloud holds the document as it is now, and how many of the
/// images and other files it references Cloud holds durably. The workspace's
/// totals live in `CloudSyncStatus::blobs`; this is the per-document answer
/// the context column shows beside the document.
///
/// Its own route rather than a field on [`CloudSyncStatus`], because that
/// report is also asked once per workspace by Manage Profiles, and this one
/// costs a read of the workspace's reference projection that only the
/// surface showing a document should pay.
///
/// **Counts only.** No address, no filename, no media type string.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CloudDocumentCoverage {
    pub workspace: Option<WorkspaceId>,
    pub document: Option<DocumentId>,
    /// **Cloud holds this document's CRDT state as it is now.** Absent when
    /// the store has not computed a coverage claim for the workspace since it
    /// started — *not measured*, which is not `false`. `false` is every way
    /// of not being held: never carried, carried at a state the document has
    /// since moved past, or not read by the store yet.
    #[serde(default)]
    pub synced: Option<bool>,
    /// The files this document references. Absent when the store does not
    /// know its references yet, which is not *no files*.
    #[serde(default)]
    pub files: Option<CloudDocumentFiles>,
}

/// One document's referenced files: images (`image/*`) and everything else,
/// each with how many Koru Cloud holds durably. An upload in flight is not
/// counted as synced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CloudDocumentFiles {
    pub images_total: u32,
    pub images_synced: u32,
    pub files_total: u32,
    pub files_synced: u32,
}

/// What one Cloud-sync request wants beyond the document.
///
/// One optional file, so the same round trip that already carries the
/// document's and the workspace's state carries the selected file's too.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CloudSyncQuery {
    /// **The file document a surface is explaining right now** — its id, not
    /// its address.
    ///
    /// A workspace file *is* a document whose root node is the blob, and only
    /// the store can turn one into the other: a window would have to fetch
    /// and decode the file's snapshot to learn the address, per click, on the
    /// thread that has to stay ready. Sending what the window already has and
    /// letting the read model do one indexed lookup is the same answer for
    /// none of that cost.
    ///
    /// Absent when the selection is not a file.
    #[serde(default)]
    pub file: Option<String>,
}

/// **One file, as the context pane explains it** (D259 §4).
///
/// A projection of what the engine already holds for one address: the local
/// record the Store has, and what the last coverage pass found on Cloud. Not
/// a second subsystem and not a live probe — a selection must cost a lookup,
/// not a round trip to a region.
///
/// **Metadata a person named is in here**, unlike every other report in this
/// module: a filename and a media type are exactly the facts the pane is for.
/// Nothing else about this route changes — it is already scoped to a caller
/// who may read the document the file is in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BlobFileReport {
    /// The address asked about, echoed so a stale answer is recognisable.
    pub hash: String,
    /// Bytes, when this device holds the object. `None` is not zero: a file
    /// that has never been here has no local size, and a guessed number is
    /// worse than an absent one.
    #[serde(default)]
    pub size: Option<u64>,
    /// The media type the local record declares.
    #[serde(default)]
    pub mime: Option<String>,
    /// The name the person's file had. Metadata, never identity.
    #[serde(default)]
    pub filename: Option<String>,
    /// **Are the bytes on this device**, and may this workspace read them.
    pub here: bool,
    /// What Cloud held at the last coverage pass — `durable`, `uploading`,
    /// `missing`, or absent when nothing has measured this address yet.
    /// Absent is **not** `missing`.
    #[serde(default)]
    pub cloud: Option<String>,
    /// Why blob transfer is holding this workspace, if it is — the same code
    /// [`BlobTransferReport::pending_reason`] carries, repeated here so the
    /// pane explaining one file has the reason beside the file.
    #[serde(default)]
    pub hold: Option<String>,
}

/// Configured Workspace Cloud policy and optional live permit progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CloudSyncState {
    /// Saved Workspace mode, capped by its Profile's saved mode, allows Cloud.
    /// This remains true between permits; it does not grant transport authority.
    pub enabled: bool,
    /// The permit's stage as the driver last set it — `reconciling`,
    /// `settled`, `unavailable`, and the rest of `CloudStage`. None between permits.
    #[serde(default)]
    pub stage: Option<String>,
    /// The stage's stable lowercase code, never a message.
    #[serde(default)]
    pub error_code: Option<String>,
}

/// The coalesced per-document record. Times are epoch milliseconds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CloudDocumentActivity {
    #[serde(default)]
    pub last_started_at: Option<i64>,
    #[serde(default)]
    pub last_settled_at: Option<i64>,
    #[serde(default)]
    pub last_uploaded_at: Option<i64>,
    #[serde(default)]
    pub last_downloaded_at: Option<i64>,
    #[serde(default)]
    pub last_deferred_at: Option<i64>,
    /// The last refusal this document's dataset ended on, with its moment.
    #[serde(default)]
    pub last_refusal_code: Option<String>,
    #[serde(default)]
    pub last_refused_at: Option<i64>,
}

/// The coalesced per-dataset record — one pass, the newest one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CloudPassActivity {
    /// Confirmed progress for the unchanged inventory, retained across rechecks.
    /// Separate from this pass's counters; never persisted across restarts.
    #[serde(default)]
    pub confirmed_documents: Option<CloudDocumentProgress>,
    /// Current manifest inventory, including the Workspace manifest itself.
    /// Unknown until that manifest has been reconciled and read locally.
    #[serde(default)]
    pub documents_required: Option<u32>,
    /// Documents verified in both directions without a refusal or deferral.
    /// This is progress for this pass, not durable coverage across restarts.
    #[serde(default)]
    pub documents_synced: u32,
    /// A committed change invalidated this pass's inventory/state sample.
    /// Cleared only by starting another pass, never by finishing this one.
    #[serde(default)]
    pub documents_changed: bool,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub finished_at: Option<i64>,
    pub settled: u32,
    pub uploaded: u32,
    pub downloaded: u32,
    pub deferred: u32,
    /// The worker says it has more to exchange, so the Agent reschedules.
    pub remote_hint: bool,
    #[serde(default)]
    pub refusal_code: Option<String>,
    #[serde(default)]
    pub refused_at: Option<i64>,
}

/// A lower bound established by observed passes over an unchanged inventory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CloudDocumentProgress {
    pub required: u32,
    pub synced: u32,
    /// A whole pass finished without changes, hints, deferrals or refusals.
    pub complete: bool,
}

/// Which replica path supplied this document's newest local update.
///
/// The same four cases as `koru_store_core::ports::UpdateOrigin`, flattened to
/// what a person can be told. The relationship and endpoint bytes are
/// deliberately **not** carried: they name a trust edge and a route, and
/// neither turns into a sentence.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloudChangeOrigin {
    /// The store itself wrote it — an import, a restore, a lifecycle move.
    Store,
    /// A client of this store: the app on this device.
    ThisDevice,
    /// Another of the person's own devices, over the peer path.
    PeerDevice,
    /// Koru Cloud delivered it.
    Cloud,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CloudLocalChange {
    /// `loro_documents.updated_at` — when the document last moved on disk.
    pub changed_at: i64,
    pub origin: CloudChangeOrigin,
    /// For [`CloudChangeOrigin::PeerDevice`] only: the peer's device id, in
    /// the same canonical uuid spelling the Agent's linked-device list uses.
    ///
    /// **An id, not a name.** Display names live in the Agent's trust records
    /// and this store holds none of them; the surface showing this resolves
    /// the name against the device list it already has, and says "another
    /// device" when it cannot. Returning the id here is what makes that
    /// possible without the store learning the trust model.
    #[serde(default)]
    pub device_id: Option<String>,
}

/// `GET /v1/metrics`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Metrics {
    pub uptime_ms: i64,
    pub workspaces_known: usize,
    pub workspaces_open: usize,
    pub documents_open: usize,
    pub rooms_occupied: usize,
    pub subscribers: usize,
    pub recoveries: usize,
}
