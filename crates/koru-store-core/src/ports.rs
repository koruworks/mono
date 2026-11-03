// SPDX-License-Identifier: MPL-2.0

//! The traits a persistence layer implements.
//!
//! Everything below this line is `koru-store-sqlite`'s business. Core states
//! *what* must be stored and *what must be atomic*; it says nothing about SQL,
//! files or directories — which is what lets a mobile build swap the backing
//! store without the lifecycle noticing, and what keeps the physical layout
//! from leaking into the API.
//!
//! ## The two atomicity requirements
//!
//! These are the whole reason these are traits rather than free functions.
//! Both are load-bearing for the durability invariant, and an implementation
//! that satisfies the signature but not the transaction is a data-loss bug that
//! type-checks:
//!
//! - [`WorkspaceRepo::commit_updates`] — the updates **and** the document's new
//!   position land together, or neither does. A journal ahead of its position
//!   replays work already applied; a position ahead of its journal claims
//!   durability for bytes that are gone.
//! - [`WorkspaceRepo::commit_checkpoint`] — writing the checkpoint **and**
//!   pruning what it covers are one commit. Between them is the one window in
//!   which both copies of a change could be absent.
//!
//! ## Five ports, not one
//!
//! Split by responsibility so a component can be replaced alone: the catalog
//! knows nothing of Loro, the blob store knows nothing of documents, and
//! [`DocumentIndex`] is deliberately separate from [`WorkspaceRepo`] because it
//! is *derived* — a rebuildable projection of the manifest, and the seam where
//! a real search index arrives later as a swap rather than a migration.

use crate::errors::Result;
use crate::ids::{ClientId, DocumentId, DocumentVersionId, HistoryEntryId, ProfileId, WorkspaceId};
use crate::model::{
    ApiClient, Blob, BlobHash, DocumentEntry, DocumentKind, Lineage, Profile, Timestamp, Workspace,
};
use crate::profile_account::ProfileAccountBinding;
use crate::profile_units::ProfileUnitRef;
use crate::transport::{
    TransportModeCommit, TransportModeGroup, TransportModeRequest, TransportModeState,
    TransportSubject,
};

// ------------------------------------------------------------------ catalog

/// Result of hiding one Profile from this installation's Store catalog. Its row
/// and CRDT remain durable so Workspace tombstones keep their owner and recovery
/// can be explicit; settings outside the Store are deliberately not covered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalProfileRemoval {
    Removed,
    AlreadyRemoved,
}

/// Result of making one retained Profile visible on this installation again.
/// Workspaces keep their existing active/deleted state; this changes only the
/// explicit Profile catalog marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalProfileRestoration {
    Restored,
    AlreadyActive,
}

/// Visibility state of an exact Profile row, including recovery-only rows that
/// ordinary Profile catalog reads deliberately hide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileCatalogState {
    Active,
    Removed,
}

/// Content-free retained Profile row used only to reconcile local presence
/// with an exact hosted Koru Identity. Removed rows remain visible here while
/// ordinary Profile listings continue to hide them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileLifecycleInventoryItem {
    pub profile: ProfileId,
    pub account_binding: ProfileAccountBinding,
    pub removed_at: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileCrdtState {
    pub profile: ProfileId,
    pub snapshot: Vec<u8>,
}

/// One independently addressed authoritative Profile document. Only personal
/// units carry the disposable reminder index stamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileUnitState {
    pub unit: ProfileUnitRef,
    pub snapshot: Vec<u8>,
    pub indexed_version: Option<Vec<u8>>,
}

/// Device-neutral reminder lookup hint, never proof that a target is absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderDue {
    pub owner: ProfileId,
    pub workspace: WorkspaceId,
    pub reminder: crate::ids::ReminderId,
    pub due_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncDocumentDigest {
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    /// The authoritative Store revision used to derive `digest`. Keeping it
    /// beside disposable state lets startup repair detect a crash between a
    /// document commit and this cache update.
    pub source_revision: Vec<u8>,
    pub digest: Vec<u8>,
}

/// Metadata about one durable state change accepted from a linked device.
/// Content bytes never enter this record; names are display snapshots only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncStateEvent {
    /// SQLite-assigned insertion order. `None` before the event is stored.
    pub id: Option<i64>,
    pub received_at: Timestamp,
    pub peer_device_id: String,
    pub kind: String,
    pub profile: Option<ProfileId>,
    pub workspace: Option<WorkspaceId>,
    pub document: Option<DocumentId>,
    pub document_name: Option<String>,
    pub blob_hash: Option<String>,
    pub detail: Option<String>,
    pub items: Option<usize>,
}

/// Profiles, the workspace index, lineage, credentials and store-wide state.
///
/// Knows nothing about Loro. A store with an empty catalog is a valid store —
/// this is the only thing opened at startup, and opening anything else is a
/// response to a request.
pub trait Catalog {
    fn put_profile(&mut self, profile: &Profile) -> Result<()>;
    fn profile(&self, id: ProfileId) -> Result<Option<Profile>>;
    /// Exact retained row, including one hidden by local Profile removal.
    fn profile_including_removed(&self, id: ProfileId) -> Result<Option<Profile>>;
    fn profiles(&self) -> Result<Vec<Profile>>;
    /// Every retained Profile row, **including** ones hidden by local removal.
    ///
    /// Exists for the one-time origin grandfathering in `Store::open`: a
    /// locally removed Profile is invisible to [`Self::profiles`] but is still
    /// restorable, and `restore_profile_local` is a legacy-upgrade path. Leaving
    /// it out of the grandfather would make a Profile that this installation
    /// genuinely originated offline, and then hid, unrestorable.
    ///
    /// Defaults to the visible listing, which is correct for any backend that
    /// does not implement local removal at all.
    fn profiles_including_removed(&self) -> Result<Vec<Profile>> {
        self.profiles()
    }
    /// Every account-bound Profile row, active or locally removed. Names,
    /// appearance, purpose, locale, and content never cross this projection.
    fn profile_lifecycle_inventory(&self) -> Result<Vec<ProfileLifecycleInventoryItem>>;
    /// Exact-row state for mutations that must distinguish absent from locally removed.
    fn profile_catalog_state(&self, id: ProfileId) -> Result<Option<ProfileCatalogState>>;
    /// Atomically refuse an active owner or mark an empty Profile removed.
    /// Implementations retain the Profile row, CRDT, and deleted Workspace rows.
    fn remove_profile_local(&mut self, id: ProfileId, at: Timestamp)
    -> Result<LocalProfileRemoval>;
    /// Atomically clear the explicit local-removal marker.
    fn restore_profile_local(&mut self, id: ProfileId) -> Result<LocalProfileRestoration>;
    fn profile_crdt(&self, _id: ProfileId) -> Result<Option<ProfileCrdtState>> {
        Ok(None)
    }
    /// Persist authoritative profile bytes and their listing projection as one
    /// durable operation. Backends without profile replication retain their
    /// existing row behavior through this default.
    fn commit_profile_crdt(&mut self, profile: &Profile, _snapshot: &[u8]) -> Result<()> {
        self.put_profile(profile)
    }
    /// Fresh identity/private bytes and the identity listing become visible
    /// together. Retained rows and local removal markers are never overwritten.
    fn initialize_profile_units(
        &mut self,
        profile: &Profile,
        identity: &[u8],
        private: &[u8],
    ) -> Result<()>;
    fn profile_unit(&self, unit: ProfileUnitRef) -> Result<Option<ProfileUnitState>>;
    fn profile_units(&self, profile: ProfileId) -> Result<Vec<ProfileUnitState>>;
    /// Retained personal authority prevents physical Workspace deletion,
    /// including when its owner Profile is locally removed.
    fn workspace_has_personal_units(&self, workspace: WorkspaceId) -> Result<bool>;
    /// Identity bytes and their catalog projection, including the original
    /// creation time rather than a provisioning placeholder's time, share a
    /// durable transaction. Local removal and reserved kind remain local guards.
    fn commit_profile_identity(&mut self, profile: &Profile, snapshot: &[u8]) -> Result<()>;
    /// Repair only the creation-time projection from a validated retained
    /// identity. Compare the exact source bytes atomically; a changed source or
    /// an already-correct row returns false. Never rewrite the identity bytes.
    fn reconcile_profile_creation_time(
        &mut self,
        profile: ProfileId,
        created_at: Timestamp,
        source_snapshot: &[u8],
    ) -> Result<bool>;
    /// A non-identity unit never projects legacy identity. Changed personal
    /// bytes atomically invalidate its due-index stamp.
    fn commit_profile_unit(&mut self, unit: ProfileUnitRef, snapshot: &[u8]) -> Result<()>;
    /// Persist a cross-workspace mutation of non-identity units atomically.
    fn commit_profile_units(&mut self, units: &[(ProfileUnitRef, Vec<u8>)]) -> Result<()>;
    /// Compare the exact source snapshot before atomically publishing all rows
    /// and their vector stamp. A stale rebuild returns false without writing.
    fn replace_profile_due_index(
        &mut self,
        owner: ProfileId,
        workspace: WorkspaceId,
        source_snapshot: &[u8],
        source_version: &[u8],
        rows: &[ReminderDue],
    ) -> Result<bool>;
    fn profile_reminder_due(&self, owner: ProfileId) -> Result<Vec<ReminderDue>>;
    fn put_workspace(&mut self, workspace: &Workspace) -> Result<()>;
    fn workspace(&self, id: WorkspaceId) -> Result<Option<Workspace>>;
    fn workspaces(&self) -> Result<Vec<Workspace>>;
    fn workspaces_including_deleted(&self) -> Result<Vec<Workspace>>;
    /// Replace one profile's complete workspace order atomically.
    fn reorder_workspaces(&mut self, owner: ProfileId, ids: &[WorkspaceId]) -> Result<()>;
    fn touch_workspace(&mut self, id: WorkspaceId, at: Timestamp) -> Result<()>;
    /// Forget a workspace's catalog row. Does **not** delete its data — the
    /// two are separate so an accidental unregister stays recoverable.
    fn forget_workspace(&mut self, id: WorkspaceId) -> Result<()>;
    /// Remove the catalog row and every derived reference after retention has
    /// expired and the backend bytes have been deleted.
    fn purge_workspace(&mut self, id: WorkspaceId) -> Result<()>;
    fn set_workspace_deleted(
        &mut self,
        id: WorkspaceId,
        deleted_at: Option<Timestamp>,
        documents: &[DocumentId],
    ) -> Result<()>;

    fn record_lineage(&mut self, lineage: &Lineage) -> Result<()>;
    fn lineage(&self, workspace: WorkspaceId) -> Result<Vec<Lineage>>;

    /// Store a credential. The **token itself is never stored** — only a hash,
    /// so a leaked catalog cannot be replayed against a running store.
    fn put_client(&mut self, client: &ApiClient, token_hash: &[u8]) -> Result<()>;
    fn client_by_token(&self, token_hash: &[u8]) -> Result<Option<ApiClient>>;
    fn revoke_client(&mut self, id: ClientId, at: Timestamp) -> Result<()>;
    fn clients(&self) -> Result<Vec<ApiClient>>;

    /// Store-wide values: the install id, the store's Loro `PeerID`, the last
    /// recovery. Small, opaque, and read once each.
    fn state(&self, key: &str) -> Result<Option<String>>;
    fn set_state(&mut self, key: &str, value: &str) -> Result<()>;

    /// Disposable anti-entropy leaves. They are derived from authoritative
    /// document version vectors and may be replaced wholesale after a crash or
    /// schema change.
    fn sync_document_digests(&self, _workspace: WorkspaceId) -> Result<Vec<SyncDocumentDigest>> {
        Ok(Vec::new())
    }
    fn put_sync_document_digest(&mut self, _digest: &SyncDocumentDigest) -> Result<()> {
        Ok(())
    }
    fn replace_sync_document_digests(
        &mut self,
        _workspace: WorkspaceId,
        _digests: &[SyncDocumentDigest],
    ) -> Result<()> {
        Ok(())
    }

    /// Insert one event and prune rows older than `retain_after` atomically.
    fn record_sync_state_event(
        &mut self,
        event: &SyncStateEvent,
        retain_after: Timestamp,
    ) -> Result<SyncStateEvent>;
    /// Newest-first page for one peer. `before_id` is an exclusive cursor.
    fn sync_state_events(
        &self,
        peer_device_id: &str,
        retain_after: Timestamp,
        before_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<SyncStateEvent>>;
    fn latest_sync_state_event_times(
        &self,
        retain_after: Timestamp,
    ) -> Result<Vec<(String, Timestamp)>>;
    fn prune_sync_state_events(&mut self, retain_after: Timestamp) -> Result<usize>;

    // ------------------------------------------------------------ locations

    /// Which workspace a document belongs to.
    ///
    /// A document id is globally unique, so a client can name one without
    /// knowing where it lives — `document:<id>` as a room, `GET /v1/documents/{id}`
    /// as a route. Without this index answering that would mean opening every
    /// workspace database in turn, which is the opposite of lazy.
    ///
    /// It is **derived**: every entry can be rebuilt by walking the workspaces.
    /// That matters because it means a lost or stale row costs a lookup, never
    /// a document.
    fn locate(&self, document: DocumentId) -> Result<Option<WorkspaceId>>;
    fn record_location(&mut self, document: DocumentId, workspace: WorkspaceId) -> Result<()>;
    fn forget_location(&mut self, document: DocumentId) -> Result<()>;

    // ------------------------------------------------------ transport modes

    /// One subject's device-local transport mode row (D230, D249).
    ///
    /// **A missing or undecodable row is a storage error, never a default.**
    /// Every retained Profile and Workspace gains a row when it is created and
    /// keeps it while it is retained — including a Workspace tombstone and a
    /// locally removed Profile — so an absent one means the catalog and the
    /// mode table disagree. Reading it as Local-only would look like caution
    /// and would in fact overwrite a person's explicit Cloud choice on the next
    /// compare-and-set.
    fn transport_mode(&self, subject: TransportSubject) -> Result<TransportModeState>;

    /// One Profile's row together with **every** Workspace row beneath it.
    ///
    /// All of them, not only the ones a caller means to change: a Profile
    /// downgrade is refused because of the child nobody mentioned, so the
    /// decision cannot be made from a subset. Implementations also validate
    /// each row's `owner_profile_id` against `workspaces.owner` here — the two
    /// foreign keys alone do not express that relationship.
    fn transport_mode_group(&self, owner: ProfileId) -> Result<TransportModeGroup>;

    /// Apply a group of requests as **one** decision.
    ///
    /// The atomicity is the contract, not an optimisation: a Profile and the
    /// children a downgrade needed resolved commit together or not at all, so
    /// an unrelated invalid mutation cannot leave half a ladder behind. The
    /// implementation reads the affected rows under a serialized transaction,
    /// decides with [`crate::transport::plan`], writes what it returns and
    /// commits — it does not re-decide any of those rules itself.
    ///
    /// Returns one outcome per request, in request order. All three outcome
    /// variants are successes; an `Err` means nothing was written.
    fn commit_transport_modes(
        &mut self,
        owner: ProfileId,
        requests: &[TransportModeRequest],
    ) -> Result<Vec<TransportModeCommit>>;
}

// ------------------------------------------------------------------ documents

/// Where a document has got to, as the store last wrote it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DocumentState {
    pub id: DocumentId,
    pub kind: DocumentKind,
    /// `None` before the first checkpoint: the journal alone is the document.
    pub latest_checkpoint_seq: Option<u64>,
    /// `VersionVector::encode()`. Opaque bytes here; compared with
    /// `includes_vv`, never by equality of the encoding.
    pub version_vector: Vec<u8>,
    pub frontiers: Vec<u8>,
    pub updated_at: Timestamp,
}

/// A snapshot of a document at a point in its history.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Checkpoint {
    pub seq: u64,
    pub snapshot: Vec<u8>,
    pub version_vector: Vec<u8>,
    pub frontiers: Vec<u8>,
    /// Verified on load. A checkpoint that fails this is not a checkpoint —
    /// it is the reason the previous one is kept.
    pub checksum: String,
    pub created_at: Timestamp,
}

/// A checkpoint without its bytes, for listing and for the debug routes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CheckpointHeader {
    pub seq: u64,
    pub size: u64,
    pub checksum: String,
    pub created_at: Timestamp,
}

/// What the journal holds above a sequence — see
/// [`WorkspaceRepo::journal_since`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct JournalSince {
    /// The highest sequence present, `None` when nothing is above `after`.
    pub head: Option<u64>,
    /// Total payload bytes above `after`.
    pub bytes: u64,
}

/// One update as it arrived, stored **verbatim**.
///
/// Never re-exported from the live document before writing: the bytes a peer
/// sent are the bytes that are kept, so what is replayed on recovery is what
/// was acknowledged rather than a re-encoding of it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StoredUpdate {
    pub seq: u64,
    pub bytes: Vec<u8>,
    pub received_at: Timestamp,
    /// Which replica path supplied it. For diagnosis; never an author.
    pub origin: UpdateOrigin,
}

/// Provenance for one verbatim journal update. Raw identifier bytes keep this
/// transport-neutral core independent of the device/trust crate.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateOrigin {
    Store,
    /// Durable replica identity only; never a device, route or credential.
    Cloud {
        logical_replica_id: [u8; 16],
    },
    LocalClient {
        client_id: ClientId,
    },
    RemoteDevice {
        relationship_id: [u8; 16],
        device_id: [u8; 16],
        endpoint_id: [u8; 32],
    },
}

impl<'de> serde::Deserialize<'de> for UpdateOrigin {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        // Internally tagged serde unit variants ignore extra fields even with
        // deny_unknown_fields. An empty struct gives Store the same strictness
        // as the other origins, without changing its existing serialized bytes.
        #[derive(serde::Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum Wire {
            Store {},
            Cloud {
                logical_replica_id: [u8; 16],
            },
            LocalClient {
                client_id: ClientId,
            },
            RemoteDevice {
                relationship_id: [u8; 16],
                device_id: [u8; 16],
                endpoint_id: [u8; 32],
            },
        }
        Ok(
            match <Wire as serde::Deserialize>::deserialize(deserializer)? {
                Wire::Store {} => Self::Store,
                Wire::Cloud { logical_replica_id } => Self::Cloud { logical_replica_id },
                Wire::LocalClient { client_id } => Self::LocalClient { client_id },
                Wire::RemoteDevice {
                    relationship_id,
                    device_id,
                    endpoint_id,
                } => Self::RemoteDevice {
                    relationship_id,
                    device_id,
                    endpoint_id,
                },
            },
        )
    }
}

impl Default for UpdateOrigin {
    fn default() -> Self {
        Self::Store
    }
}

impl From<Option<ClientId>> for UpdateOrigin {
    fn from(client: Option<ClientId>) -> Self {
        client.map_or(Self::Store, |client_id| Self::LocalClient { client_id })
    }
}

/// One group commit: everything accepted inside a single window.
///
/// The batch exists because the alternative is one fsync per keystroke. Every
/// update in it is acknowledged together, after the single commit — so the
/// window changes how often the disk is touched and never what an
/// acknowledgement means.
#[derive(Clone, Debug)]
pub struct UpdateBatch {
    pub document: DocumentId,
    pub updates: Vec<Vec<u8>>,
    pub version_vector: Vec<u8>,
    pub frontiers: Vec<u8>,
    pub received_at: Timestamp,
    /// Exactly one origin per update; group commits may safely mix sources.
    pub origins: Vec<UpdateOrigin>,
}

/// Why a durable, user-visible point exists. This is deliberately not an
/// author: a Store client/peer identifies a replica, never a person.
#[derive(Copy, Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryReason {
    Manual,
    AutomaticActive,
    AutomaticIdle,
    McpBefore,
    McpAfter,
    RestoreBefore,
    RestoreAfter,
    /// The storage layer took a checkpoint. Client-facing on purpose: a
    /// checkpoint is a moment the whole document was durably re-encoded, and
    /// a person scanning history can anchor on it the way they anchor on an
    /// automatic save.
    Checkpoint,
}

impl HistoryReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::AutomaticActive => "automatic_active",
            Self::AutomaticIdle => "automatic_idle",
            Self::McpBefore => "mcp_before",
            Self::McpAfter => "mcp_after",
            Self::RestoreBefore => "restore_before",
            Self::RestoreAfter => "restore_after",
            Self::Checkpoint => "checkpoint",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "manual" => Self::Manual,
            "automatic_active" => Self::AutomaticActive,
            "automatic_idle" => Self::AutomaticIdle,
            "mcp_before" => Self::McpBefore,
            "mcp_after" => Self::McpAfter,
            "restore_before" => Self::RestoreBefore,
            "restore_after" => Self::RestoreAfter,
            "checkpoint" => Self::Checkpoint,
            _ => return None,
        })
    }

    pub fn pinned(self) -> bool {
        !matches!(
            self,
            Self::AutomaticActive | Self::AutomaticIdle | Self::Checkpoint
        )
    }
}

/// Immutable CRDT state. Deduplicated by `(document, frontiers)`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DocumentVersion {
    pub id: DocumentVersionId,
    pub document: DocumentId,
    pub snapshot: Vec<u8>,
    pub version_vector: Vec<u8>,
    pub frontiers: Vec<u8>,
    pub checksum: String,
    pub captured_at: Timestamp,
}

/// A user-visible marker referring to immutable state.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HistoryEntry {
    pub id: HistoryEntryId,
    pub document: DocumentId,
    pub version: DocumentVersionId,
    pub reason: HistoryReason,
    pub recorded_at: Timestamp,
    pub label: Option<String>,
    pub message: Option<String>,
    pub boundary: Option<String>,
    pub pinned: bool,
}

/// Input to the one transaction which deduplicates state and appends a marker.
pub struct HistoryCapture<'a> {
    pub document: DocumentId,
    pub snapshot: &'a [u8],
    pub version_vector: &'a [u8],
    pub frontiers: &'a [u8],
    pub checksum: &'a str,
    pub reason: HistoryReason,
    pub recorded_at: Timestamp,
    pub label: Option<&'a str>,
    pub message: Option<&'a str>,
    pub boundary: Option<&'a str>,
}

/// A complete opaque document planted before its manifest entry is published.
///
/// The repository owns idempotency because a bridge process can disappear
/// between receiving the durable acknowledgement and returning it to its MCP
/// host. `scope` is the authenticated caller within this workspace; `key` is
/// caller supplied. `request_hash` compares semantic requests without asking
/// the Store to understand the opaque Loro update.
#[derive(Debug)]
pub struct SeedDocument {
    pub candidate: DocumentId,
    /// Whether the caller supplied this identity. Such a candidate may recover
    /// its own content-first seed after the transport receipt expires; the
    /// stored first update remains authoritative over any later retry body.
    pub recover_candidate: bool,
    pub scope: String,
    pub key: String,
    pub request_hash: String,
    pub initial_update: Vec<u8>,
    pub version_vector: Vec<u8>,
    pub frontiers: Vec<u8>,
    pub created_at: Timestamp,
    /// Records older than this may be replaced by a new request using the key.
    pub expires_before: Timestamp,
}

/// The durable content associated with an idempotent creation request.
#[derive(Debug)]
pub struct SeededDocument {
    pub document: DocumentId,
    /// The original bytes, including on replay, so an adapter can reproduce
    /// the exact success projection rather than reading a document changed
    /// since its creation.
    pub initial_update: Vec<u8>,
    pub replayed: bool,
}

/// One workspace's Loro state.
pub trait WorkspaceRepo {
    fn documents(&self) -> Result<Vec<DocumentState>>;
    fn document(&self, id: DocumentId) -> Result<Option<DocumentState>>;
    fn create_document(&mut self, id: DocumentId, kind: DocumentKind, at: Timestamp) -> Result<()>;
    /// **One transaction:** plant the opaque initial document and its
    /// idempotency record, or replay the original bytes for the same request.
    fn seed_document(&mut self, _seed: &SeedDocument) -> Result<SeededDocument> {
        Err(crate::errors::Error::new(
            crate::errors::Code::StorageFailed,
            "seeded document creation is not implemented by this repository",
        ))
    }
    /// Drop a document's rows entirely. Separate from unregistering it in the
    /// manifest, and always the later of the two.
    fn drop_document(&mut self, id: DocumentId) -> Result<()>;

    /// Newest first.
    fn checkpoints(&self, id: DocumentId) -> Result<Vec<CheckpointHeader>>;
    fn checkpoint(&self, id: DocumentId, seq: u64) -> Result<Option<Checkpoint>>;
    /// The newest checkpoint **whose checksum verifies**. Recovery walks
    /// backwards from here; a damaged one is skipped, not returned.
    fn latest_sound_checkpoint(&self, id: DocumentId) -> Result<Option<Checkpoint>>;

    /// Updates above `after`, in sequence order. `None` means from the start.
    fn updates_after(&self, id: DocumentId, after: Option<u64>) -> Result<Vec<StoredUpdate>>;

    /// Where the newest journal update for this document came from, and when
    /// it arrived. `None` for a document whose journal is empty — every update
    /// is already inside a checkpoint, or nothing was ever written.
    ///
    /// **Deliberately not `updates_after(id, None).last()`.** That reads every
    /// update's bytes to answer a question about one row's provenance, which
    /// on a long journal is the whole document; this is one indexed row and no
    /// payload at all.
    fn latest_update_origin(&self, id: DocumentId) -> Result<Option<(Timestamp, UpdateOrigin)>>;
    fn update_count(&self, id: DocumentId) -> Result<u64>;

    /// The journal above `after` (`None` = from the start), as bookkeeping
    /// rather than bytes: the highest sequence present and the total payload
    /// size. This is what the checkpoint policy accumulates over — reading
    /// the updates themselves to find out whether one is due would cost what
    /// the checkpoint exists to avoid.
    fn journal_since(&self, id: DocumentId, after: Option<u64>) -> Result<JournalSince>;

    /// **One transaction.** Returns the highest sequence written.
    fn commit_updates(&mut self, batch: &UpdateBatch) -> Result<u64>;

    /// **One transaction**: write the checkpoint, move the document's
    /// `latest_checkpoint_seq`, and delete the updates it covers. Returns how
    /// many updates were pruned.
    fn commit_checkpoint(&mut self, id: DocumentId, checkpoint: &Checkpoint) -> Result<usize>;

    /// **One transaction:** reuse or insert immutable state, then append the
    /// marker which refers to it.
    fn capture_history(&mut self, _capture: &HistoryCapture<'_>) -> Result<HistoryEntry> {
        Err(crate::errors::Error::new(
            crate::errors::Code::StorageFailed,
            "history is not implemented by this repository",
        ))
    }
    /// Newest first, with an exclusive `(recorded_at, id)` cursor.
    fn history_entries(
        &self,
        _id: DocumentId,
        _before: Option<(Timestamp, HistoryEntryId)>,
        _limit: usize,
    ) -> Result<Vec<HistoryEntry>> {
        Err(crate::errors::Error::new(
            crate::errors::Code::StorageFailed,
            "history is not implemented by this repository",
        ))
    }
    fn history_entry(&self, _id: HistoryEntryId) -> Result<Option<HistoryEntry>> {
        Err(crate::errors::Error::new(
            crate::errors::Code::StorageFailed,
            "history is not implemented by this repository",
        ))
    }
    /// The encoded version vector of the state the *newest* marker names,
    /// **without its snapshot bytes**. `Ok(None)` when the document has no
    /// markers.
    ///
    /// Its own method rather than `history_entries().first()` plus
    /// `document_version`, because that path reads a whole snapshot — megabytes
    /// — to compare a few dozen bytes.
    ///
    /// **The default is `Ok(None)`, not an error**, unlike its neighbours: "I
    /// cannot say" is a legitimate answer that travels the wire as
    /// `stale: null`, and failing a whole history listing because a repository
    /// keeps no markers would be the worse trade.
    fn newest_history_version(&self, _id: DocumentId) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn document_version(&self, _id: DocumentVersionId) -> Result<Option<DocumentVersion>> {
        Err(crate::errors::Error::new(
            crate::errors::Code::StorageFailed,
            "history is not implemented by this repository",
        ))
    }
    /// Rename a marker. The label lives on the marker, never on the immutable
    /// state it names — renaming a snapshot changes nothing about the bytes.
    fn relabel_history_entry(
        &mut self,
        _id: HistoryEntryId,
        _label: Option<&str>,
    ) -> Result<HistoryEntry> {
        Err(crate::errors::Error::new(
            crate::errors::Code::StorageFailed,
            "history is not implemented by this repository",
        ))
    }

    fn put_blob(&mut self, blob: &Blob) -> Result<()>;
    fn blob(&self, hash: &BlobHash) -> Result<Option<Blob>>;
    fn blobs(&self) -> Result<Vec<Blob>>;

    /// Record who a peer's advertisement said first held a blob, when we hold
    /// no row for it ourselves. First record wins: provenance is who *first*
    /// held the bytes, never last touch. Never consulted for routing.
    ///
    /// A default no-op keeps the trait's test doubles honest without each of
    /// them growing a table they would never read.
    fn record_blob_origin(
        &mut self,
        _hash: &BlobHash,
        _device: &str,
        _at: Timestamp,
    ) -> Result<()> {
        Ok(())
    }
    /// The origin learned from an advertisement, when no `blobs` row carries it.
    fn advertised_blob_origin(&self, _hash: &BlobHash) -> Result<Option<String>> {
        Ok(None)
    }

    // ------------------------------------------------- document-private state

    /// A reader's private snapshot of a document's view state, or `None`
    /// when they never changed it. Device-local; never replicated.
    fn private_snapshot(
        &self,
        _document: DocumentId,
        _owner: ProfileId,
    ) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    /// Replace one reader's private snapshot in one transaction. The caller
    /// has already validated and merged it.
    fn put_private_snapshot(
        &mut self,
        _document: DocumentId,
        _owner: ProfileId,
        _snapshot: &[u8],
        _at: Timestamp,
    ) -> Result<()> {
        Err(crate::errors::Error::new(
            crate::errors::Code::StorageFailed,
            "document-private state is not implemented by this repository",
        ))
    }
    /// Every reader's private snapshot of one document, for a transfer that
    /// must carry them before the source row is dropped.
    fn private_snapshots(&self, _document: DocumentId) -> Result<Vec<(ProfileId, Vec<u8>)>> {
        Ok(Vec::new())
    }
    /// Delete private rows whose document is not in `listed` — the documents
    /// the manifest still names, trashed or not. Idempotent. Returns how many
    /// rows went.
    fn prune_private_snapshots(&mut self, _listed: &[DocumentId]) -> Result<usize> {
        Ok(0)
    }

    /// Bytes on disk, for the debug routes. Metadata, never content.
    fn size_bytes(&self) -> Result<u64>;
}

// ------------------------------------------------------------------ index

/// The derived listing of a workspace's documents.
///
/// **Rebuildable, always.** The manifest is the truth; this is a projection of
/// it kept so that listing a workspace does not mean opening everything in it.
/// It is its own port precisely because it is the seam a real full-text index
/// arrives at: language-specific tokenisation then becomes a reindex behind
/// this trait rather than a migration of stored state.
pub trait DocumentIndex {
    /// Replace the projection wholesale. Idempotent by construction — the
    /// manifest is re-walked rather than diffed, because a diff that drifts is
    /// a listing that quietly stops matching the document.
    fn reindex(&mut self, entries: &[DocumentEntry]) -> Result<()>;
    fn entries(&self) -> Result<Vec<DocumentEntry>>;
    fn entry(&self, id: DocumentId) -> Result<Option<DocumentEntry>>;
}

// ------------------------------------------------------------------ blobs

/// Content-addressed bytes, outside Loro.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CompletedBlobUpload {
    pub hash: BlobHash,
    pub size: u64,
}

/// An upload whose bytes are not visible at their content address until it is
/// complete. Implementations clean up unfinished work when this value drops.
pub trait PendingBlobUpload: Send {
    /// Append one bounded transport chunk and extend the incremental hash.
    fn write_chunk(&mut self, bytes: &[u8]) -> Result<()>;

    /// Atomically publish the completed bytes and return their actual address.
    fn finish(self: Box<Self>) -> Result<CompletedBlobUpload>;
}

/// The largest individual blob accepted by the Store.
pub const BLOB_UPLOAD_BODY_LIMIT: u64 = 2 * 1024 * 1024 * 1024;

pub trait BlobStore {
    /// Start an owned upload. The returned value may outlive the Store lock.
    fn begin(&self) -> Result<Box<dyn PendingBlobUpload>>;

    /// Hash and write. Returns the hash the bytes actually have — the caller
    /// does not get to assert one.
    fn put(&self, bytes: &[u8]) -> Result<BlobHash> {
        let mut pending = self.begin()?;
        pending.write_chunk(bytes)?;
        Ok(pending.finish()?.hash)
    }
    fn get(&self, hash: &BlobHash) -> Result<Vec<u8>>;
    /// The bytes `[offset, offset + len)`, without materialising the rest.
    ///
    /// Serving a blob to a peer walks it in chunks, so a whole-file read per
    /// chunk is quadratic in the blob's size — 32 reads of 32 MiB to send one
    /// 32 MiB file. An implementation that can seek should. The default is
    /// correct and slow, which is the right way round for a port with one
    /// serious implementor.
    ///
    /// A short read at the end is not an error: the caller learns the real
    /// length from the blob's metadata, not from this.
    fn read_range(&self, hash: &BlobHash, offset: u64, len: usize) -> Result<Vec<u8>> {
        let bytes = self.get(hash)?;
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let end = start.saturating_add(len).min(bytes.len());
        Ok(bytes[start..end].to_vec())
    }
    fn has(&self, hash: &BlobHash) -> Result<bool>;
    /// A reader over the stored bytes, without materialising them.
    ///
    /// **Uploads stream.** `get` returns a `Vec`, and a 2 GiB `Vec` on the
    /// Store dispatcher is a memory event, a latency event, and on a phone an
    /// out-of-memory kill — which is why `cloud-blob-object-v1` §9 froze this
    /// alongside `get_blob` rather than leaving a caller to choose.
    ///
    /// Unverified, deliberately: whoever streams a blob calls `verify` once
    /// first, because re-hashing per chunk would be O(size²). The default is
    /// correct and slow, which is the right way round for a port with one
    /// serious implementor.
    fn open(&self, hash: &BlobHash) -> Result<Box<dyn std::io::Read + Send>> {
        Ok(Box::new(std::io::Cursor::new(self.get(hash)?)))
    }
    /// Re-hash what is on disk and compare. `false` means the bytes rotted.
    fn verify(&self, hash: &BlobHash) -> Result<bool>;
    /// Delete the local bytes of a blob, keeping nothing behind.
    ///
    /// **The one deliberately destructive operation on this port, and the
    /// caller owns the safety.** Every other method here refuses to lose
    /// bytes — `quarantine` copies rather than moves for exactly that reason.
    /// This exists so a person can reclaim space on a device that is not the
    /// only place the bytes live, and the Store cannot check that: whether a
    /// blob is durable somewhere else is Cloud's answer, not the disk's.
    ///
    /// The association is untouched, so the document still references the
    /// blob and the next read reports `BlobNotFound` — the same state as a
    /// device that has never held it, which is what makes the bytes fetchable
    /// again rather than gone.
    ///
    /// Removing what is already absent is success: the caller asked for the
    /// bytes to be gone and they are.
    fn release(&self, hash: &BlobHash) -> Result<()>;

    /// Set a damaged blob aside **by copying it**, never by moving or deleting:
    /// recovery must not be able to destroy the thing it was called to
    /// preserve. Returns an opaque location for the report — not a path a
    /// client may parse.
    fn quarantine(&self, hash: &BlobHash) -> Result<String>;
}

/// What happened when a legacy per-Workspace file was offered to the object
/// store.
///
/// Bytes used to be filed under `<root>/workspaces/<id>/blobs/<hash>`. They are
/// not moved: the object store **copies**, and the old file stays intact so an
/// older build still finds it. This value is how the service tells the three
/// outcomes apart, because they are three different pieces of news and only one
/// of them is silence.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LegacyPromotion {
    /// No file for that Workspace and hash. The ordinary answer on a store that
    /// never held the old layout, so it says nothing.
    NoCandidate,
    /// The object is present, either because this call installed it or because
    /// somebody else installed the identical bytes first.
    Installed,
    /// A file was there and its bytes do not hash to the name it sits under.
    /// **Nothing was installed and the file was not touched** — it is the only
    /// copy of whatever it really holds. A caller reports this; it never
    /// swallows it, because a picture silently becoming a different picture is
    /// the failure content addressing exists to prevent.
    Mismatch { found: BlobHash },
}

// ------------------------------------------------------------------ backups

/// An opaque handle to one backup. Not a path.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BackupId(pub String);

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BackupInfo {
    pub id: BackupId,
    pub taken_at: Timestamp,
    pub size: u64,
}

/// Local recovery copies.
///
/// A backup is **not valid because it was written**. Every implementation is
/// tested by restoring and opening the artefact, never by checking that a file
/// appeared.
pub trait Backups {
    /// A consistent copy of a live database, taken without stopping writes.
    fn take(&self, at: Timestamp) -> Result<BackupId>;
    /// Newest first.
    fn list(&self) -> Result<Vec<BackupInfo>>;
    fn restore(&self, id: &BackupId) -> Result<()>;
    /// Keep the newest `keep` and remove the rest. Returns how many went.
    fn prune(&self, keep: usize) -> Result<usize>;
}

// ------------------------------------------------------------------ opening

/// How a document or a database came back — **a value, not a log line**.
///
/// A caller cannot ignore a recovery by accident: it is in the return type, so
/// reporting it is the default and swallowing it is a deliberate act.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Opened<T> {
    /// Nothing was there. A genuine first run.
    Fresh(T),
    /// Read as written.
    Normal(T),
    /// Something did not decode, and this is what was salvaged.
    Recovered(T, Recovery),
}

impl<T> Opened<T> {
    pub fn value(&self) -> &T {
        match self {
            Self::Fresh(v) | Self::Normal(v) => v,
            Self::Recovered(v, _) => v,
        }
    }

    pub fn into_value(self) -> T {
        match self {
            Self::Fresh(v) | Self::Normal(v) => v,
            Self::Recovered(v, _) => v,
        }
    }

    pub fn recovery(&self) -> Option<&Recovery> {
        match self {
            Self::Recovered(_, r) => Some(r),
            _ => None,
        }
    }
}

/// What went wrong, and what was done about it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Recovery {
    /// Developer text. Never customer content.
    pub problem: String,
    /// Which checkpoint was fallen back to. `None` means the journal alone
    /// rebuilt the document.
    pub from_checkpoint: Option<u64>,
    /// Opaque locations of anything set aside, for the report.
    pub quarantined: Vec<String>,
    /// Updates that would not apply and were skipped.
    pub skipped_updates: usize,
}

#[cfg(test)]
mod tests;
