// SPDX-License-Identifier: MPL-2.0

//! **Every operation the store can perform, with no transport in sight.**
//!
//! HTTP is an adapter over this. So is the WebSocket. So, later, is a named
//! pipe or shared memory — and when one arrives, no operation here changes,
//! which is what "the public API is transport-independent" has to mean if it is
//! to mean anything.
//!
//! It is also the rule that keeps Koru an API platform rather than an app with
//! an API bolted on: **the first-party UI has no privileged path.** If the
//! editor can do it, it does it through a method on this type, and every method
//! here has a route.
//!
//! ## Lazy, deliberately
//!
//! Opening the store opens `catalog.db` and nothing else. A workspace database
//! is opened the first time something asks for that workspace; a document is
//! loaded the first time something asks for that document. Start-up therefore
//! costs the same on a store holding one workspace and one holding a thousand.
//!
//! ## The durability cycle
//!
//! ```text
//! apply_update   import into the live doc — validates, nothing written yet
//!      │
//! flush          one transaction, one fsync, returns the acknowledged sequence
//!      │
//!    (caller acks, then broadcasts — in that order, never the reverse)
//! ```
//!
//! Splitting those two is what makes group commit possible: many
//! `apply_update`s share one `flush`. The caller decides when to flush, because
//! the caller is the one holding a clock.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::bundle::{
    Bundle, BundleDocument, BundleHistoryEntry, BundleHistoryVersion, BundleManifest, ImportMode,
};
use crate::errors::{Code, Error, Result};
use crate::ids::{
    ActionId, ClientId, DocumentId, DocumentVersionId, HistoryEntryId, ProfileId, ReminderId,
    RuleRevisionId, WorkspaceId,
};
use crate::lifecycle::{CheckpointPolicy, CommitWindow, LiveDocument};
use crate::manifest::Manifest;
use crate::model::{
    Blob, BlobHash, DocumentEntry, DocumentKind, Lineage, Profile, Timestamp, Workspace,
};
use crate::personal_attributes::EntityRef;
use crate::ports::SyncStateEvent;
use crate::ports::{
    BlobStore, Catalog, CompletedBlobUpload, DocumentIndex, DocumentVersion, HistoryCapture,
    HistoryEntry, HistoryReason, LegacyPromotion, Opened, PendingBlobUpload, ProfileCatalogState,
    SeedDocument, SyncDocumentDigest, WorkspaceRepo,
};
use crate::profile_account::{ProfileAccountBinding, ProfileAccountBindingOutcome};
use crate::profile_manifest::ProfileManifest;
use crate::profile_units::{IdentityManifest, PrivateManifest, ProfileUnitRef};
use crate::recovery::open_document;
use crate::reminders::{
    OccurrenceAction, ReminderChange, ReminderReductionInput, ReminderRule, ReminderSettings,
    RuleStatus, StoredReminder,
};
use crate::text::Locale;

mod document_private;
mod profile_projection;
mod profile_units;
mod transport_modes;
pub use transport_modes::TransportModeChoice;
mod updates;
mod welcome_progress;
pub use profile_units::{build_reminder_index, read_personal_reminder_inputs};
pub use welcome_progress::{WELCOME_QUERY_MAX, valid_welcome_row};

/// Where a workspace's own storage comes from.
///
/// The seam that keeps SQL out of this file: core says "give me the storage for
/// this workspace" and a backend decides what that means. A mobile build can
/// answer differently without a single operation below noticing.
pub trait Backend {
    type Workspace: WorkspaceRepo + DocumentIndex;
    type Blobs: BlobStore;

    /// Open (or create) one workspace's storage.
    fn open_workspace(&self, id: WorkspaceId) -> Result<Self::Workspace>;

    /// Permanently remove one workspace's repository and blobs.
    fn delete_workspace(&self, id: WorkspaceId) -> Result<()>;

    /// **The** blob store — one per installation, not one per Workspace.
    ///
    /// Separate from the workspace repository because blobs are a different
    /// medium with a different failure mode: bytes on a filesystem, addressed
    /// by content, with no transaction around them. Keeping them apart is what
    /// would let a later build put them somewhere else entirely — a cache, a
    /// cloud bucket — without touching a line of the document path.
    ///
    /// It takes no [`WorkspaceId`] because **it is not authority**. A picture
    /// in two Workspaces is one file; which Workspaces may read it is the
    /// `workspace.db.blobs` row, checked by the operations below and by nobody
    /// else (`ADR-000-blobobjectworker`).
    fn objects(&self) -> Result<Self::Blobs>;

    /// Offer a Workspace's legacy per-Workspace file to the object store.
    ///
    /// The layout migration is lazy — start-up hashes nothing — so this is
    /// asked only on a global miss, and only for the Workspace that is asking.
    /// The candidate is **copied**, never moved: the old file stays where an
    /// older build would look for it, which is what makes this rung
    /// reversible.
    ///
    /// The default is `NoCandidate`, because a backend with no legacy layout
    /// has nothing to promote and should not have to say so in code.
    fn promote_legacy_object(
        &self,
        _workspace: WorkspaceId,
        _hash: &BlobHash,
    ) -> Result<LegacyPromotion> {
        Ok(LegacyPromotion::NoCandidate)
    }
}

/// A workspace the store is currently holding open.
///
/// **The manifest is held once, as a [`Manifest`] — never also as a
/// [`LiveDocument`].** An earlier shape kept both, and they were two `LoroDoc`s
/// of the same document: an update applied to one left the other stale, so a
/// listing and the thing it listed could disagree with nothing to detect it.
/// Every operation that can name a document therefore asks
/// [`Open::is_manifest`] first, and there is exactly one copy of the answer.
struct Open<W> {
    repo: W,
    manifest: Manifest,
    /// Only the documents somebody has asked for — and never the manifest.
    documents: HashMap<DocumentId, LiveDocument>,
    windows: HashMap<DocumentId, CommitWindow>,
    /// The manifest's own document id, so it is addressable like any other.
    manifest_id: DocumentId,
}

struct WorkspaceManifestProjection {
    name: Option<String>,
    icon: Option<Option<String>>,
    color: Option<Option<String>>,
    pinned: Option<bool>,
    archived: Option<bool>,
    deleted_at: Option<Option<Timestamp>>,
}

/// Take a checkpoint of a document's current position.
///
/// Failure never propagates — a checkpoint is an optimisation and must not
/// fail the operation that wanted one, whether that is the commit carrying
/// someone's work or the open that just replayed the journal — but it is
/// logged: a checkpoint that silently never lands is a journal that grows
/// forever, which is the bug this whole path exists to prevent.
fn take_checkpoint<W: crate::ports::WorkspaceRepo>(
    workspace: WorkspaceId,
    open: &mut Open<W>,
    document: DocumentId,
    head: u64,
    at: Timestamp,
    why: &'static str,
) {
    let Some((version_vector, frontiers)) = open.position(document) else {
        return;
    };
    let snapshot = if open.is_manifest(document) {
        open.manifest.export_snapshot().ok()
    } else {
        open.documents
            .get(&document)
            .and_then(|live| live.export_snapshot().ok())
    };
    if let Some(snapshot) = snapshot {
        let span = crate::stats::Span::start(crate::stats::names::CHECKPOINT);
        let snapshot_bytes = snapshot.len();
        let manifest = open.is_manifest(document);
        let checkpoint = crate::ports::Checkpoint {
            seq: head,
            checksum: crate::lifecycle::checksum(&snapshot),
            snapshot,
            version_vector,
            frontiers,
            created_at: at,
        };
        match open.repo.commit_checkpoint(document, &checkpoint) {
            Ok(pruned) => {
                tracing::info!(
                    workspace = %workspace,
                    document = %document,
                    seq = head,
                    snapshot_bytes,
                    pruned,
                    manifest,
                    why,
                    "checkpoint taken"
                );
                // The client-facing record: a checkpoint is a moment the
                // whole document was durably re-encoded, so it earns a row
                // in the history a person scans — deduplicated by frontiers,
                // so an unchanged document costs a marker, not a snapshot.
                // Soft like the checkpoint itself, but never silent. The
                // manifest is exempt: it is not a document anyone's history
                // view lists, and its snapshots would only grow the version
                // store.
                if !manifest {
                    let capture = crate::ports::HistoryCapture {
                        document,
                        snapshot: &checkpoint.snapshot,
                        version_vector: &checkpoint.version_vector,
                        frontiers: &checkpoint.frontiers,
                        checksum: &checkpoint.checksum,
                        reason: crate::ports::HistoryReason::Checkpoint,
                        recorded_at: at,
                        label: None,
                        message: None,
                        boundary: None,
                    };
                    if let Err(error) = open.repo.capture_history(&capture) {
                        tracing::warn!(
                            workspace = %workspace,
                            document = %document,
                            error = %error,
                            "checkpoint history entry failed"
                        );
                    }
                }
            }
            Err(error) => tracing::warn!(
                workspace = %workspace,
                document = %document,
                seq = head,
                manifest,
                why,
                error = %error,
                "checkpoint failed; the journal remains complete"
            ),
        }
        span.stop();
    }
}

/// Take the overdue checkpoint for a document that was just loaded by
/// replaying its journal. Loading is the repair point: the replay just paid
/// is the last one of that size, and a document nobody edits again would
/// otherwise never receive its first checkpoint at all.
fn checkpoint_if_overdue<W: crate::ports::WorkspaceRepo>(
    policy: &crate::lifecycle::CheckpointPolicy,
    workspace: WorkspaceId,
    open: &mut Open<W>,
    document: DocumentId,
) {
    let since_seq = open
        .repo
        .document(document)
        .ok()
        .flatten()
        .and_then(|state| state.latest_checkpoint_seq);
    let Ok(since) = open.repo.journal_since(document, since_seq) else {
        return;
    };
    let Some(head) = since.head else {
        return;
    };
    let count = head.saturating_sub(since_seq.unwrap_or(0));
    if policy.due(count, since.bytes) {
        take_checkpoint(workspace, open, document, head, Timestamp::now(), "load");
    }
}

fn manifest_workspace_projection(manifest: &Manifest) -> WorkspaceManifestProjection {
    WorkspaceManifestProjection {
        name: manifest.name(),
        icon: manifest.icon_state(),
        color: manifest.color_state(),
        pinned: manifest.pinned(),
        archived: manifest.archived(),
        deleted_at: manifest.workspace_deletion_state(),
    }
}

impl<W> Open<W> {
    fn is_manifest(&self, document: DocumentId) -> bool {
        document == self.manifest_id
    }

    /// The encoded position of whichever document this is.
    fn position(&self, document: DocumentId) -> Option<(Vec<u8>, Vec<u8>)> {
        if self.is_manifest(document) {
            return Some((
                self.manifest.version().encode(),
                self.manifest.frontiers().encode(),
            ));
        }
        let live = self.documents.get(&document)?;
        Some((live.encoded_version(), live.encoded_frontiers()))
    }
}

/// The store.
pub struct Store<C: Catalog, B: Backend> {
    catalog: C,
    backend: B,
    open: HashMap<WorkspaceId, Open<B::Workspace>>,
    open_profiles: HashMap<ProfileId, IdentityManifest>,
    private_profiles: HashMap<ProfileId, PrivateManifest>,
    /// The Loro `PeerID` this store's durable replica uses, stable for the life
    /// of the install so a restart is not a new peer in every version vector.
    peer_id: u64,
    pub checkpoints: CheckpointPolicy,
    /// Recoveries seen since start-up, for the debug routes. Metadata only.
    recoveries: Vec<String>,
    /// Documents committed since somebody last drained this.
    ///
    /// **A map rather than a queue, keyed by document**, which is what keeps it
    /// bounded: a build with nothing draining it holds at most one entry per
    /// document rather than one per edit forever. It also coalesces at the
    /// source — twenty commits to one document are one entry naming the newest
    /// revision, which is exactly what a consumer wants, since it re-reads the
    /// state rather than replaying what it was told.
    committed: crate::commit_signal::Commits,
    /// Coalesced content-free profile reminder wakes waiting for the same
    /// post-durable consumer seam as document commits.
    reminder_committed: HashMap<(ProfileId, Option<ReminderId>, ReminderChange), ReminderCommitted>,
    /// Coalesced content-free personal-dictionary wakes. The spelling surface
    /// re-reads the private Profile unit after this post-commit trigger.
    dictionary_committed: HashMap<ProfileId, DictionaryCommitted>,
    /// Coalesced content-free wakes for an identity unit that arrived from
    /// somewhere else — a peer or the Cloud. A locally made rename announces
    /// itself from the route that made it; an imported one has no route, so
    /// this is where it becomes visible to the same consumer seam.
    identity_committed: HashMap<(ProfileId, IdentityChange), IdentityCommitted>,
    /// Documents whose disposable anti-entropy leaf may be stale. This is
    /// deliberately separate from `committed`: projection workers drain that
    /// queue, while replication may refresh its digest at any later time.
    sync_dirty: HashMap<WorkspaceId, BTreeSet<DocumentId>>,
    /// Workspaces whose persisted leaf cache has been checked against the
    /// authoritative repository during this process, plus whether the checked
    /// inventory was the deleted (manifest-only) view. The first access after
    /// a restart or lifecycle transition repairs membership; later accesses
    /// touch only dirty leaves.
    sync_verified: HashMap<WorkspaceId, bool>,
    /// The in-memory half of the device-local transport mode (A1 §5).
    ///
    /// **Deliberately not persisted.** It closes admission ahead of a durable
    /// write and stays closed when that write fails, which is only meaningful
    /// within one process: a restart must reload the last actually committed
    /// mode rather than a promise nobody kept.
    transport: crate::transport::Fence,
}

/// A document whose bytes have reached the disk.
///
/// **The signal derived state is built from, and it is deliberately opaque.**
/// Ids and an encoded version vector — nothing about what is *inside* a
/// document, because core does not know and must not learn
/// ([ADR-80](../../../docs/decisions/ADR-076-100.md#adr-80-manifest--a-workspace-is-a-manifest-document-plus-opaque-documents)).
/// Whoever consumes this reads the authoritative state for itself; **this is a
/// trigger, never truth**, so a consumer that misses one is stale rather than
/// wrong and repairs by comparing revisions.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Committed {
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    /// The journal position now durable.
    pub seq: u64,
    /// `VersionVector::encode()` as committed. Compared with `includes_vv`
    /// after decoding, never by equality of the encoding.
    pub version_vector: Vec<u8>,
    /// **The manifest is a document to the store and is not a Koru document.**
    /// A consumer projecting content skips it; one that joins against the
    /// listing — a title, a parent, a trash flag — needs to know it moved.
    pub is_manifest: bool,
}

/// A durable profile-root change. It is a wake, never reminder truth.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ReminderCommitted {
    pub profile: ProfileId,
    pub reminder: Option<ReminderId>,
    pub change: ReminderChange,
}

/// A durable personal-dictionary change. It carries no word or language.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DictionaryCommitted {
    pub profile: ProfileId,
}

/// Which of a Profile's two identity facts an import moved.
///
/// Two variants rather than one wake because the two are already two published
/// events (`profile.renamed`, `profile.appearance_changed`) and a consumer that
/// is told "renamed" when only the colour moved has been lied to. The diff is
/// made here because here is the only place that holds both the committed
/// snapshot and the one it replaced.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum IdentityChange {
    Name,
    Appearance,
}

/// A durable Profile identity change that arrived from a peer or the Cloud.
///
/// A wake, never truth: it names the profile and which fact moved, and the
/// consumer re-reads the profile. The values themselves never travel, for the
/// same reason [`Committed`] carries no content.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IdentityCommitted {
    pub profile: ProfileId,
    pub change: IdentityChange,
}

/// A content-first document creation, safe to expose after this returns.
pub struct SeededCreation {
    pub document: DocumentId,
    /// The original opaque bytes are replayed even if the live document has
    /// subsequently changed, allowing an adapter to reproduce its first
    /// success response exactly.
    pub initial_update: Vec<u8>,
    pub replayed: bool,
    /// Present only when this call published the manifest entry.
    pub manifest_delta: Option<Vec<u8>>,
}

/// The answer to [`Store::ensure_file_note`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EnsuredFileNote {
    /// The note's id — minted on this call or the one already attached.
    pub id: DocumentId,
    /// Whether this call created the note. A fact the store knows, handed up
    /// so no caller infers it from the delta.
    pub created: bool,
    /// The manifest delta — empty on the idempotent path, which changed
    /// nothing, so a broadcaster should skip it rather than push a no-op into
    /// the room.
    pub delta: Vec<u8>,
}

/// When a document last changed, and where that change came from.
///
/// **Provenance, never an author.** An origin names a replica path — this
/// store, a client of it, a peer device, Koru Cloud — and a store client or
/// peer identifies a replica rather than a person, which is the same rule
/// [`crate::ports::HistoryReason`] states for a version.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DocumentProvenance {
    pub updated_at: Timestamp,
    /// `None` when the journal holds nothing above the newest checkpoint.
    pub origin: Option<crate::ports::UpdateOrigin>,
}

/// Where one document has got to. Numbers, versions and instants only.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DocumentReport {
    pub kind: DocumentKind,
    pub version_vector: Vec<u8>,
    pub frontiers: Vec<u8>,
    pub latest_checkpoint: Option<u64>,
    pub checkpoints: usize,
    pub pending_updates: u64,
    /// The manifest's declared CRDT schema, when this document is one. The
    /// version a too-new document was written by is the whole reason it is
    /// read-only, so it belongs in a diagnostic report.
    pub schema_version: Option<i64>,
    /// When the document is in the workspace's trash — hidden from the
    /// listing on one device and shown with an In Trash strip on another.
    pub trashed_at: Option<Timestamp>,
    pub updated_at: Timestamp,
}

/// The key under which the store's peer id is kept.
pub const K_PEER_ID: &str = "peer_id";
/// A stable identifier for this installation. Not an account, not a machine.
pub const K_INSTALL_ID: &str = "install_id";

/// Prefix of the per-Profile **origin marker**.
///
/// > Exactly one device ever creates a given manifest. Every other device
/// > receives it.
///
/// Loro converges one document's history; it cannot tell you that two devices
/// created the same document. Two independently created identity manifests for
/// one Profile do not fail to merge — last-writer-wins simply picks one
/// `created_at`, a field the contract calls immutable. Document *identity* is
/// therefore established by **who created it**, which is outside the CRDT and
/// cannot be delegated to it. This key is where that fact is kept.
///
/// It lives in `store_state` rather than on the catalog's `profiles` row for
/// two reasons. A column is a migration, and this is not a schema decision to
/// take in passing. More importantly the marker **must never replicate**: a
/// Profile row travels, and a marker that travelled with it would tell every
/// receiving device that it was the origin — the exact confusion it exists to
/// prevent. `store_state` is local to one installation by construction.
const K_PROFILE_ORIGIN_PREFIX: &str = "profile_origin:";

/// Set once, the first time a build that understands origin markers opens a
/// store. Its absence means "this installation's markers have not begun yet",
/// which is what makes the one-time grandfathering below happen exactly once.
const K_PROFILE_ORIGIN_EPOCH: &str = "profile_origin_epoch";

/// This installation created the Profile. Written by `create_profile*`, the
/// only path on which a person actually originates one here.
const ORIGIN_CREATED_HERE: &str = "created-here";

/// Legacy bytes that were already on this disk before any marker existed.
///
/// Provenance genuinely unknown: a legacy `profile_crdts` row can be a Profile
/// this installation authored under an older build, or a peer's document that
/// `apply_profile_unit_updates(LegacyPrivate { .. })` wrote into the same
/// table. Recorded as its own value rather than as [`ORIGIN_CREATED_HERE`] so a
/// reader can never mistake an assumption for a fact.
const ORIGIN_LEGACY_GRANDFATHERED: &str = "legacy-grandfathered";

fn profile_origin_key(id: ProfileId) -> String {
    format!("{K_PROFILE_ORIGIN_PREFIX}{}", id.to_storage())
}

/// Whether this installation may author a fresh identity lineage for `id`.
///
/// A free function rather than a method because [`Store::open`] has to ask it
/// before a `Store` exists.
fn may_mint_identity<C: Catalog>(catalog: &C, id: ProfileId) -> Result<bool> {
    Ok(catalog.state(&profile_origin_key(id))?.is_some())
}

/// The application-owned kind tokens [`Store::ensure_file_note`] has to know.
///
/// Kind tokens belong to the application (`koru_core::explore::KIND_FILE` /
/// `KIND_NOTE`); these two are duplicated here because core cannot name that
/// crate — the same reason a manifest test writes `"page"` by hand. A kind the
/// store *interprets* is the exception this operation is: a note may attach
/// only to a file.
const KIND_FILE: &str = "file";
const KIND_NOTE: &str = "note";

impl<C: Catalog, B: Backend> Store<C, B> {
    /// Open the store. Touches `catalog.db` and nothing else.
    pub fn open(mut catalog: C, backend: B) -> Result<Self> {
        let peer_id = match catalog.state(K_PEER_ID)? {
            Some(v) => v.parse().unwrap_or_else(|_| mint_peer_id()),
            None => {
                let minted = mint_peer_id();
                catalog.set_state(K_PEER_ID, &minted.to_string())?;
                minted
            }
        };
        if catalog.state(K_INSTALL_ID)?.is_none() {
            catalog.set_state(K_INSTALL_ID, &uuid::Uuid::new_v4().to_string())?;
        }
        let profiles = catalog.profiles()?;
        // The one-time grandfather, and the only place an origin marker is
        // ever written without knowing it is true.
        //
        // An installation upgrading from a build that kept no marker has
        // legacy rows whose provenance nothing on this disk records. Refusing
        // those would strand a device that genuinely did originate its Profile
        // offline and has no route to receive the original — and losing
        // content is the one unrecoverable bug. So every legacy row present at
        // the moment markers begin is grandfathered, once, under a value that
        // says it was assumed rather than observed. The loop below upgrades
        // them in this same open, so the window is one pass wide: nothing
        // arriving afterwards is ever grandfathered.
        if catalog.state(K_PROFILE_ORIGIN_EPOCH)?.is_none() {
            // Locally removed rows are included deliberately. They are
            // invisible to `profiles()` but still restorable, and
            // `restore_profile_local` is itself a legacy-upgrade path — a
            // Profile this installation originated offline and then hid would
            // otherwise become unrestorable the moment markers began.
            for profile in catalog.profiles_including_removed()? {
                if catalog.profile_crdt(profile.id)?.is_some()
                    && !may_mint_identity(&catalog, profile.id)?
                {
                    let key = profile_origin_key(profile.id);
                    catalog.set_state(&key, ORIGIN_LEGACY_GRANDFATHERED)?;
                }
            }
            catalog.set_state(K_PROFILE_ORIGIN_EPOCH, "1")?;
        }
        // A legacy snapshot does **not** prove this is a local upgrade, which
        // is what the comment here used to claim. `profile_crdts` is also
        // written by `apply_profile_unit_updates(LegacyPrivate { .. })`, so a
        // peer's document lands in the very table that was being read as proof
        // of local origin — and a device that had only been *told* about a
        // Profile then minted a second lineage for it, stamped from its own
        // clock, which can never reconcile with the first. See PLAN19 round
        // five. Only the origin marker proves it; without one the device waits
        // to receive an identity unit, which is what receipt means.
        for profile in &profiles {
            if catalog
                .profile_unit(ProfileUnitRef::Identity {
                    profile: profile.id,
                })?
                .is_none()
                && let Some(legacy) = catalog.profile_crdt(profile.id)?
            {
                if !may_mint_identity(&catalog, profile.id)? {
                    // Skipped, not fatal. This is the ordinary state of a
                    // Profile whose identity bytes have not arrived yet, and
                    // it must not stop the store from opening.
                    tracing::warn!(
                        profile = %profile.id,
                        "legacy Profile bytes with no local origin marker; waiting to receive an identity unit rather than minting one",
                    );
                    continue;
                }
                let materialized = ProfileManifest::from_snapshot(&legacy.snapshot)?.profile()?;
                if materialized.id != profile.id {
                    return Err(Error::bare(Code::ProfileUnitScopeMismatch));
                }
                let identity = IdentityManifest::create(&materialized)?;
                let private = PrivateManifest::create(profile.id)?;
                identity.set_peer_id(peer_id)?;
                private.set_peer_id(peer_id)?;
                catalog.initialize_profile_units(
                    &materialized,
                    &identity.export_snapshot()?,
                    &private.export_snapshot()?,
                )?;
            }
        }
        // Version equality cannot repair a stale catalog projection: no new
        // identity frame arrives when both devices already have the same CRDT.
        // Reconcile retained identity timestamps once on open, before peers
        // can compare workspace descriptors against provisioning placeholders.
        profile_projection::reconcile_creation_times(&mut catalog)?;
        // The ordinary post-commit wake is deliberately in memory. Seed one
        // reconciliation wake per retained private unit on Store restart so a
        // death after commit but before the projection drain cannot leave a
        // live spelling overlay stale. Consumers still re-read authority; an
        // empty dictionary must wake too because the missed change may have
        // removed its final word.
        let mut dictionary_committed = HashMap::new();
        for profile in profiles {
            if catalog
                .profile_unit(ProfileUnitRef::Private {
                    profile: profile.id,
                })?
                .is_some()
            {
                dictionary_committed.insert(
                    profile.id,
                    DictionaryCommitted {
                        profile: profile.id,
                    },
                );
            }
        }
        Ok(Self {
            catalog,
            backend,
            open: HashMap::new(),
            open_profiles: HashMap::new(),
            private_profiles: HashMap::new(),
            peer_id,
            checkpoints: CheckpointPolicy::default(),
            recoveries: Vec::new(),
            committed: crate::commit_signal::Commits::default(),
            reminder_committed: HashMap::new(),
            dictionary_committed,
            // Deliberately not seeded on restart, unlike the dictionary above:
            // a wake here becomes `profile.renamed` on a published bus, and a
            // rename nobody performed announced on every start-up is a worse
            // failure than the one restart-seeding would close. A surface reads
            // every profile when it opens anyway.
            identity_committed: HashMap::new(),
            sync_dirty: HashMap::new(),
            sync_verified: HashMap::new(),
            transport: crate::transport::Fence::new(),
        })
    }

    pub fn record_sync_state_event(
        &mut self,
        event: &SyncStateEvent,
        retain_after: Timestamp,
    ) -> Result<SyncStateEvent> {
        self.catalog.record_sync_state_event(event, retain_after)
    }

    pub fn sync_state_events(
        &self,
        peer_device_id: &str,
        retain_after: Timestamp,
        before_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<SyncStateEvent>> {
        self.catalog
            .sync_state_events(peer_device_id, retain_after, before_id, limit)
    }

    pub fn latest_sync_state_event_times(
        &self,
        retain_after: Timestamp,
    ) -> Result<Vec<(String, Timestamp)>> {
        self.catalog.latest_sync_state_event_times(retain_after)
    }

    pub fn prune_sync_state_events(&mut self, retain_after: Timestamp) -> Result<usize> {
        self.catalog.prune_sync_state_events(retain_after)
    }

    pub fn peer_id(&self) -> u64 {
        self.peer_id
    }

    pub fn recoveries(&self) -> &[String] {
        &self.recoveries
    }

    /// How many workspaces are currently held open. A number for the debug
    /// routes, and the thing that proves laziness is working.
    pub fn open_workspaces(&self) -> usize {
        self.open.len()
    }

    pub fn open_documents(&self) -> usize {
        self.open.values().map(|w| w.documents.len()).sum()
    }

    // ------------------------------------------------------------- profiles

    /// Create a persona.
    ///
    /// `purpose` and `avatar` are carried, never read: the store has no opinion
    /// about what "work" means or what a `color:purple` avatar looks like. They
    /// are arguments rather than a later `update_profile` because onboarding
    /// knows all four answers at once, and a create-then-patch would leave a
    /// profile briefly on disk in a state no screen ever asked for.
    pub fn create_profile(
        &mut self,
        name: &str,
        purpose: Option<&str>,
        avatar: Option<&str>,
        locale: Locale,
        at: Timestamp,
    ) -> Result<Profile> {
        self.create_profile_with_appearance(name, purpose, avatar, None, None, locale, at)
    }

    /// Create a persona with its complete initial appearance atomically.
    pub fn create_profile_with_appearance(
        &mut self,
        name: &str,
        purpose: Option<&str>,
        avatar: Option<&str>,
        icon: Option<&str>,
        color: Option<&str>,
        locale: Locale,
        at: Timestamp,
    ) -> Result<Profile> {
        let profile = Profile::new(name, locale, at)?
            .with_purpose(purpose)
            .with_avatar(avatar)
            .with_appearance(icon, color);
        let manifest = IdentityManifest::create(&profile)?;
        let private = PrivateManifest::create(profile.id)?;
        manifest.set_peer_id(self.peer_id)?;
        private.set_peer_id(self.peer_id)?;
        // Record the origin before authoring the bytes. This is the one path
        // on which a person actually creates a Profile here, and therefore the
        // only place the marker is written from knowledge rather than
        // assumption. `provision_account_profile` deliberately does not write
        // it: being told a Profile exists is receipt, not origin.
        self.catalog
            .set_state(&profile_origin_key(profile.id), ORIGIN_CREATED_HERE)?;
        self.catalog.initialize_profile_units(
            &profile,
            &manifest.export_snapshot()?,
            &private.export_snapshot()?,
        )?;
        self.open_profiles.insert(profile.id, manifest);
        self.private_profiles.insert(profile.id, private);
        Ok(profile)
    }

    pub fn profiles(&self) -> Result<Vec<Profile>> {
        self.catalog.profiles()
    }

    /// Materialize an explicitly enrolled Profile before its Workspaces arrive.
    /// The trusted account adapter supplies the authorized id and binding;
    /// scoped peers still cannot import descriptive Profile metadata or CRDTs.
    /// Retries preserve local names and content, and never restore removal marks
    /// or attach an existing offline Profile without its separate consent flow.
    pub fn provision_account_profile(
        &mut self,
        id: ProfileId,
        binding: ProfileAccountBinding,
        name: &str,
        locale: Locale,
        at: Timestamp,
    ) -> Result<Profile> {
        match self.catalog.profile_catalog_state(id)? {
            Some(ProfileCatalogState::Removed) => return Err(Error::bare(Code::ProfileRemoved)),
            Some(ProfileCatalogState::Active) => {
                let profile = self.profile(id)?;
                if profile.account.as_deref() != Some(binding.to_storage().as_str()) {
                    return Err(Error::bare(Code::ProfileAccountConflict));
                }
                return Ok(profile);
            }
            None => {}
        }
        let mut profile = Profile::new(name, locale, at)?;
        profile.id = id;
        profile.account = Some(binding.to_storage());
        // This is a presence placeholder, not authority to author CRDT bytes.
        // Received identity/private units complete it under their own lineage.
        // **No origin marker is written here**, and that omission is the whole
        // point: this device was told the Profile exists, so it is a receiver,
        // and `profile.created_at` above is this device's clock — a fact about
        // this device, never the Profile's creation time.
        self.catalog.put_profile(&profile)?;
        Ok(profile)
    }

    /// Minimal local presence used by the Agent's exact-account lifecycle
    /// reconciliation. This deliberately includes locally removed rows.
    pub fn profile_lifecycle_inventory(
        &self,
    ) -> Result<Vec<crate::ports::ProfileLifecycleInventoryItem>> {
        self.catalog.profile_lifecycle_inventory()
    }

    /// Remove an empty Profile from this installation's visible catalog.
    ///
    /// This never deletes the Profile row, its CRDT, Workspace tombstones, or
    /// Workspace databases. The catalog backend owns the atomic active-Workspace
    /// check so a successful result cannot race a new local Workspace.
    pub fn remove_profile_local(
        &mut self,
        id: ProfileId,
        at: Timestamp,
    ) -> Result<crate::ports::LocalProfileRemoval> {
        self.refuse_holder(id, "removed")?;
        let result = self.catalog.remove_profile_local(id, at)?;
        if result == crate::ports::LocalProfileRemoval::Removed {
            self.open_profiles.remove(&id);
            self.private_profiles.remove(&id);
        }
        Ok(result)
    }

    /// Restore a locally removed Profile after validating its retained CRDT.
    ///
    /// Validation happens before the catalog marker changes, so corrupt
    /// recovery data cannot turn an invisible Profile into one startup then
    /// fails to open. Workspace tombstones are deliberately untouched.
    pub fn restore_profile_local(
        &mut self,
        id: ProfileId,
    ) -> Result<crate::ports::LocalProfileRestoration> {
        match self.catalog.profile_catalog_state(id)? {
            None => return Err(Error::bare(Code::ProfileNotFound)),
            Some(ProfileCatalogState::Active) => {
                self.ensure_profile_manifest(id)?;
                return Ok(crate::ports::LocalProfileRestoration::AlreadyActive);
            }
            Some(ProfileCatalogState::Removed) => {}
        }
        let projected = self
            .catalog
            .profile_including_removed(id)?
            .ok_or_else(|| Error::bare(Code::ProfileNotFound))?;
        if projected.is_holder() {
            return Err(Error::new(
                Code::ProfileReserved,
                "the No-profile holder cannot be restored",
            ));
        }
        let manifest = match self
            .catalog
            .profile_unit(ProfileUnitRef::Identity { profile: id })?
        {
            Some(state) => IdentityManifest::from_snapshot(&state.snapshot)?,
            None => {
                let state = self
                    .catalog
                    .profile_crdt(id)?
                    .ok_or_else(|| Error::bare(Code::VersionUnavailable))?;
                // The same guard `open` applies, for the same reason: legacy
                // bytes on this disk may be a peer's, and minting from them is
                // how a second lineage is born. Restoring a Profile this
                // installation did not originate waits for its identity unit.
                if !may_mint_identity(&self.catalog, id)? {
                    return Err(Error::new(
                        Code::VersionUnavailable,
                        "this installation did not originate this Profile; its identity bytes have not arrived",
                    ));
                }
                let profile = ProfileManifest::from_snapshot(&state.snapshot)?.profile()?;
                if profile.id != id {
                    return Err(Error::bare(Code::ProfileUnitScopeMismatch));
                }
                let identity = IdentityManifest::create(&profile)?;
                let private = PrivateManifest::create(id)?;
                self.catalog.initialize_profile_units(
                    &profile,
                    &identity.export_snapshot()?,
                    &private.export_snapshot()?,
                )?;
                identity
            }
        };
        let authoritative = manifest.profile()?;
        if authoritative.id != id {
            return Err(Error::new(
                Code::CorruptState,
                "retained Profile identity does not match its catalog row",
            ));
        }
        manifest.set_peer_id(self.peer_id)?;
        let outcome = self.catalog.restore_profile_local(id)?;
        self.open_profiles.insert(id, manifest);
        Ok(outcome)
    }

    /// The one gate every persona-only operation asks.
    ///
    /// Renaming, recolouring and removal each refuse on the holder, and they
    /// refuse *here* rather than at their call sites so a fourth persona
    /// operation added later cannot forget to.
    fn refuse_holder(&self, id: ProfileId, verb: &str) -> Result<()> {
        if self.catalog.profile(id)?.is_some_and(|p| p.is_holder()) {
            return Err(Error::new(
                Code::ProfileReserved,
                format!("the No-profile holder cannot be {verb}"),
            ));
        }
        Ok(())
    }

    pub fn profile(&self, id: ProfileId) -> Result<Profile> {
        self.catalog
            .profile(id)?
            .ok_or_else(|| Error::bare(Code::ProfileNotFound))
    }

    /// Atomically compare and set the hosted Koru Identity bound to this Profile.
    ///
    /// The Store lock serializes the comparison with every other Profile manifest
    /// mutation, while `commit_profile_identity` persists the manifest and catalog
    /// projection in one transaction before this method acknowledges `Bound`.
    /// `AlreadyBound` makes exact retries idempotent. No Profile, Workspace, or
    /// content id is ever minted or changed here.
    pub fn bind_profile_account(
        &mut self,
        id: ProfileId,
        expected: Option<ProfileAccountBinding>,
        new: ProfileAccountBinding,
    ) -> Result<ProfileAccountBindingOutcome> {
        match self.catalog.profile_catalog_state(id)? {
            None => return Err(Error::bare(Code::ProfileNotFound)),
            Some(ProfileCatalogState::Removed) => return Err(Error::bare(Code::ProfileRemoved)),
            Some(ProfileCatalogState::Active) => {}
        }
        self.ensure_profile_manifest(id)?;
        let before = self
            .open_profiles
            .get(&id)
            .ok_or_else(|| Error::bare(Code::ProfileNotFound))?
            .export_snapshot()?;
        let current = self
            .open_profiles
            .get(&id)
            .ok_or_else(|| Error::bare(Code::ProfileNotFound))?
            .profile()?
            .account
            .as_deref()
            .map(ProfileAccountBinding::parse_storage)
            .transpose()?;
        if current == Some(new) {
            return Ok(ProfileAccountBindingOutcome::AlreadyBound);
        }
        if current != expected {
            return Err(Error::bare(Code::ProfileAccountConflict));
        }

        let manifest = self
            .open_profiles
            .get(&id)
            .ok_or_else(|| Error::bare(Code::ProfileNotFound))?;
        manifest.set_account(&new.to_storage())?;
        let profile = manifest.profile()?;
        let snapshot = manifest.export_snapshot()?;
        if let Err(error) = self.catalog.commit_profile_identity(&profile, &snapshot) {
            // A failed durable commit must not leave a binding visible only in
            // this process. The pre-write snapshot is known-good and restores
            // the same peer before the error crosses the boundary.
            let restored = IdentityManifest::from_snapshot(&before)?;
            restored.set_peer_id(self.peer_id)?;
            self.open_profiles.insert(id, restored);
            return Err(error);
        }
        Ok(ProfileAccountBindingOutcome::Bound)
    }

    /// Atomically clear one exact hosted binding while retaining the local
    /// Profile. A compare-and-set prevents a delayed deletion decision from
    /// detaching a Profile that has since been connected to another identity.
    pub fn clear_profile_account(
        &mut self,
        id: ProfileId,
        expected: ProfileAccountBinding,
    ) -> Result<crate::ProfileAccountClearOutcome> {
        match self.catalog.profile_catalog_state(id)? {
            None => return Err(Error::bare(Code::ProfileNotFound)),
            Some(ProfileCatalogState::Removed) => return Err(Error::bare(Code::ProfileRemoved)),
            Some(ProfileCatalogState::Active) => {}
        }
        self.ensure_profile_manifest(id)?;
        let before = self
            .open_profiles
            .get(&id)
            .ok_or_else(|| Error::bare(Code::ProfileNotFound))?
            .export_snapshot()?;
        let current = self
            .open_profiles
            .get(&id)
            .ok_or_else(|| Error::bare(Code::ProfileNotFound))?
            .profile()?
            .account
            .as_deref()
            .map(ProfileAccountBinding::parse_storage)
            .transpose()?;
        if current.is_none() {
            return Ok(crate::ProfileAccountClearOutcome::AlreadyOffline);
        }
        if current != Some(expected) {
            return Err(Error::bare(Code::ProfileAccountConflict));
        }

        let manifest = self
            .open_profiles
            .get(&id)
            .ok_or_else(|| Error::bare(Code::ProfileNotFound))?;
        manifest.clear_account()?;
        let profile = manifest.profile()?;
        let snapshot = manifest.export_snapshot()?;
        if let Err(error) = self.catalog.commit_profile_identity(&profile, &snapshot) {
            let restored = IdentityManifest::from_snapshot(&before)?;
            restored.set_peer_id(self.peer_id)?;
            self.open_profiles.insert(id, restored);
            return Err(error);
        }
        Ok(crate::ProfileAccountClearOutcome::Cleared)
    }

    /// Rename a profile. The CRDT is the replicated authority — the write goes
    /// there first — and the catalog row is refreshed from it in the same
    /// durable commit, the exact shape of `apply_profile_updates` so a local
    /// rename and a peer's rename cannot diverge in how they land.
    pub fn rename_profile(&mut self, id: ProfileId, name: &str) -> Result<Profile> {
        self.refuse_holder(id, "renamed")?;
        self.ensure_profile_manifest(id)?;
        // Validation first, against the same character-counted limits as
        // creation, before anything touches the CRDT.
        let mut validated = self.profile(id)?;
        validated.rename(name)?;
        let manifest =
            IdentityManifest::from_snapshot(&self.open_profiles[&id].export_snapshot()?)?;
        manifest.set_peer_id(self.peer_id)?;
        manifest.set_name(&validated.name)?;
        let profile = manifest.profile()?;
        self.catalog
            .commit_profile_identity(&profile, &manifest.export_snapshot()?)?;
        self.open_profiles.insert(id, manifest);
        Ok(profile)
    }

    /// Set the two switcher appearance fields together in the replicated
    /// profile manifest and its catalog projection.
    pub fn set_profile_appearance(
        &mut self,
        id: ProfileId,
        icon: Option<&str>,
        color: Option<&str>,
    ) -> Result<Profile> {
        self.refuse_holder(id, "recoloured")?;
        self.ensure_profile_manifest(id)?;
        let manifest =
            IdentityManifest::from_snapshot(&self.open_profiles[&id].export_snapshot()?)?;
        manifest.set_peer_id(self.peer_id)?;
        manifest.set_appearance(icon, color)?;
        let profile = manifest.profile()?;
        self.catalog
            .commit_profile_identity(&profile, &manifest.export_snapshot()?)?;
        self.open_profiles.insert(id, manifest);
        Ok(profile)
    }

    /// Create only the listing row for a remotely advertised profile. No CRDT
    /// lineage exists until exact received unit bytes materialize it.
    pub fn provision_replica_profile(&mut self, profile: &Profile) -> Result<()> {
        match self.catalog.profile_catalog_state(profile.id)? {
            Some(ProfileCatalogState::Removed) => return Err(Error::bare(Code::ProfileRemoved)),
            Some(ProfileCatalogState::Active) => {}
            None => self.catalog.put_profile(profile)?,
        }
        Ok(())
    }

    pub fn profile_version(&mut self, id: ProfileId) -> Result<loro::VersionVector> {
        self.ensure_profile_manifest(id)?;
        Ok(self.open_profiles[&id].version())
    }

    pub fn profile_updates_since(
        &mut self,
        id: ProfileId,
        from: &loro::VersionVector,
    ) -> Result<Vec<u8>> {
        self.ensure_profile_manifest(id)?;
        self.open_profiles[&id].export_updates_since(from)
    }

    // ----------------------------------------------------------- credentials

    /// Resolve a credential by the **hash** of its token.
    ///
    /// The token itself never reaches the store — the caller hashes it — so a
    /// leaked `catalog.db` is not a set of working keys, and no code path here
    /// ever holds one in a form it could log.
    pub fn client_by_token(&self, token_hash: &[u8]) -> Result<Option<crate::model::ApiClient>> {
        Ok(self
            .catalog
            .client_by_token(token_hash)?
            .filter(|c| c.active()))
    }

    pub fn register_client(
        &mut self,
        client: &crate::model::ApiClient,
        token_hash: &[u8],
    ) -> Result<()> {
        self.catalog.put_client(client, token_hash)
    }

    pub fn revoke_client(&mut self, id: ClientId, at: Timestamp) -> Result<()> {
        self.catalog.revoke_client(id, at)
    }

    pub fn clients(&self) -> Result<Vec<crate::model::ApiClient>> {
        self.catalog.clients()
    }

    // ----------------------------------------------------------- workspaces

    /// Create a workspace, its storage, and its manifest — one operation,
    /// because a workspace without a manifest is not a workspace.
    pub fn create_workspace(
        &mut self,
        owner: ProfileId,
        name: &str,
        at: Timestamp,
    ) -> Result<Workspace> {
        // Refuse early rather than leaving a database behind for an owner that
        // does not exist.
        if self.catalog.profile(owner)?.is_none() {
            return Err(Error::bare(Code::ProfileNotFound));
        }
        self.ensure_profile_manifest(owner)?;
        let mut workspace = Workspace::new(owner, name, at)?;
        workspace.position = self.next_workspace_position(owner)?;
        self.catalog.put_workspace(&workspace)?;

        let mut repo = self.backend.open_workspace(workspace.id)?;
        let manifest_id = DocumentId::new();
        repo.create_document(manifest_id, DocumentKind::Manifest, at)?;

        let manifest = Manifest::create(workspace.id, name, at)?;
        manifest.set_peer_id(self.peer_id)?;
        // The manifest's own creation has to reach the journal, or a reopen
        // finds a registered document with no history — and a later delta from
        // a client would name ancestors nobody has.
        let mut open = Open {
            repo,
            manifest,
            documents: HashMap::new(),
            windows: HashMap::new(),
            manifest_id,
        };
        let whole = open.manifest.export_all_updates()?;
        commit_one(
            &mut open,
            workspace.id,
            &mut self.committed,
            &mut self.sync_dirty,
            manifest_id,
            whole,
            at,
            crate::ports::UpdateOrigin::Store,
        )?;
        open.repo.reindex(&open.manifest.entries())?;

        self.catalog.record_location(manifest_id, workspace.id)?;
        self.open.insert(workspace.id, open);
        Ok(workspace)
    }

    /// Provision the identity rows for a workspace being replicated for the
    /// first time, preserving every logical id. This creates an empty manifest
    /// container only; the peer's manifest snapshot/update must still traverse
    /// [`apply_update`](Self::apply_update) and [`flush`](Self::flush).
    ///
    /// Narrow by design: transports call this after authorization. Ordinary
    /// workspace creation continues to mint its own ids.
    pub fn provision_replica(
        &mut self,
        profile: &Profile,
        workspace: &Workspace,
        manifest: DocumentId,
        at: Timestamp,
    ) -> Result<()> {
        match self.catalog.profile(profile.id)? {
            Some(existing) if existing.created_at != profile.created_at => {
                // Keep the identity guard intact. These exact millisecond
                // values distinguish a bootstrap/precision mismatch from a
                // different replica identity; names and content are omitted.
                tracing::warn!(
                    profile = %profile.id.as_uuid(),
                    workspace = %workspace.id.as_uuid(),
                    manifest = %manifest.as_uuid(),
                    local_profile_created_at_ms = existing.created_at.millis(),
                    incoming_profile_created_at_ms = profile.created_at.millis(),
                    creation_delta_ms = i128::from(profile.created_at.millis())
                        - i128::from(existing.created_at.millis()),
                    incoming_workspace_created_at_ms = workspace.created_at.millis(),
                    "store replica: profile creation timestamp mismatch"
                );
                return Err(Error::new(
                    Code::ProfileExists,
                    "the replica profile id has a different immutable creation time",
                ));
            }
            // Mutable profile fields come from the profile CRDT. A workspace
            // descriptor is bootstrap compatibility data and may legitimately
            // carry an older name or omit fields learned through a newer peer.
            Some(_) => {}
            None => self.catalog.put_profile(profile)?,
        }

        if let Some(existing) = self.catalog.workspace(workspace.id)? {
            if existing.owner != workspace.owner {
                return Err(Error::new(
                    Code::WorkspaceExists,
                    "the replica workspace id already belongs to another profile",
                ));
            }
            let repo = self.backend.open_workspace(workspace.id)?;
            let manifests: Vec<_> = repo
                .documents()?
                .into_iter()
                .filter(|document| document.kind == DocumentKind::Manifest)
                .collect();
            if manifests.len() != 1 || manifests[0].id != manifest {
                return Err(Error::new(
                    Code::WorkspaceExists,
                    "the replica workspace id already has another manifest identity",
                ));
            }
            // The manifest is the replicated authority for the name. The
            // descriptor supplies a useful name while a replica is first being
            // provisioned, but after that it is only bootstrap metadata: an
            // older peer inventory must not overwrite a rename already present
            // in this replica's manifest. `flush` projects the authoritative
            // manifest name back into this catalog row after a durable commit.
            // Every shared mutable field is now manifest-authoritative. The
            // descriptor is bootstrap only and must not overwrite local CRDT
            // state merely because an older peer sent another inventory.
            return Ok(());
        }

        if workspace.owner != profile.id {
            return Err(Error::new(
                Code::InvalidRequest,
                "the replica workspace owner does not match its profile",
            ));
        }
        if let Some(other) = self.catalog.locate(manifest)? {
            return Err(Error::new(
                Code::DocumentExists,
                format!("the replica manifest already belongs to workspace {other}"),
            ));
        }

        // Ordering and last-opened state are local. Shared appearance and
        // lifecycle arrive in the descriptor for bootstrap, then the manifest
        // CRDT becomes authoritative as soon as its updates are imported.
        let mut record = workspace.clone();
        record.last_opened_at = None;
        let deleted_at = record.deleted_at.take();
        record.position = self.next_workspace_position(profile.id)?;
        self.catalog.put_workspace(&record)?;
        let mut repo = self.backend.open_workspace(record.id)?;
        repo.create_document(manifest, DocumentKind::Manifest, at)?;
        self.catalog.record_location(manifest, record.id)?;
        if let Some(deleted_at) = deleted_at {
            self.catalog
                .set_workspace_deleted(record.id, Some(deleted_at), &[manifest])?;
        }
        Ok(())
    }

    /// The manifest's own deletion state: `Some(Some(at))` actively deleted,
    /// `Some(None)` restored, `None` never recorded. A diagnostic that only
    /// reads the catalog row cannot tell a stale row from shared truth; this
    /// is the truth the row projects.
    pub fn workspace_manifest_deletion(
        &mut self,
        workspace: WorkspaceId,
    ) -> Result<Option<Option<Timestamp>>> {
        self.ensure_open_including_deleted(workspace)?;
        let open = self.open.get(&workspace).expect("just opened");
        Ok(manifest_workspace_projection(&open.manifest).deleted_at)
    }

    /// Read the manifest identity without making a soft-deleted workspace
    /// active. Replication needs this solely to carry its lifecycle tombstone;
    /// ordinary Store callers must continue through [`manifest_id`](Self::manifest_id).
    pub fn replica_manifest_id_including_deleted(
        &self,
        workspace: WorkspaceId,
    ) -> Result<DocumentId> {
        self.backend
            .open_workspace(workspace)?
            .documents()?
            .into_iter()
            .find(|document| document.kind == DocumentKind::Manifest)
            .map(|document| document.id)
            .ok_or_else(|| Error::bare(Code::DocumentNotFound))
    }

    /// Provision one empty ordinary document during bounded bootstrap. Calling
    /// it again is idempotent only for the same workspace and document kind.
    pub fn provision_replica_document(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        at: Timestamp,
    ) -> Result<()> {
        if self.catalog.workspace(workspace)?.is_none() {
            return Err(Error::bare(Code::WorkspaceNotFound));
        }
        if let Some(owner) = self.catalog.locate(document)? {
            if owner == workspace {
                let repo = self.backend.open_workspace(workspace)?;
                match repo.document(document)? {
                    Some(state) => {
                        return if state.kind == DocumentKind::Document {
                            Ok(())
                        } else {
                            Err(Error::new(
                                Code::DocumentExists,
                                "the replica document id is already the manifest",
                            ))
                        };
                    }
                    None => {
                        // The catalog claims a document that is not here. The
                        // location table is a disposable projection, and a row
                        // that points at nothing must never win over it: drop
                        // it and provision as if it had never been. Honoring
                        // it instead failed every sync pass with
                        // `document_not_found` forever.
                        self.catalog.forget_location(document)?;
                    }
                }
            } else {
                // A cross-workspace move is two manifest commits and they can
                // arrive in either order. Opening the former owner repairs the
                // derived location index from its authoritative manifest. If
                // the source removal has arrived, its retained/orphan bytes no
                // longer claim ownership and provisioning may continue here.
                let _ = self.ensure_open_including_deleted(owner);
                if self.catalog.locate(document)? == Some(owner) {
                    // The open repair forgets rows the owner's manifest does
                    // not list, but it only walks physical documents — a
                    // catalog row whose document the owner does not physically
                    // hold is the same phantom as above, one hop away.
                    let owner_repo = self.backend.open_workspace(owner)?;
                    if owner_repo.document(document)?.is_some() {
                        return Err(Error::new(
                            Code::DocumentExists,
                            format!("the replica document already belongs to workspace {owner}"),
                        ));
                    }
                    self.catalog.forget_location(document)?;
                }
            }
        }
        let mut repo = self.backend.open_workspace(workspace)?;
        // The location table is a disposable projection. A crash after the
        // workspace row was committed, or a manifest/location repair racing
        // bounded bootstrap, can therefore leave the physical document here
        // while `catalog.locate` says nothing. Treat the matching ordinary
        // document as the idempotent retry it is and repair the projection;
        // asking the repository to create it again turns harmless recovery
        // into `document_exists` and exponential replication backoff.
        if let Some(state) = repo.document(document)? {
            if state.kind != DocumentKind::Document {
                return Err(Error::new(
                    Code::DocumentExists,
                    "the replica document id is already the manifest",
                ));
            }
            self.catalog.record_location(document, workspace)?;
            return Ok(());
        }
        repo.create_document(document, DocumentKind::Document, at)?;
        self.catalog.record_location(document, workspace)?;
        Ok(())
    }

    /// Rename a workspace.
    ///
    /// **Both names are written**, and they are two different facts that happen
    /// to agree: the catalog row is what `workspaces()` lists — the switcher, a
    /// picker, anything choosing between workspaces — and the manifest's name
    /// travels with the workspace's own document, so a replica that has the
    /// manifest and not the catalog still knows what it is called.
    ///
    /// Writing only the row was the first version of this and it is wrong in a
    /// way that hides: the window would show the new name from the catalog while
    /// the manifest kept the old one, and the two would part company for good.
    /// Set how a workspace is presented in the switcher.
    ///
    /// **The whole set at once, not one field at a time.** These four are edited
    /// together — one dialog, one row's menu — and a caller always knows all of
    /// them, so sending them whole avoids the *absent versus null* question that
    /// a partial update has to answer in JSON and that nobody answers the same
    /// way twice.
    ///
    /// These values live in the manifest CRDT as shared workspace metadata.
    /// The catalog row is only their local listing projection; ordering and
    /// last-opened state are the device-local exceptions.
    pub fn set_workspace_appearance(
        &mut self,
        id: WorkspaceId,
        icon: Option<&str>,
        color: Option<&str>,
        pinned: bool,
        archived: bool,
    ) -> Result<Workspace> {
        self.ensure_open(id)?;
        let mut workspace = self
            .catalog
            .workspace(id)?
            .ok_or_else(|| Error::bare(Code::WorkspaceNotFound))?;
        workspace.icon = icon.map(str::to_owned);
        workspace.color = color.map(str::to_owned);
        workspace.pinned = pinned;
        workspace.archived = archived;
        self.catalog.put_workspace(&workspace)?;
        if let Some(open) = self.open.get_mut(&id) {
            let before = open.manifest.version();
            open.manifest
                .set_appearance(icon, color, pinned, archived)?;
            let update = open.manifest.export_updates_since(&before)?;
            let manifest_id = open.manifest_id;
            commit_one(
                open,
                id,
                &mut self.committed,
                &mut self.sync_dirty,
                manifest_id,
                update,
                Timestamp::now(),
                crate::ports::UpdateOrigin::Store,
            )?;
        }
        Ok(workspace)
    }

    pub fn rename_workspace(
        &mut self,
        id: WorkspaceId,
        name: &str,
        at: Timestamp,
    ) -> Result<Workspace> {
        // Open the manifest before changing its catalog projection. Opening a
        // cold workspace reconciles the catalog from the durable manifest; if
        // the row were renamed first, that reconciliation would immediately
        // put the old name back and the later manifest commit would leave the
        // two disagreeing until another repair pass.
        self.ensure_open(id)?;
        let mut workspace = self
            .catalog
            .workspace(id)?
            .ok_or_else(|| Error::bare(Code::WorkspaceNotFound))?;
        workspace.rename(name)?;
        self.catalog.put_workspace(&workspace)?;

        // The manifest is a document like any other, so its rename has to reach
        // the journal — a name changed only in memory is lost on the next
        // reopen, which is precisely the bug this method exists to fix.
        if let Some(open) = self.open.get_mut(&id) {
            let before = open.manifest.version();
            open.manifest.set_name(name)?;
            let update = open.manifest.export_updates_since(&before)?;
            let manifest_id = open.manifest_id;
            let sequence = commit_one(
                open,
                id,
                &mut self.committed,
                &mut self.sync_dirty,
                manifest_id,
                update,
                at,
                crate::ports::UpdateOrigin::Store,
            )?;
            tracing::info!(
                workspace = %id,
                document = %manifest_id,
                sequence,
                "workspace_change direction=local action=rename"
            );
        }
        Ok(workspace)
    }

    pub fn workspaces(&self) -> Result<Vec<Workspace>> {
        self.catalog.workspaces()
    }

    pub fn workspaces_including_deleted(&self) -> Result<Vec<Workspace>> {
        self.catalog.workspaces_including_deleted()
    }

    /// Replace an owner's complete order. Requiring the complete set makes a
    /// stale client fail rather than silently dropping a concurrently-created row.
    pub fn reorder_workspaces(
        &mut self,
        owner: ProfileId,
        ids: &[WorkspaceId],
    ) -> Result<Vec<Workspace>> {
        let current: Vec<_> = self
            .catalog
            .workspaces_including_deleted()?
            .into_iter()
            .filter(|workspace| workspace.owner == owner)
            .collect();
        let expected: std::collections::HashSet<_> = current.iter().map(|w| w.id).collect();
        let supplied: std::collections::HashSet<_> = ids.iter().copied().collect();
        if ids.len() != current.len() || supplied != expected {
            return Err(Error::new(
                Code::InvalidRequest,
                "workspace order must contain every owned workspace exactly once",
            ));
        }
        self.catalog.reorder_workspaces(owner, ids)?;
        Ok(self
            .catalog
            .workspaces_including_deleted()?
            .into_iter()
            .filter(|workspace| workspace.owner == owner)
            .collect())
    }

    /// **Give a workspace to another profile.** Ids and content unchanged.
    ///
    /// *What ownership is here*, and therefore what this has to move: the
    /// `workspaces.owner` column — a foreign key into `profiles` — and, in the
    /// same row, `position`, which is the workspace's place in its **owner's**
    /// order. Nothing else in the catalog is keyed by owner: lineage, document
    /// locations and sync digests are keyed by the workspace, and a profile's
    /// personal attributes live in the workspace's own manifest CRDT under the
    /// profile that recorded them, so they travel with the content and are not
    /// this operation's to move. Both fields the move does change are written
    /// by one `put_workspace` upsert, which is one statement — so a move is
    /// atomic without a transaction of its own, and there is no window in which
    /// a workspace has a new owner and its old neighbour's position.
    ///
    /// **It arrives last in its new home.** The position is the destination's
    /// next, exactly as a freshly created workspace's is: no other placement is
    /// derivable, because the source's order says nothing about the target's.
    /// The source is left with a gap rather than renumbered — order is read as
    /// `ORDER BY position`, never as a dense index, and a second write that
    /// changed nothing a person can see would be a second write that can fail.
    ///
    /// **No manifest write, and so no constraint on open documents.** The owner
    /// is not replicated workspace metadata — the manifest carries the name,
    /// the appearance and `deleted_at`, and nothing carries the owner — so this
    /// touches no document, no room and no connection. A soft delete refuses
    /// while documents are open ([`Code::WorkspaceInUse`]) because it
    /// disconnects them; a move has nothing to disconnect, and refusing under
    /// open documents would be a rule invented for the symmetry rather than for
    /// a reason.
    ///
    /// **Moving a workspace to the profile that already owns it succeeds and
    /// changes nothing** — not even the position. That is what makes the same
    /// request twice a retry rather than a second move to the end of the list.
    pub fn move_workspace(&mut self, id: WorkspaceId, to: ProfileId) -> Result<Workspace> {
        // The target first: refusing before the row is touched keeps a move to
        // a profile that does not exist from being distinguishable, by timing
        // or by side effect, from one that does.
        if self.catalog.profile(to)?.is_none() {
            return Err(Error::bare(Code::ProfileNotFound));
        }
        let mut workspace = self.workspace_including_deleted(id)?;
        // **The trash is answered honestly and by its own words.** A workspace
        // in the trash is not missing — the caller can see it in the
        // include-deleted listing — so the message says which of the two it is,
        // while the code stays the one every other read of a deleted workspace
        // gives, because a caller branching on `workspace_not_found` must not
        // have to learn a second code to keep working.
        if workspace.deleted_at.is_some() {
            return Err(Error::new(
                Code::WorkspaceNotFound,
                "that workspace is in the trash; restore it before moving it",
            ));
        }
        if workspace.owner == to {
            return Ok(workspace);
        }
        workspace.owner = to;
        workspace.position = self.next_workspace_position(to)?;
        self.catalog.put_workspace(&workspace)?;
        Ok(workspace)
    }

    fn next_workspace_position(&self, owner: ProfileId) -> Result<i64> {
        Ok(self
            .catalog
            .workspaces_including_deleted()?
            .into_iter()
            .filter(|workspace| workspace.owner == owner)
            .map(|workspace| workspace.position)
            .max()
            .unwrap_or(-1)
            .saturating_add(1))
    }

    pub fn delete_workspace(&mut self, id: WorkspaceId, at: Timestamp) -> Result<Workspace> {
        let mut workspace = self.workspace_including_deleted(id)?;
        if workspace.deleted_at.is_some() {
            return Ok(workspace);
        }
        self.ensure_open_including_deleted(id)?;
        if let Some(open) = self.open.get_mut(&id) {
            let before = open.manifest.version();
            open.manifest.set_workspace_deleted_at(Some(at))?;
            let update = open.manifest.export_updates_since(&before)?;
            let manifest_id = open.manifest_id;
            commit_one(
                open,
                id,
                &mut self.committed,
                &mut self.sync_dirty,
                manifest_id,
                update,
                at,
                crate::ports::UpdateOrigin::Store,
            )?;
        }
        let documents: Vec<_> = match self.open.get(&id) {
            Some(open) => open.repo.documents()?,
            None => self.backend.open_workspace(id)?.documents()?,
        }
        .into_iter()
        .map(|document| document.id)
        .collect();
        self.catalog
            .set_workspace_deleted(id, Some(at), &documents)?;
        workspace.deleted_at = Some(at);
        self.open.remove(&id);
        Ok(workspace)
    }

    pub fn restore_workspace(&mut self, id: WorkspaceId) -> Result<Workspace> {
        let mut workspace = self.workspace_including_deleted(id)?;
        if workspace.deleted_at.is_none() {
            return Ok(workspace);
        }
        // A locally removed owner can be recovered only by an explicit future
        // Profile operation. Refuse before changing the retained manifest.
        self.profile(workspace.owner)?;
        self.ensure_open_including_deleted(id)?;
        let at = Timestamp::now();
        if let Some(open) = self.open.get_mut(&id) {
            let before = open.manifest.version();
            open.manifest.set_workspace_deleted_at(None)?;
            let update = open.manifest.export_updates_since(&before)?;
            let manifest_id = open.manifest_id;
            commit_one(
                open,
                id,
                &mut self.committed,
                &mut self.sync_dirty,
                manifest_id,
                update,
                at,
                crate::ports::UpdateOrigin::Store,
            )?;
        }
        let documents: Vec<_> = self.open[&id]
            .repo
            .documents()?
            .into_iter()
            .map(|document| document.id)
            .collect();
        self.catalog.set_workspace_deleted(id, None, &documents)?;
        workspace.deleted_at = None;
        Ok(workspace)
    }

    /// Permanently remove soft-deleted workspaces whose retention window has
    /// elapsed. Backend bytes go first; a failed filesystem removal therefore
    /// leaves the catalog row available for a later retry.
    pub fn purge_deleted(&mut self, before: Timestamp) -> Result<usize> {
        let expired: Vec<_> = self
            .catalog
            .workspaces_including_deleted()?
            .into_iter()
            .filter(|workspace| {
                workspace
                    .deleted_at
                    .is_some_and(|at| at.millis() <= before.millis())
            })
            .map(|workspace| workspace.id)
            .collect();
        for id in &expired {
            // The catalog's RESTRICT fence must be checked before removing
            // backend bytes. Retained personal units have their own explicit
            // deletion boundary; expiry is not authority to destroy them.
            if self.catalog.workspace_has_personal_units(*id)? {
                return Err(Error::new(
                    Code::WorkspaceInUse,
                    "retained personal units prevent workspace purge",
                ));
            }
        }
        for id in &expired {
            self.open.remove(id);
            self.backend.delete_workspace(*id)?;
            self.catalog.purge_workspace(*id)?;
        }
        Ok(expired.len())
    }

    pub fn workspace(&self, id: WorkspaceId) -> Result<Workspace> {
        let workspace = self.workspace_including_deleted(id)?;
        if workspace.deleted_at.is_some() {
            return Err(Error::bare(Code::WorkspaceNotFound));
        }
        Ok(workspace)
    }

    pub fn workspace_including_deleted(&self, id: WorkspaceId) -> Result<Workspace> {
        self.catalog
            .workspace(id)?
            .ok_or_else(|| Error::bare(Code::WorkspaceNotFound))
    }

    pub fn lineage(&self, id: WorkspaceId) -> Result<Vec<Lineage>> {
        self.catalog.lineage(id)
    }

    /// Which active workspace currently owns a document.
    ///
    /// A document id is globally unique, so a client can name one without
    /// knowing where it lives. The normal path checks the indexed owner and
    /// opens **that** workspace to do it; only a row the owner disowns opens
    /// the others to rebuild the derived route. Verifying nothing would answer
    /// faster and sometimes wrongly; scanning every workspace database in turn
    /// would be the opposite of lazy.
    pub fn locate(&mut self, document: DocumentId) -> Result<Option<WorkspaceId>> {
        // A missing row remains an ordinary unknown-document lookup. Scanning
        // every workspace for arbitrary ids would turn an untrusted protocol
        // request into an expensive store walk. A stale row, by contrast, is
        // evidence that this is a real document whose derived route needs
        // repair.
        let Some(indexed) = self.catalog.locate(document)? else {
            return Ok(None);
        };

        // **The recorded owner keeps its claim unless it disclaims the
        // document *and* does not physically hold it.** Both halves are a
        // regression test, and requiring the manifest *and* the bytes — which
        // is what this did first — fails both:
        //
        // - A cross-workspace move publishes the manifest entry before the
        //   bytes arrive, so the destination lists a document it cannot yet
        //   open. Repairing then moves the route back and undoes the move
        //   (`a_recorded_owner_keeps_its_claim_when_its_manifest_lists_the_document`).
        // - `provision_replica_document` writes the bytes before the manifest
        //   entry, so a replica physically holds a document its manifest has
        //   not heard of yet (`replica_provisioning_repairs_a_missing_location_for_existing_bytes`
        //   and the two phantom-location tests beside it).
        //
        // `owner_disclaims` is the same predicate `reconcile_document_locations`
        // uses, deliberately: one answer to "has this owner lost its say",
        // including its rule that an owner which cannot be opened keeps its
        // claim, because a repair must never steal on the strength of an error.
        if !self.owner_disclaims(indexed, document)?
            || self.workspace_holds_document(indexed, document)?
        {
            return Ok(Some(indexed));
        }
        tracing::warn!(
            document = %document,
            indexed_workspace = %indexed,
            "document location index was stale; rebuilding route"
        );

        for workspace in self.catalog.workspaces()? {
            if self.workspace_owns_document(workspace.id, document)? {
                self.catalog.record_location(document, workspace.id)?;
                return Ok(Some(workspace.id));
            }
        }

        // The document is absent from every authoritative manifest or has no
        // durable row behind one. Forgetting the projection prevents the next
        // lookup from repeating a full repair scan; a later restore or manifest
        // commit records the route again.
        self.catalog.forget_location(document)?;
        Ok(None)
    }

    /// Note that a workspace was opened. Best-effort bookkeeping, never a
    /// reason to fail a request.
    pub fn touch(&mut self, id: WorkspaceId, at: Timestamp) {
        let _ = self.catalog.touch_workspace(id, at);
    }

    // ------------------------------------------------------------ documents

    /// The workspace's document list, from the derived index rather than by
    /// opening anything.
    pub fn documents(&mut self, workspace: WorkspaceId) -> Result<Vec<DocumentEntry>> {
        self.ensure_open(workspace)?;
        self.open[&workspace].repo.entries()
    }

    /// When each document in a workspace last changed, without opening bodies.
    ///
    /// **The manifest carries no timestamp**, and deliberately: it holds what
    /// a document *is*, not when it moved. Recency comes from the repository's
    /// own record, which is where [`Self::workspace_modified_at`] reads it too
    /// — this is that read without the `max`.
    pub fn document_updates(
        &mut self,
        workspace: WorkspaceId,
    ) -> Result<Vec<(DocumentId, Timestamp)>> {
        self.ensure_open(workspace)?;
        Ok(self.open[&workspace]
            .repo
            .documents()?
            .into_iter()
            .map(|document| (document.id, document.updated_at))
            .collect())
    }

    /// The newest stored change in a workspace, without opening document bodies.
    ///
    /// The manifest is a document too, so an otherwise-empty workspace still
    /// has a timestamp and structural changes participate in recency.
    pub fn workspace_modified_at(&mut self, workspace: WorkspaceId) -> Result<Timestamp> {
        self.ensure_open(workspace)?;
        self.open[&workspace]
            .repo
            .documents()?
            .into_iter()
            .map(|document| document.updated_at)
            .max()
            .ok_or_else(|| Error::new(Code::CorruptState, "this workspace has no documents"))
    }

    /// How many live listed entries a workspace holds and the blob metadata
    /// needed to measure the live references in the projection layer.
    ///
    /// Closed workspaces are read through their indexed repository directly:
    /// no manifest snapshot is decoded and the workspace is not retained in
    /// `open`. The manager asks for every workspace at once, so turning this
    /// into `ensure_open` would make one table visit load every manifest.
    pub fn workspace_usage(
        &mut self,
        workspace: WorkspaceId,
    ) -> Result<(std::collections::BTreeMap<String, u64>, Vec<Blob>)> {
        self.workspace_including_deleted(workspace)?;
        let usage = |repo: &B::Workspace| -> Result<_> {
            let mut kinds = std::collections::BTreeMap::new();
            for entry in repo
                .entries()?
                .into_iter()
                .filter(|entry| entry.trashed_at.is_none())
            {
                *kinds.entry(entry.kind).or_insert(0_u64) += 1;
            }
            Ok((kinds, repo.blobs()?))
        };
        match self.open.get(&workspace) {
            Some(open) => usage(&open.repo),
            None => usage(&self.backend.open_workspace(workspace)?),
        }
    }

    /// Register a new document in a workspace, and create its storage.
    ///
    /// Returns the id **and the manifest delta it produced**. The delta is not
    /// an implementation detail leaking out: a subscriber to the manifest room
    /// needs it, and this is the only place it exists. Without returning it,
    /// a document created over HTTP is invisible to every WebSocket client
    /// until it rejoins — which is a silent divergence between two clients
    /// doing perfectly ordinary things.
    ///
    /// `index` places it among `parent`'s children; `None` appends. A client
    /// that has an opinion — Koru's *New document*, which belongs below the
    /// page it was made from — sends one, and the position is applied inside
    /// the same manifest commit as the registration, so the delta that reaches
    /// every other client already has it in the right place.
    pub fn create_document(
        &mut self,
        workspace: WorkspaceId,
        title: &str,
        kind: &str,
        parent: Option<DocumentId>,
        index: Option<usize>,
        at: Timestamp,
    ) -> Result<(DocumentId, Vec<u8>)> {
        self.ensure_open(workspace)?;
        let id = DocumentId::new();
        let open = self.open.get_mut(&workspace).expect("just opened");

        let before = open.manifest.version();
        open.manifest
            .add_document_at(id, title, kind, parent, index)?;
        let delta = open.manifest.export_updates_since(&before)?;

        open.repo.create_document(id, DocumentKind::Document, at)?;
        let manifest_id = open.manifest_id;
        commit_one(
            open,
            workspace,
            &mut self.committed,
            &mut self.sync_dirty,
            manifest_id,
            delta.clone(),
            at,
            crate::ports::UpdateOrigin::Store,
        )?;
        open.repo.reindex(&open.manifest.entries())?;
        self.catalog.record_location(id, workspace)?;
        Ok((id, delta))
    }

    /// Update the workspace manifest's disposable title projection.
    ///
    /// For a Koru document the caller must first commit the authoritative
    /// row-zero title into the opaque document. Files have no Koru node model,
    /// so their manifest title is authoritative on its own.
    ///
    /// **A file's note is renamed with it.** The note's manifest title is the
    /// file's title — a note open in a tab says what it is a note *of* — so the
    /// two titles move in one manifest commit rather than drifting apart per
    /// caller.
    pub fn rename_document_entry(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        title: &str,
        at: Timestamp,
    ) -> Result<Vec<u8>> {
        self.ensure_open(workspace)?;
        let open = self.open.get_mut(&workspace).expect("just opened");
        // Read before the rename, so both edits share one version window, one
        // commit and one broadcast.
        let note = open.manifest.note_of(document)?;
        let before = open.manifest.version();
        open.manifest.rename_document(document, title)?;
        if let Some(note) = note {
            open.manifest.rename_document(note, title)?;
        }
        let delta = open.manifest.export_updates_since(&before)?;
        let manifest_id = open.manifest_id;
        commit_one(
            open,
            workspace,
            &mut self.committed,
            &mut self.sync_dirty,
            manifest_id,
            delta.clone(),
            at,
            crate::ports::UpdateOrigin::Store,
        )?;
        open.repo.reindex(&open.manifest.entries())?;
        Ok(delta)
    }

    /// The one note a file carries, creating it when it has none.
    ///
    /// **Idempotent**: a file whose manifest meta already names a note returns
    /// that note, so a retried or racing call never makes a second. Notes
    /// attach to files and nothing else — anything whose kind is not the
    /// application's file token is refused.
    ///
    /// The note is an ordinary document, created **empty** exactly as
    /// [`create_document`](Self::create_document)'s pages are: the store
    /// deliberately does not know what is inside a document (ADR-80), so the
    /// root node is seeded by whoever first opens the note, not written here.
    /// It is registered as the file's child and titled with the file's title,
    /// so the trash, the purge and the rename follow the file with no
    /// lifecycle code of its own; the file's meta carries the link back.
    ///
    /// Returns the note's id, whether this call created it, and the manifest
    /// delta. `created` is a fact the store knows — the idempotent path
    /// changed nothing, so its delta is empty and a broadcaster should skip
    /// it rather than push a no-op into the room.
    pub fn ensure_file_note(
        &mut self,
        workspace: WorkspaceId,
        file: DocumentId,
        at: Timestamp,
    ) -> Result<EnsuredFileNote> {
        self.ensure_open(workspace)?;
        let open = self.open.get_mut(&workspace).expect("just opened");
        let entry = open
            .manifest
            .entries()
            .into_iter()
            .find(|entry| entry.id == file)
            .ok_or_else(|| Error::bare(Code::DocumentNotFound))?;
        if let Some(note) = entry.note {
            return Ok(EnsuredFileNote {
                id: note,
                created: false,
                delta: Vec::new(),
            });
        }
        if entry.kind != KIND_FILE {
            return Err(Error::new(
                Code::InvalidRequest,
                "a note can only be attached to a file",
            ));
        }
        let title = entry.title.clone();
        let id = DocumentId::new();

        let before = open.manifest.version();
        open.manifest
            .add_document_at(id, &title, KIND_NOTE, Some(file), None)?;
        open.manifest.set_note(file, Some(id))?;
        let delta = open.manifest.export_updates_since(&before)?;

        open.repo.create_document(id, DocumentKind::Document, at)?;
        let manifest_id = open.manifest_id;
        commit_one(
            open,
            workspace,
            &mut self.committed,
            &mut self.sync_dirty,
            manifest_id,
            delta.clone(),
            at,
            crate::ports::UpdateOrigin::Store,
        )?;
        open.repo.reindex(&open.manifest.entries())?;
        self.catalog.record_location(id, workspace)?;
        Ok(EnsuredFileNote {
            id,
            created: true,
            delta,
        })
    }

    /// Move one document entry and its inherited subtree to workspace trash.
    /// Bytes remain locally available and a later manifest restore can expose
    /// the same document again.
    pub fn trash_document_entry(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        at: Timestamp,
        by: Option<&str>,
    ) -> Result<Vec<u8>> {
        self.ensure_open(workspace)?;
        let open = self.open.get_mut(&workspace).expect("just opened");
        let before = open.manifest.version();
        open.manifest.trash_document(document, at, by)?;
        let delta = open.manifest.export_updates_since(&before)?;
        let manifest_id = open.manifest_id;
        commit_one(
            open,
            workspace,
            &mut self.committed,
            &mut self.sync_dirty,
            manifest_id,
            delta.clone(),
            at,
            crate::ports::UpdateOrigin::Store,
        )?;
        open.repo.reindex(&open.manifest.entries())?;
        Ok(delta)
    }

    /// Register a document whose complete initial state is already available.
    ///
    /// The opaque document bytes are committed before the manifest row. If a
    /// later manifest step fails, the new storage row is removed and the
    /// in-memory manifest is restored, so an importer cannot leave an empty or
    /// half-registered page behind.
    pub fn import_document(
        &mut self,
        workspace: WorkspaceId,
        title: &str,
        kind: &str,
        parent: Option<DocumentId>,
        snapshot: Vec<u8>,
        at: Timestamp,
    ) -> Result<(DocumentId, Vec<u8>)> {
        self.import_document_at(
            workspace,
            DocumentId::new(),
            title,
            kind,
            parent,
            None,
            snapshot,
            at,
        )
    }

    /// Register an imported document at a client-minted identity.
    pub fn import_document_at(
        &mut self,
        workspace: WorkspaceId,
        id: DocumentId,
        title: &str,
        kind: &str,
        parent: Option<DocumentId>,
        index: Option<usize>,
        snapshot: Vec<u8>,
        at: Timestamp,
    ) -> Result<(DocumentId, Vec<u8>)> {
        self.import_document_at_with_mime(
            workspace, id, title, kind, None, parent, index, snapshot, at,
        )
    }

    /// Register an imported document and optional file metadata without
    /// requiring listing consumers to inspect its opaque snapshot.
    #[allow(clippy::too_many_arguments)]
    pub fn import_document_at_with_mime(
        &mut self,
        workspace: WorkspaceId,
        id: DocumentId,
        title: &str,
        kind: &str,
        mime: Option<&str>,
        parent: Option<DocumentId>,
        index: Option<usize>,
        snapshot: Vec<u8>,
        at: Timestamp,
    ) -> Result<(DocumentId, Vec<u8>)> {
        self.import_document_at_with_mime_and_trash(
            workspace, id, title, kind, mime, parent, index, None, snapshot, at,
        )
    }

    /// Register an imported document with optional file metadata and an
    /// optional source trash timestamp in one durable manifest publication.
    #[allow(clippy::too_many_arguments)]
    pub fn import_document_at_with_mime_and_trash(
        &mut self,
        workspace: WorkspaceId,
        id: DocumentId,
        title: &str,
        kind: &str,
        mime: Option<&str>,
        parent: Option<DocumentId>,
        index: Option<usize>,
        trashed_at: Option<Timestamp>,
        snapshot: Vec<u8>,
        at: Timestamp,
    ) -> Result<(DocumentId, Vec<u8>)> {
        if snapshot.is_empty() {
            return Err(Error::new(
                Code::InvalidRequest,
                "an imported document snapshot is empty",
            ));
        }
        self.ensure_open(workspace)?;
        let open = self.open.get_mut(&workspace).expect("just opened");
        let manifest_before = open.manifest.export_snapshot()?;

        open.repo.create_document(id, DocumentKind::Document, at)?;
        if let Err(error) = open.repo.commit_updates(&crate::ports::UpdateBatch {
            document: id,
            updates: vec![snapshot],
            version_vector: Vec::new(),
            frontiers: Vec::new(),
            received_at: at,
            origins: vec![crate::ports::UpdateOrigin::Store],
        }) {
            let _ = open.repo.drop_document(id);
            return Err(error);
        }

        let result = (|| {
            let before = open.manifest.version();
            open.manifest.add_document_at_with_mime_and_trash(
                id, title, kind, mime, parent, index, trashed_at,
            )?;
            let delta = open.manifest.export_updates_since(&before)?;
            let manifest_id = open.manifest_id;
            commit_one(
                open,
                workspace,
                &mut self.committed,
                &mut self.sync_dirty,
                manifest_id,
                delta.clone(),
                at,
                crate::ports::UpdateOrigin::Store,
            )?;
            open.repo.reindex(&open.manifest.entries())?;
            Ok(delta)
        })();
        let delta = match result {
            Ok(delta) => delta,
            Err(error) => {
                if let Ok(restored) = Manifest::from_snapshot(&manifest_before) {
                    open.manifest = restored;
                }
                let _ = open.repo.drop_document(id);
                return Err(error);
            }
        };
        self.catalog.record_location(id, workspace)?;
        Ok((id, delta))
    }
    /// Move a document and its explore subtree from one workspace to another
    /// inside the same profile store.
    ///
    /// **Preserves `DocumentId`s.** Each document's opaque snapshot and the
    /// blobs it references are written into the target first; only then is the
    /// source manifest detached. A failure mid-way leaves the source intact.
    ///
    /// `parent` / `index` place the *root* in the target library. Nested pages
    /// keep their relative parentage inside the moving set.
    /// Returns the moved ids (root first) and the manifest deltas for
    /// `(source, target)` so both rooms can be broadcast.
    pub fn transfer_documents(
        &mut self,
        source: WorkspaceId,
        target: WorkspaceId,
        root: DocumentId,
        parent: Option<DocumentId>,
        index: usize,
        at: Timestamp,
    ) -> Result<(Vec<DocumentId>, Vec<u8>, Vec<u8>)> {
        if source == target {
            return Err(Error::new(
                Code::InvalidRequest,
                "transfer is for a different workspace; use move within one",
            ));
        }
        self.ensure_open(source)?;
        self.ensure_open(target)?;

        // Expand the explore subtree in source-manifest order (parents first).
        let moving = {
            let src = &self.open[&source];
            if !src.manifest.contains(root) {
                return Err(Error::bare(Code::DocumentNotFound));
            }
            let entries = src.manifest.entries();
            let mut ids = vec![root];
            let mut changed = true;
            while changed {
                changed = false;
                for e in &entries {
                    if let Some(p) = e.parent
                        && ids.contains(&p)
                        && !ids.contains(&e.id)
                    {
                        ids.push(e.id);
                        changed = true;
                    }
                }
            }
            // Capture listing fields before anything mutates.
            let meta: Vec<_> = ids
                .iter()
                .filter_map(|id| entries.iter().find(|e| e.id == *id).cloned())
                .collect();
            (ids, meta)
        };
        let (ids, meta) = moving;
        if ids.is_empty() {
            return Err(Error::bare(Code::DocumentNotFound));
        }

        // Snapshots + blob hashes from source.
        let mut snapshots: Vec<(DocumentId, Vec<u8>)> = Vec::with_capacity(ids.len());
        let mut blob_hashes: Vec<BlobHash> = Vec::new();
        {
            for id in &ids {
                self.ensure_document(source, *id)?;
            }
            let src = &self.open[&source];
            for id in &ids {
                let bytes = src.documents[id].export_snapshot()?;
                snapshots.push((*id, bytes));
            }
            // Every blob the source workspace is associated with. The target
            // gains the same associations below; extras are harmless and a
            // miss would break images after the move.
            for b in src.repo.blobs()? {
                blob_hashes.push(b.hash);
            }
        }
        // Each reader's private view state travels with the document: it is
        // keyed by the document id, which a transfer keeps, and the source
        // rows cascade away when the source drops its bytes below.
        let mut private_rows: Vec<(DocumentId, ProfileId, Vec<u8>)> = Vec::new();
        for id in &ids {
            for (owner, bytes) in self.open[&source].repo.private_snapshots(*id)? {
                private_rows.push((*id, owner, bytes));
            }
        }

        // Target parent must exist there (and not be in the moving set unless
        // it is the destination parent outside the set).
        if let Some(p) = parent {
            let tgt = &self.open[&target];
            if ids.contains(&p) {
                return Err(Error::new(
                    Code::InvalidRequest,
                    "destination parent is inside the documents being moved",
                ));
            }
            if !tgt.manifest.contains(p) {
                return Err(Error::new(
                    Code::InvalidRequest,
                    "destination parent is not in the target workspace",
                ));
            }
        }

        // --- write target first ------------------------------------------------
        {
            // **No byte copy.** There is one object per store root, so moving
            // documents between Workspaces moves *associations*: the rows
            // mirrored below are the whole of it. The loop here only gives the
            // lazy migration its chance, in case a store written by an older
            // build still has these bytes in the source Workspace's own
            // directory.
            //
            // A blob whose bytes cannot be found is recorded and the move goes
            // on. Refusing the whole transfer would strand the documents in
            // the source Workspace over a picture that is already missing,
            // which trades a visible gap for an invisible one.
            for hash in &blob_hashes {
                if !self.ensure_object(source, hash)? {
                    self.recoveries.push(format!(
                        "workspace {source}: blob {hash} had no bytes when documents moved to \
                         {target}"
                    ));
                }
            }
            // Mirror blob meta rows into the target repo.
            let src_meta = self.open[&source].repo.blobs()?;
            let open = self.open.get_mut(&target).expect("opened");
            for b in src_meta {
                if blob_hashes.iter().any(|h| *h == b.hash) {
                    let _ = open.repo.put_blob(&b);
                }
            }
        }

        let target_delta = {
            let open = self.open.get_mut(&target).expect("opened");
            for (id, bytes) in &snapshots {
                // Resume a half-finished transfer: storage without a manifest
                // row is leftover from a bug; replace bytes and continue.
                if open.repo.document(*id)?.is_some() {
                    if open.manifest.contains(*id) {
                        return Err(Error::new(
                            Code::DocumentExists,
                            "target workspace already holds that document id",
                        ));
                    }
                    open.repo.drop_document(*id)?;
                }
                open.repo.create_document(*id, DocumentKind::Document, at)?;
                open.repo.commit_updates(&crate::ports::UpdateBatch {
                    document: *id,
                    updates: vec![bytes.clone()],
                    version_vector: Vec::new(),
                    frontiers: Vec::new(),
                    received_at: at,
                    origins: vec![crate::ports::UpdateOrigin::Store],
                })?;
            }
            for (id, owner, bytes) in &private_rows {
                open.repo.put_private_snapshot(*id, *owner, bytes, at)?;
            }

            // Manifest rows: roots under `parent`, nested under remapped parents.
            let before = open.manifest.version();
            // Order parents before children so add_document's parent exists.
            let mut remaining = meta.clone();
            let mut placed = std::collections::HashSet::new();
            // First place the root at the requested spot by adding then moving.
            while !remaining.is_empty() {
                let before_len = remaining.len();
                remaining.retain(|e| {
                    let new_parent = if e.id == root {
                        parent
                    } else {
                        e.parent.filter(|p| ids.contains(p)).or(Some(root))
                    };
                    let parent_ready = match new_parent {
                        None => true,
                        Some(p) if p == root && placed.contains(&root) => true,
                        Some(p) if placed.contains(&p) => true,
                        Some(p) if !ids.contains(&p) => {
                            // External parent only allowed for the root.
                            e.id == root && p == parent.unwrap_or(p)
                        }
                        _ => false,
                    };
                    if !parent_ready && e.id != root {
                        return true; // keep
                    }
                    if e.id != root && !parent_ready {
                        return true;
                    }
                    let use_parent = if e.id == root { parent } else { new_parent };
                    if open
                        .manifest
                        .add_document_at_with_mime(
                            e.id,
                            &e.title,
                            &e.kind,
                            e.mime.as_deref(),
                            use_parent,
                            None,
                        )
                        .is_err()
                    {
                        return true;
                    }
                    placed.insert(e.id);
                    false
                });
                if remaining.len() == before_len {
                    return Err(Error::new(
                        Code::Internal,
                        "could not place transferred documents into the target manifest",
                    ));
                }
            }
            // Position the root among its new siblings.
            let siblings = open
                .manifest
                .entries()
                .into_iter()
                .filter(|e| e.parent == parent && e.id != root)
                .count();
            open.manifest
                .move_document(root, parent, index.min(siblings))?;
            let target_delta = open.manifest.export_updates_since(&before)?;
            let manifest_id = open.manifest_id;
            commit_one(
                open,
                target,
                &mut self.committed,
                &mut self.sync_dirty,
                manifest_id,
                target_delta.clone(),
                at,
                crate::ports::UpdateOrigin::Store,
            )?;
            open.repo.reindex(&open.manifest.entries())?;
            target_delta
        };

        for id in &ids {
            self.catalog.record_location(*id, target)?;
        }

        // --- detach source -----------------------------------------------------
        let source_delta = {
            let open = self.open.get_mut(&source).expect("opened");
            let before = open.manifest.version();
            let _ = open.manifest.remove_document(root)?;
            let delta = open.manifest.export_updates_since(&before)?;
            let manifest_id = open.manifest_id;
            commit_one(
                open,
                source,
                &mut self.committed,
                &mut self.sync_dirty,
                manifest_id,
                delta.clone(),
                at,
                crate::ports::UpdateOrigin::Store,
            )?;
            open.repo.reindex(&open.manifest.entries())?;
            // **Bytes leave the source.** Keeping orphan storage rows made a
            // reverse transfer (undo) fail with DocumentExists: the former
            // source still held the id under the covers. Target already has
            // the snapshots; drop source rows now.
            for id in &ids {
                open.documents.remove(id);
                open.repo.drop_document(*id)?;
            }
            delta
        };

        Ok((ids, source_delta, target_delta))
    }

    /// Plant a complete opaque document before publishing its manifest entry.
    ///
    /// This deliberately uses two durable transactions in the safe order:
    /// content plus idempotency first, manifest second. A crash between them
    /// leaves hidden, recoverable bytes; it can never leave a visible empty
    /// document. Retrying the same request completes the missing manifest step
    /// and returns the original content bytes.
    #[allow(clippy::too_many_arguments)]
    pub fn create_seeded_document(
        &mut self,
        workspace: WorkspaceId,
        title: &str,
        kind: &str,
        parent: Option<DocumentId>,
        index: Option<usize>,
        scope: &str,
        idempotency_key: &str,
        request_hash: &str,
        candidate: Option<DocumentId>,
        initial_update: Vec<u8>,
        at: Timestamp,
    ) -> Result<SeededCreation> {
        if idempotency_key.is_empty() || idempotency_key.len() > 128 {
            return Err(Error::new(
                Code::InvalidRequest,
                "idempotency_key must contain 1 to 128 bytes",
            ));
        }
        if request_hash.is_empty() || request_hash.len() > 128 {
            return Err(Error::new(
                Code::InvalidRequest,
                "request_hash must contain 1 to 128 bytes",
            ));
        }
        if let Some(candidate) = candidate {
            let mut owner = None;
            for known in self.catalog.workspaces_including_deleted()? {
                let exists = match self.open.get(&known.id) {
                    Some(open) => open.repo.document(candidate)?.is_some(),
                    None => self
                        .backend
                        .open_workspace(known.id)?
                        .document(candidate)?
                        .is_some(),
                };
                if !exists {
                    continue;
                }
                if owner.replace(known.id).is_some() || known.id != workspace {
                    return Err(Error::new(
                        Code::IdempotencyConflict,
                        "the candidate identity belongs to another workspace",
                    ));
                }
            }
        }
        self.ensure_open(workspace)?;

        let recover_candidate = candidate.is_some();
        let candidate = candidate.unwrap_or_default();
        let mut validation = LiveDocument::empty(candidate);
        validation.import(&initial_update)?;
        let expires_before =
            Timestamp::from_millis(at.millis().saturating_sub(7_i64 * 24 * 60 * 60 * 1_000));

        let (document, original_update, replayed, manifest_delta) = {
            let open = self.open.get_mut(&workspace).expect("just opened");
            if recover_candidate
                && let Some(existing) = open
                    .manifest
                    .entries()
                    .into_iter()
                    .find(|entry| entry.id == candidate)
                && existing.kind != kind
            {
                return Err(Error::new(
                    Code::IdempotencyConflict,
                    "the candidate identity belongs to another manifest kind",
                ));
            }
            let durable = open.repo.seed_document(&SeedDocument {
                candidate,
                recover_candidate,
                scope: scope.to_owned(),
                key: idempotency_key.to_owned(),
                request_hash: request_hash.to_owned(),
                initial_update,
                version_vector: validation.encoded_version(),
                frontiers: validation.encoded_frontiers(),
                created_at: at,
                expires_before,
            })?;
            let document = durable.document;

            // Open from the repository rather than from `initial_update`: on a
            // replay the document may have received later edits, and replacing
            // the live Store replica with its creation state would rewind it.
            if !open.documents.contains_key(&document) {
                let live = open_document(&open.repo, document)?.into_value();
                live.set_peer_id(self.peer_id)?;
                open.documents.insert(document, live);
            }

            if !durable.replayed {
                let live = &open.documents[&document];
                self.committed.insert(
                    (workspace, document),
                    Committed {
                        workspace,
                        document,
                        seq: 1,
                        version_vector: live.encoded_version(),
                        is_manifest: false,
                    },
                );
                self.sync_dirty
                    .entry(workspace)
                    .or_default()
                    .insert(document);
            }

            let manifest_delta = if open.manifest.contains_document(document) {
                None
            } else {
                let rollback = open.manifest.export_snapshot()?;
                let peer = open.manifest.peer_id();
                let before = open.manifest.version();
                open.manifest
                    .add_document_at(document, title, kind, parent, index)?;
                let delta = open.manifest.export_updates_since(&before)?;
                let manifest_id = open.manifest_id;
                if let Err(error) = commit_one(
                    open,
                    workspace,
                    &mut self.committed,
                    &mut self.sync_dirty,
                    manifest_id,
                    delta.clone(),
                    at,
                    crate::ports::UpdateOrigin::Store,
                ) {
                    let restored = Manifest::from_snapshot(&rollback)?;
                    restored.set_peer_id(peer)?;
                    open.manifest = restored;
                    return Err(error);
                }
                open.repo.reindex(&open.manifest.entries())?;
                Some(delta)
            };
            (
                document,
                durable.initial_update,
                durable.replayed,
                manifest_delta,
            )
        };

        self.catalog.record_location(document, workspace)?;
        Ok(SeededCreation {
            document,
            initial_update: original_update,
            replayed,
            manifest_delta,
        })
    }

    /// The manifest's document id, which is what a client joins to see the
    /// workspace's contents.
    pub fn manifest_id(&mut self, workspace: WorkspaceId) -> Result<DocumentId> {
        self.ensure_open(workspace)?;
        Ok(self.open[&workspace].manifest_id)
    }

    /// A snapshot of a document, for a client that has none.
    ///
    /// A full snapshot rather than a delta: the receiver has no history, and a
    /// delta from a non-empty version names ancestors it cannot obtain — Loro
    /// would park it as pending and apply nothing, with no error to notice.
    pub fn snapshot(&mut self, workspace: WorkspaceId, document: DocumentId) -> Result<Vec<u8>> {
        self.ensure_document(workspace, document)?;
        let open = &self.open[&workspace];
        let manifest = open.is_manifest(document);
        let bytes = if manifest {
            open.manifest.export_snapshot()?
        } else {
            open.documents[&document].export_snapshot()?
        };
        // **What a client is about to be handed.** A snapshot is the expensive
        // call on the open path — opening is O(the document) — so its size is
        // the number to look at first when a workspace is slow to appear.
        tracing::debug!(
            %workspace,
            %document,
            manifest,
            bytes = bytes.len(),
            "snapshot {} ({} bytes)",
            document,
            bytes.len()
        );
        Ok(bytes)
    }

    /// Append a visible history marker, deduplicating the immutable state it
    /// names. Exact-frontier capture forks the live CRDT and never checks out
    /// or pauses the editor's replica.
    pub fn capture_history(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        frontiers: Option<&[u8]>,
        reason: HistoryReason,
        at: Timestamp,
        label: Option<&str>,
        message: Option<&str>,
        boundary: Option<&str>,
    ) -> Result<HistoryEntry> {
        self.ensure_document(workspace, document)?;
        let open = self.open.get_mut(&workspace).expect("just opened");
        if open.is_manifest(document) {
            return Err(Error::new(
                Code::InvalidRequest,
                "the manifest has no user history",
            ));
        }
        let (snapshot, version_vector, actual_frontiers) =
            open.documents[&document].snapshot_at(frontiers)?;
        let checksum = crate::lifecycle::checksum(&snapshot);
        open.repo.capture_history(&HistoryCapture {
            document,
            snapshot: &snapshot,
            version_vector: &version_vector,
            frontiers: &actual_frontiers,
            checksum: &checksum,
            reason,
            recorded_at: at,
            label,
            message,
            boundary,
        })
    }

    /// Persist state already forked/exported by a background worker. This is
    /// the automatic-history path: the worker takes an O(1) fork under the
    /// Store lock, performs snapshot encoding outside it, then briefly returns
    /// to append the version and marker transaction.
    pub fn record_history_state(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        snapshot: &[u8],
        version_vector: &[u8],
        frontiers: &[u8],
        reason: HistoryReason,
        at: Timestamp,
    ) -> Result<HistoryEntry> {
        self.ensure_document(workspace, document)?;
        let checksum = crate::lifecycle::checksum(snapshot);
        self.open
            .get_mut(&workspace)
            .expect("just opened")
            .repo
            .capture_history(&HistoryCapture {
                document,
                snapshot,
                version_vector,
                frontiers,
                checksum: &checksum,
                reason,
                recorded_at: at,
                label: None,
                message: None,
                boundary: None,
            })
    }

    pub fn history_entries(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        before: Option<(Timestamp, HistoryEntryId)>,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>> {
        self.ensure_document(workspace, document)?;
        self.open
            .get(&workspace)
            .expect("just opened")
            .repo
            .history_entries(document, before, limit)
    }

    /// Rename an existing marker, or make it unnamed again with `None`.
    ///
    /// **The only thing about a marker that can change.** Everything else it
    /// records — which version, why, when — is what happened, and history that
    /// can be edited is not history. A label is what a person calls the moment,
    /// so correcting a typo in one rewrites nothing.
    ///
    /// The lookup is scoped to `workspace`'s repository, so a marker id from
    /// another workspace is not found rather than renamed — the same property
    /// [`Self::history_version`] has, and for the same reason.
    pub fn relabel_history(
        &mut self,
        workspace: WorkspaceId,
        entry: HistoryEntryId,
        label: Option<&str>,
    ) -> Result<HistoryEntry> {
        self.ensure_open(workspace)?;
        self.open
            .get_mut(&workspace)
            .expect("just opened")
            .repo
            .relabel_history_entry(entry, label)
    }

    /// Whether `document` has changed since its newest history marker.
    ///
    /// **The store's own comparison, and the reason it belongs here.** The only
    /// other change detection in the tree is the projection worker's
    /// `AutoHistory`, which is a process-local `HashMap` that is lost on
    /// restart and never leaves the server. This reads two durable facts — the
    /// version vector stored beside the newest marker, and the version of the
    /// replica this store holds — so it answers the same after a restart, and
    /// it counts a peer's edit the moment that edit reaches the store.
    ///
    /// `Ok(None)` means the repository cannot say; it travels the wire as
    /// `stale: null` and a caller must not read it as *unchanged*.
    /// `Ok(Some(true))` when there are no markers: there is nothing to be
    /// unchanged relative to.
    ///
    /// Not stale means the marker's version **includes** the live one — not
    /// that the two encode to the same bytes. A marker restored from a bundle
    /// can hold ops this replica has not seen, and that is still "nothing has
    /// changed since".
    pub fn history_staleness(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<Option<bool>> {
        self.ensure_document(workspace, document)?;
        let open = self.open.get(&workspace).expect("just opened");
        if open.is_manifest(document) {
            return Ok(None);
        }
        let Some(encoded) = open.repo.newest_history_version(document)? else {
            return Ok(Some(true));
        };
        let Ok(marker) = loro::VersionVector::decode(&encoded) else {
            // A version vector we cannot decode is not a claim that nothing has
            // changed. Say nothing rather than something reassuring.
            return Ok(None);
        };
        let live = open.documents[&document].version();
        Ok(Some(!marker.includes_vv(&live)))
    }

    pub fn history_version(
        &mut self,
        workspace: WorkspaceId,
        entry: HistoryEntryId,
    ) -> Result<DocumentVersion> {
        self.ensure_open(workspace)?;
        let open = self.open.get(&workspace).expect("just opened");
        let marker = open
            .repo
            .history_entry(entry)?
            .ok_or_else(|| Error::bare(Code::HistoryEntryNotFound))?;
        open.repo
            .document_version(marker.version)?
            .ok_or_else(|| Error::bare(Code::HistoryEntryNotFound))
    }

    /// The immutable state a version id names, checksum-verified, or `None`
    /// when this workspace's repository holds no such version.
    ///
    /// **By version rather than by marker**, which is what a *pin* is: a
    /// delegated run keeps a `DocumentVersionId` (ROADMAP §7.2), not the
    /// history entry that happened to record it, because a marker can be
    /// relabelled and a version cannot be anything but its bytes. Scoped to
    /// `workspace`'s repository like [`Self::history_version`], so a version
    /// id from another workspace is not found rather than served.
    ///
    /// `None` is the honest answer after a physical purge: `document_versions`
    /// cascade when a document's rows are dropped, and nothing here — or in
    /// any caller — is allowed to keep a version alive past that.
    pub fn document_version(
        &mut self,
        workspace: WorkspaceId,
        version: DocumentVersionId,
    ) -> Result<Option<DocumentVersion>> {
        self.ensure_open(workspace)?;
        self.open
            .get(&workspace)
            .expect("just opened")
            .repo
            .document_version(version)
    }

    /// Everything this store has that the caller's version vector does not.
    pub fn updates_since(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        from: &loro::VersionVector,
    ) -> Result<Vec<u8>> {
        self.ensure_document(workspace, document)?;
        let open = &self.open[&workspace];
        if open.is_manifest(document) {
            return open.manifest.export_updates_since(from);
        }
        open.documents[&document].export_updates_since(from)
    }

    pub fn version(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<loro::VersionVector> {
        self.ensure_document(workspace, document)?;
        let open = &self.open[&workspace];
        if open.is_manifest(document) {
            return Ok(open.manifest.version());
        }
        Ok(open.documents[&document].version())
    }

    // ---------------------------------------------------------------- blobs
    //
    // **Two questions, never one.** `object_present(hash)` asks whether the
    // bytes are on this device; `workspace_has_blob(workspace, hash)` asks
    // whether this Workspace may read them. Bytes live once per store root and
    // the `workspace.db.blobs` row is the permission, so nothing below infers
    // the second answer from the first — the file's presence grants nothing.

    /// Begin receiving a blob without tying the upload lifetime to this Store
    /// borrow. The caller may release its Store mutex immediately after this
    /// returns and perform all body I/O through the owned value.
    ///
    /// The Workspace is opened rather than used to choose a byte store: the
    /// bytes go to the one object store, and the *association* is written by
    /// [`finish_blob_upload`](Self::finish_blob_upload) once they land.
    pub fn begin_blob_upload(
        &mut self,
        workspace: WorkspaceId,
    ) -> Result<Box<dyn PendingBlobUpload>> {
        self.ensure_open(workspace)?;
        self.backend.objects()?.begin()
    }

    /// The `workspace.db.blobs` row, or `BlobNotFound`.
    ///
    /// **Every read goes through this first.** A hash is never authority, and
    /// since B1 the bytes are shared, so the row is the only thing standing
    /// between one Workspace's picture and another Workspace's document that
    /// happens to name the same hash.
    fn require_association(&mut self, workspace: WorkspaceId, hash: &BlobHash) -> Result<()> {
        if self.open[&workspace].repo.blob(hash)?.is_none() {
            return Err(Error::bare(Code::BlobNotFound));
        }
        Ok(())
    }

    /// Whether the object is on this device, promoting a legacy per-Workspace
    /// file into the object store if that is still where the bytes are.
    ///
    /// **This is about bytes, and bytes are not permission** — the caller has
    /// already established the association, or is answering a question about
    /// one. Start-up hashes nothing: a miss is what makes this device look, so
    /// a store that never held the old layout pays one `exists` call.
    ///
    /// A candidate whose bytes do not hash to the name it sits under is
    /// refused and **recorded**, never installed and never moved. The read
    /// then reports the object as absent, which is true: those bytes are not
    /// here.
    fn ensure_object(&mut self, workspace: WorkspaceId, hash: &BlobHash) -> Result<bool> {
        if self.backend.objects()?.has(hash)? {
            return Ok(true);
        }
        match self.backend.promote_legacy_object(workspace, hash)? {
            LegacyPromotion::Installed => Ok(true),
            LegacyPromotion::NoCandidate => Ok(false),
            LegacyPromotion::Mismatch { found } => {
                self.recoveries.push(format!(
                    "workspace {workspace}: the legacy file for blob {hash} holds {found}; \
                     it was left where it is and nothing was promoted"
                ));
                tracing::warn!(
                    workspace = %workspace,
                    expected = %hash,
                    found = %found,
                    "a legacy blob does not match the name it is filed under"
                );
                Ok(false)
            }
        }
    }

    /// Re-hash the object and set it aside if the bytes have rotted.
    ///
    /// Quarantine **copies**: the document still references these bytes, and a
    /// reference to nothing is worse than one to something broken.
    fn verify_object(&mut self, workspace: WorkspaceId, hash: &BlobHash) -> Result<()> {
        let objects = self.backend.objects()?;
        if !objects.verify(hash)? {
            let location = objects.quarantine(hash).unwrap_or_default();
            self.recoveries.push(format!(
                "workspace {workspace}: blob {hash} failed its checksum"
            ));
            return Err(Error::new(
                Code::BlobChecksumMismatch,
                format!(
                    "the stored bytes no longer match this hash; a copy was set aside at {location}"
                ),
            ));
        }
        Ok(())
    }

    /// Record metadata for bytes an owned pending upload has already
    /// published. This is the short Store-locked part of a streaming upload.
    pub fn finish_blob_upload(
        &mut self,
        workspace: WorkspaceId,
        completed: CompletedBlobUpload,
        mime: &str,
        filename: Option<&str>,
        at: Timestamp,
        origin_device: Option<&str>,
    ) -> Result<Blob> {
        self.ensure_open(workspace)?;
        let mut attributes = std::collections::BTreeMap::new();
        if let Some(device) = origin_device {
            attributes.insert(
                crate::model::blob_attr::ORIGIN_DEVICE.to_string(),
                crate::model::BlobAttribute::Text(device.to_string()),
            );
        }
        let blob = Blob {
            hash: completed.hash,
            size: completed.size,
            mime: mime.to_string(),
            filename: filename.map(str::to_string),
            created_at: at,
            attributes,
        };
        self.open
            .get_mut(&workspace)
            .expect("just opened")
            .repo
            .put_blob(&blob)?;
        Ok(blob)
    }

    /// Store bytes and record what they are.
    ///
    /// The hash comes back from the blob store rather than being asserted by
    /// the caller: content addressing means the bytes decide their own name.
    /// `filename` is **metadata** — arbitrary Unicode, never a path component,
    /// never identity. `origin_device` is the storing device's id in storage
    /// form, when the caller knows it; it is recorded once and never
    /// overwritten by a later store of the same bytes.
    pub fn put_blob(
        &mut self,
        workspace: WorkspaceId,
        bytes: &[u8],
        mime: &str,
        filename: Option<&str>,
        at: Timestamp,
        origin_device: Option<&str>,
    ) -> Result<Blob> {
        self.ensure_open(workspace)?;
        let hash = self.backend.objects()?.put(bytes)?;
        self.finish_blob_upload(
            workspace,
            CompletedBlobUpload {
                hash,
                size: bytes.len() as u64,
            },
            mime,
            filename,
            at,
            origin_device,
        )
    }

    /// Store bytes that arrived from a peer, keeping the record as announced.
    ///
    /// Unlike [`put_blob`](Self::put_blob), which *describes* a local store,
    /// this preserves the announcing device's metadata — above all its
    /// `attributes`, where `origin_device` names whoever first held the bytes.
    /// Stamping the serving peer instead would rewrite provenance on every
    /// hop, and this device is known as a holder through the replication
    /// runtime's directory rather than by claiming to be the origin.
    pub fn put_remote_blob(
        &mut self,
        workspace: WorkspaceId,
        blob: &Blob,
        bytes: &[u8],
    ) -> Result<Blob> {
        self.ensure_open(workspace)?;
        let written = self.backend.objects()?.put(bytes)?;
        if written != blob.hash || bytes.len() as u64 != blob.size {
            return Err(Error::new(
                Code::BlobChecksumMismatch,
                "the received bytes do not hash to the name the peer announced",
            ));
        }
        self.open
            .get_mut(&workspace)
            .expect("just opened")
            .repo
            .put_blob(blob)?;
        Ok(blob.clone())
    }

    /// Associate a Workspace with a blob a sibling of the same Profile already
    /// holds. **No byte copy.**
    ///
    /// This was `copy_blob`, and the rename is the point rather than tidiness.
    /// There is one object per store root, so moving or copying a picture
    /// between Workspaces is *writing a row*; a caller still thinking in bytes
    /// would rebuild the per-Workspace layout B1 removed, and would do it by
    /// duplicating somebody's photograph library.
    ///
    /// The source association is required and the object is verified before
    /// the target row is written, because the caller publishes its CRDT
    /// reference only after this succeeds — a reference must never be able to
    /// name bytes that are absent or rotted. The metadata row is carried
    /// across unchanged, so provenance follows the association the way it used
    /// to follow the bytes.
    pub fn associate_blob(
        &mut self,
        source: WorkspaceId,
        target: WorkspaceId,
        hash: &BlobHash,
    ) -> Result<Blob> {
        let source_workspace = self.workspace(source)?;
        let target_workspace = self.workspace(target)?;
        if source_workspace.owner != target_workspace.owner {
            return Err(Error::new(
                Code::InvalidRequest,
                "a blob can only be associated with a workspace in the same profile",
            ));
        }
        self.ensure_open(source)?;
        self.ensure_open(target)?;

        // The source's row is the permission, and it is read before anything
        // looks at the disk: a global object nobody is associated with must
        // not become readable by asking for it from a Workspace that is.
        let blob = self.open[&source]
            .repo
            .blob(hash)?
            .ok_or_else(|| Error::bare(Code::BlobNotFound))?;
        if !self.ensure_object(source, hash)? {
            return Err(Error::bare(Code::BlobNotFound));
        }
        self.verify_object(source, hash)?;
        self.open
            .get_mut(&target)
            .expect("just opened")
            .repo
            .put_blob(&blob)?;
        Ok(blob)
    }

    /// Repair a cross-Workspace reference whose target Workspace has no
    /// `blobs` row, from a sibling Workspace of the same Profile.
    ///
    /// Older selection moves published the CRDT node without its
    /// Workspace-scoped bytes. A local read miss is the safe repair point: the
    /// document already names the exact content hash, and only a sibling may
    /// supply the permission.
    ///
    /// **Orchestrated here and nowhere else.** Only this type holds the
    /// catalog, so only this type can answer "which sibling Workspaces exist";
    /// a blob store and a `WorkspaceRepo` each see one `workspace.db` and
    /// cannot see the question at all. It searches **associations**, never
    /// files — a sibling qualifies by holding a row, and the global object's
    /// presence is never treated as permission — and it never searches another
    /// Profile.
    pub fn repair_blob_association(
        &mut self,
        target: WorkspaceId,
        hash: &BlobHash,
    ) -> Result<bool> {
        let owner = self.workspace(target)?.owner;
        let candidates: Vec<_> = self
            .catalog
            .workspaces()?
            .into_iter()
            .filter(|workspace| workspace.owner == owner && workspace.id != target)
            .map(|workspace| workspace.id)
            .collect();
        for source in candidates {
            match self.associate_blob(source, target, hash) {
                Ok(_) => return Ok(true),
                Err(error) if error.code() == Code::BlobNotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(false)
    }

    /// The bytes, verified against the name they are filed under.
    ///
    /// **Verification is not optional here.** A blob is the one thing whose
    /// identity is its checksum, so serving one that no longer hashes to its
    /// own name would be serving something else entirely — silently, and into
    /// a document that believes it knows what it is getting.
    /// **The association is checked first**, because the object is shared: the
    /// same file serves every Workspace associated with it, and a Workspace
    /// that is not associated is refused even though the bytes are right there.
    pub fn get_blob(&mut self, workspace: WorkspaceId, hash: &BlobHash) -> Result<Vec<u8>> {
        self.ensure_open(workspace)?;
        self.require_association(workspace, hash)?;
        if !self.ensure_object(workspace, hash)? {
            return Err(Error::bare(Code::BlobNotFound));
        }
        let bytes = self.backend.objects()?.get(hash)?;
        // Copied aside, never removed: the document still references it, and a
        // reference to nothing is worse than one to something broken.
        self.verify_object(workspace, hash)?;
        Ok(bytes)
    }

    /// **Bytes exist locally**, anywhere on this device.
    ///
    /// One of the two answers `has_blob` used to conflate, and the contract's
    /// §9 froze both under names that say which they mean. This one is about
    /// the *object*: a second Workspace that becomes associated with the same
    /// picture needs no transfer at all, and asking the Cloud for bytes this
    /// device already holds is the wasted gigabyte nobody sees.
    ///
    /// One lookup, because there is one object store: `<root>/blobs/objects/`
    /// holds each blob exactly once whatever it is referenced from.
    ///
    /// It deliberately does **not** promote a legacy per-Workspace file: it
    /// has no Workspace to ask about, and hunting for candidate files across
    /// the whole store is how one Profile's bytes would be offered to another.
    /// The Workspace-scoped [`workspace_has_blob`](Self::workspace_has_blob)
    /// is the one that can promote, and it is what a transfer asks.
    ///
    /// **It is never authority.** Holding the bytes is not permission to serve
    /// them: every read still requires the Workspace association.
    pub fn object_present(&mut self, hash: &BlobHash) -> Result<bool> {
        self.backend.objects()?.has(hash)
    }

    /// The Workspace is associated **and** the bytes are present.
    ///
    /// The other answer, and **no caller infers it from
    /// [`object_present`](Self::object_present)**: a `blobs` row without bytes
    /// is a Workspace that knows about a picture it does not hold, and bytes
    /// without a row are bytes this Workspace may not read.
    pub fn workspace_has_blob(&mut self, workspace: WorkspaceId, hash: &BlobHash) -> Result<bool> {
        self.ensure_open(workspace)?;
        if self.open[&workspace].repo.blob(hash)?.is_none() {
            return Ok(false);
        }
        // Answering "no" for bytes still sitting in the legacy directory would
        // send a transfer to fetch a picture this device already has, so the
        // question is also the trigger for promoting it.
        self.ensure_object(workspace, hash)
    }

    /// A reader over the local object, for a caller that streams it.
    ///
    /// **Uploads stream**: [`get_blob`](Self::get_blob) returns a `Vec`, and a
    /// 2 GiB `Vec` on the Store dispatcher is the same mistake the blob
    /// announcement seam avoids. The handle outlives this borrow, so the Store
    /// lock is released before the first chunk is read.
    ///
    /// **Association first, then bytes, and verification is the caller's.**
    /// The association is checked here because a hash is never authority;
    /// re-hashing is not, because a chunked read that verified per chunk would
    /// be O(size²) — a streaming caller calls
    /// [`verify_blob`](Self::verify_blob) once, before it starts.
    pub fn open_blob(
        &mut self,
        workspace: WorkspaceId,
        hash: &BlobHash,
    ) -> Result<Box<dyn std::io::Read + Send>> {
        self.ensure_open(workspace)?;
        self.require_association(workspace, hash)?;
        if !self.ensure_object(workspace, hash)? {
            return Err(Error::bare(Code::BlobNotFound));
        }
        self.backend.objects()?.open(hash)
    }

    /// Drop this device's copy of a blob, keeping the document's reference.
    ///
    /// **For reclaiming space, and only safe when the bytes live somewhere
    /// else.** The Store cannot know that — whether Cloud holds a durable
    /// copy is Cloud's answer — so this refuses nothing on those grounds and
    /// the caller carries the whole of that judgement. `koru-blob-sync` is
    /// where it is made, against the control plane rather than against a
    /// coverage reading that is knowingly a moment old.
    ///
    /// **The association stays.** The document still references the blob, so
    /// the next read answers `BlobNotFound` and the transfer engine fetches it
    /// again — the same state as a device that has never held these bytes,
    /// which is precisely the state a person reclaiming space is asking for.
    /// Removing the association instead would unlink the picture from the
    /// document, which is a different and much worse operation.
    pub fn release_blob(&mut self, workspace: WorkspaceId, hash: &BlobHash) -> Result<u64> {
        self.ensure_open(workspace)?;
        // Association first: releasing bytes this Workspace never referenced
        // would be reaching into another Workspace's storage through a hash
        // somebody happened to know.
        self.require_association(workspace, hash)?;
        let size = self
            .blobs(workspace)?
            .into_iter()
            .find(|blob| blob.hash == *hash)
            .map(|blob| blob.size)
            .unwrap_or_default();
        if !self.ensure_object(workspace, hash)? {
            // Nothing held locally: the caller's goal is already true, and
            // reporting bytes freed would be inventing a number.
            return Ok(0);
        }
        self.backend.objects()?.release(hash)?;
        Ok(size)
    }

    /// Re-hash a stored blob and quarantine it if the bytes have rotted, without
    /// handing them back.
    ///
    /// This is `get_blob`'s integrity half on its own, for a reader that walks a
    /// blob in ranges. Verifying is O(size), so a chunked read that verified per
    /// chunk would be O(size²) — the caller pays this once, at the start of a
    /// transfer, and reads ranges after it.
    pub fn verify_blob(&mut self, workspace: WorkspaceId, hash: &BlobHash) -> Result<()> {
        self.ensure_open(workspace)?;
        self.require_association(workspace, hash)?;
        if !self.ensure_object(workspace, hash)? {
            return Err(Error::bare(Code::BlobNotFound));
        }
        self.verify_object(workspace, hash)
    }

    /// Part of a blob, unverified.
    ///
    /// Deliberately not a verifying read: whoever walks a blob in ranges calls
    /// `verify_blob` once first, and a peer receiving these bytes verifies the
    /// whole payload against its hash before publishing it. Nothing here is
    /// trusted on the strength of having been read.
    pub fn read_blob_range(
        &mut self,
        workspace: WorkspaceId,
        hash: &BlobHash,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>> {
        self.ensure_open(workspace)?;
        // Per chunk, because the object is shared and the association is the
        // only thing that says this peer's Workspace may have it. Both checks
        // are one indexed row and one `exists`; the alternative is a window in
        // which a revoked association keeps streaming.
        self.require_association(workspace, hash)?;
        if !self.ensure_object(workspace, hash)? {
            return Err(Error::bare(Code::BlobNotFound));
        }
        self.backend.objects()?.read_range(hash, offset, len)
    }

    pub fn blob(&mut self, workspace: WorkspaceId, hash: &BlobHash) -> Result<Blob> {
        self.ensure_open(workspace)?;
        self.open[&workspace]
            .repo
            .blob(hash)?
            .ok_or_else(|| Error::bare(Code::BlobNotFound))
    }

    pub fn blobs(&mut self, workspace: WorkspaceId) -> Result<Vec<Blob>> {
        self.ensure_open(workspace)?;
        self.open[&workspace].repo.blobs()
    }

    /// The device that first stored a blob (storage-form id), wherever the
    /// record lives: the `blobs` row's attributes where the bytes are held,
    /// else the provenance a peer's advertisement left behind. Provenance for
    /// parking labels and holder hints — never routing authority.
    pub fn blob_origin(
        &mut self,
        workspace: WorkspaceId,
        hash: &BlobHash,
    ) -> Result<Option<String>> {
        self.ensure_open(workspace)?;
        let repo = &self.open[&workspace].repo;
        if let Some(blob) = repo.blob(hash)?
            && let Some(origin) = blob.text_attribute(crate::model::blob_attr::ORIGIN_DEVICE)
        {
            return Ok(Some(origin.to_owned()));
        }
        repo.advertised_blob_origin(hash)
    }

    /// Record the origins a peer's inventory advertised for blobs we may never
    /// have held. This is what lets a freshly booted device name the origin of
    /// a missing image while every holder is offline: the advertisement used to
    /// reach only the replication runtime's per-session directory, which a
    /// restart discards.
    ///
    /// First record wins and blobs without an origin attribute are skipped.
    /// Returns how many *new* provenance rows landed — a caller can stay silent
    /// on zero, because a re-advertisement is not news.
    pub fn record_blob_origins(
        &mut self,
        workspace: WorkspaceId,
        blobs: &[Blob],
        at: Timestamp,
    ) -> Result<usize> {
        self.ensure_open(workspace)?;
        let mut recorded = 0;
        for blob in blobs {
            let Some(origin) = blob.text_attribute(crate::model::blob_attr::ORIGIN_DEVICE) else {
                continue;
            };
            let repo = &mut self.open.get_mut(&workspace).expect("just opened").repo;
            // A held blob's own attributes are the record; the advertisement
            // table exists for hashes with no local row at all.
            if repo.blob(&blob.hash)?.is_none()
                && repo.advertised_blob_origin(&blob.hash)?.is_none()
            {
                repo.record_blob_origin(&blob.hash, origin, at)?;
                recorded += 1;
            }
        }
        Ok(recorded)
    }

    // -------------------------------------------------------- export/import

    /// Everything in a workspace, as one value.
    ///
    /// A **snapshot** per document rather than its journal: a bundle is for
    /// moving a workspace, not auditing it, and a snapshot already carries the
    /// history Loro needs to converge.
    pub fn export_workspace(&mut self, workspace: WorkspaceId, at: Timestamp) -> Result<Bundle> {
        self.ensure_open(workspace)?;
        let record = self.workspace(workspace)?;
        let lineage = self.catalog.lineage(workspace)?;

        let states = self.open[&workspace].repo.documents()?;
        let mut documents = Vec::with_capacity(states.len());
        let mut snapshots = Vec::with_capacity(states.len());
        for state in states {
            // Through `snapshot`, so a document that has to be recovered is
            // recovered rather than exported half-read.
            let bytes = self.snapshot(workspace, state.id)?;
            documents.push(BundleDocument {
                id: state.id,
                kind: state.kind,
                checksum: crate::lifecycle::checksum(&bytes),
            });
            snapshots.push((state.id, bytes));
        }

        let mut history_versions = Vec::new();
        let mut history_entries = Vec::new();
        let mut history_snapshots = Vec::new();
        let mut seen_versions = std::collections::HashSet::<DocumentVersionId>::new();
        for document in &documents {
            let mut before = None;
            loop {
                let page = self.open[&workspace]
                    .repo
                    .history_entries(document.id, before, 200)?;
                if page.is_empty() {
                    break;
                }
                for entry in &page {
                    if seen_versions.insert(entry.version) {
                        let version = self.open[&workspace]
                            .repo
                            .document_version(entry.version)?
                            .ok_or_else(|| Error::bare(Code::VersionUnavailable))?;
                        history_snapshots.push((version.id, version.snapshot.clone()));
                        history_versions.push(BundleHistoryVersion {
                            id: version.id,
                            document: version.document,
                            version_vector: version.version_vector,
                            frontiers: version.frontiers,
                            checksum: version.checksum,
                            captured_at: version.captured_at,
                        });
                    }
                    history_entries.push(BundleHistoryEntry {
                        id: entry.id,
                        document: entry.document,
                        version: entry.version,
                        reason: entry.reason,
                        recorded_at: entry.recorded_at,
                        label: entry.label.clone(),
                        message: entry.message.clone(),
                        boundary: entry.boundary.clone(),
                        pinned: entry.pinned,
                    });
                }
                if page.len() < 200 {
                    break;
                }
                let last = page.last().expect("non-empty history page");
                before = Some((last.recorded_at, last.id));
            }
        }

        // The rows are this Workspace's own, so the association is settled and
        // what is in question is only the bytes — which on a store written by
        // an older build may still be in the legacy per-Workspace directory.
        // An export is therefore one of the places the lazy migration runs.
        let recorded = self.blobs(workspace)?;
        let mut blobs = Vec::with_capacity(recorded.len());
        for blob in &recorded {
            let hash = blob.hash.clone();
            // A blob the metadata knows about but the disk does not is not a
            // reason to refuse the export — the documents are the valuable part
            // and a missing picture is visible. It is recorded instead.
            if self.ensure_object(workspace, &hash)?
                && let Ok(bytes) = self.backend.objects()?.get(&hash)
            {
                blobs.push((hash, bytes));
                continue;
            }
            self.recoveries.push(format!(
                "workspace {workspace}: blob {hash} was missing at export"
            ));
        }
        let present: Vec<Blob> = recorded
            .into_iter()
            .filter(|b| blobs.iter().any(|(hash, _)| *hash == b.hash))
            .collect();

        Ok(Bundle {
            manifest: BundleManifest {
                format_version: crate::version::BUNDLE_FORMAT_VERSION,
                workspace,
                name: record.name,
                created_at: record.created_at,
                exported_at: at,
                lineage,
                documents,
                history_versions,
                history_entries,
                blobs: present,
            },
            snapshots,
            history_snapshots,
            blobs,
        })
    }

    /// Bring a bundle in, as a replica or as an independent copy.
    ///
    /// See [`ImportMode`]. The whole bundle is validated **before** anything is
    /// written: importing half a workspace and then finding the rest unreadable
    /// leaves something that looks present and is not.
    pub fn import_workspace(
        &mut self,
        bundle: &Bundle,
        mode: ImportMode,
        owner: ProfileId,
        at: Timestamp,
    ) -> Result<WorkspaceId> {
        bundle.validate()?;
        if self.catalog.profile(owner)?.is_none() {
            return Err(Error::bare(Code::ProfileNotFound));
        }

        let origin = bundle.manifest.workspace;
        let workspace = match mode {
            ImportMode::Restore => {
                if self.catalog.workspace(origin)?.is_some() {
                    // Restoring over a workspace already here would merge two
                    // histories under one id with no record of it. Refusing is
                    // the only answer that cannot lose anything.
                    return Err(Error::new(
                        Code::WorkspaceExists,
                        "this workspace is already in the store; import it as a duplicate instead",
                    ));
                }
                origin
            }
            ImportMode::Duplicate => WorkspaceId::new(),
        };

        // Document ids are preserved on restore and re-minted on duplicate, so
        // a duplicate cannot collide with its origin inside one store.
        let mut remap: HashMap<DocumentId, DocumentId> = HashMap::new();
        for document in &bundle.manifest.documents {
            let id = match mode {
                ImportMode::Restore => document.id,
                ImportMode::Duplicate => DocumentId::new(),
            };
            remap.insert(document.id, id);
        }

        // A manifest is itself a Loro document, and its rows contain the
        // document ids. Importing its opaque source bytes after reminting the
        // storage rows leaves a workspace whose catalog and manifest disagree.
        // Rebuild just that one document from its public entries.
        let duplicated_manifest = if mode == ImportMode::Duplicate {
            Some(self.duplicate_manifest_snapshot(bundle, workspace, &remap)?)
        } else {
            None
        };

        let record = Workspace {
            id: workspace,
            owner,
            name: bundle.manifest.name.clone(),
            created_at: bundle.manifest.created_at,
            last_opened_at: None,
            // An imported bundle brings its content, not the switcher entry the
            // person had built around it. Defaults rather than guesses.
            icon: None,
            color: None,
            pinned: false,
            archived: false,
            deleted_at: None,
            position: self.next_workspace_position(owner)?,
        };
        self.catalog.put_workspace(&record)?;

        let mut repo = self.backend.open_workspace(workspace)?;
        for document in &bundle.manifest.documents {
            let id = remap[&document.id];
            let bytes = match &duplicated_manifest {
                Some((source, bytes)) if *source == document.id => bytes.as_slice(),
                _ => bundle
                    .snapshot_of(document.id)
                    .ok_or_else(|| Error::new(Code::InvalidRequest, "a snapshot went missing"))?,
            };

            repo.create_document(id, document.kind, at)?;
            // The snapshot becomes the document's whole history, written as one
            // update from nothing. A delta would name ancestors that do not
            // exist here, and Loro would park it as pending — applying nothing
            // and reporting nothing.
            repo.commit_updates(&crate::ports::UpdateBatch {
                document: id,
                updates: vec![bytes.to_vec()],
                version_vector: Vec::new(),
                frontiers: Vec::new(),
                received_at: at,
                origins: vec![crate::ports::UpdateOrigin::Store],
            })?;
            self.catalog.record_location(id, workspace)?;
        }

        // History is imported after the live documents exist. `capture_history`
        // intentionally re-mints storage ids while deduplicating identical
        // frontiers: a duplicated workspace must not share external identities
        // with its origin, and callers address the imported entries from the
        // returned timeline rather than carrying stale ids across stores.
        for entry in bundle.manifest.history_entries.iter().rev() {
            let source = bundle
                .manifest
                .history_versions
                .iter()
                .find(|version| version.id == entry.version)
                .ok_or_else(|| {
                    Error::new(Code::InvalidRequest, "a history version went missing")
                })?;
            let snapshot = bundle.history_snapshot_of(source.id).ok_or_else(|| {
                Error::new(Code::InvalidRequest, "a history snapshot went missing")
            })?;
            let document = remap[&entry.document];
            repo.capture_history(&HistoryCapture {
                document,
                snapshot,
                version_vector: &source.version_vector,
                frontiers: &source.frontiers,
                checksum: &source.checksum,
                reason: entry.reason,
                recorded_at: entry.recorded_at,
                label: entry.label.as_deref(),
                message: entry.message.as_deref(),
                boundary: entry.boundary.as_deref(),
            })?;
        }

        // **Objects first, then associations.** The bytes are installed into
        // the one object store and only then does the Workspace gain its row,
        // so a restore interrupted between the two leaves an object nothing
        // references — which this rung keeps — rather than a row naming bytes
        // that are not there.
        let blobs = self.backend.objects()?;
        for (hash, bytes) in &bundle.blobs {
            let written = blobs.put(bytes)?;
            if written != *hash {
                return Err(Error::new(
                    Code::BlobChecksumMismatch,
                    "a blob in the bundle does not hash to the name it was filed under",
                ));
            }
            if let Some(meta) = bundle.manifest.blobs.iter().find(|b| b.hash == *hash) {
                repo.put_blob(meta)?;
            }
        }

        if mode == ImportMode::Duplicate {
            self.catalog.record_lineage(&Lineage {
                workspace,
                derived_from: origin,
                kind: crate::model::LineageKind::Duplicate,
                at,
            })?;
        }

        // Opened afresh, so the manifest is read back from what was actually
        // written rather than assumed from the bundle.
        drop(repo);
        self.ensure_open(workspace)?;
        let entries = self.open[&workspace].manifest.entries();
        self.open
            .get_mut(&workspace)
            .expect("just opened")
            .repo
            .reindex(&entries)?;
        Ok(workspace)
    }

    fn duplicate_manifest_snapshot(
        &self,
        bundle: &Bundle,
        workspace: WorkspaceId,
        remap: &HashMap<DocumentId, DocumentId>,
    ) -> Result<(DocumentId, Vec<u8>)> {
        let source_id = bundle.manifest_document().ok_or_else(|| {
            Error::new(Code::InvalidRequest, "the bundle has no manifest document")
        })?;
        let source_bytes = bundle
            .snapshot_of(source_id)
            .ok_or_else(|| Error::new(Code::InvalidRequest, "the manifest snapshot is missing"))?;
        let source = Manifest::from_snapshot(source_bytes)?;
        let mut target = Manifest::create(
            workspace,
            source.name().as_deref().unwrap_or(&bundle.manifest.name),
            source.created_at().unwrap_or(bundle.manifest.created_at),
        )?;
        for entry in source.entries() {
            let id = *remap.get(&entry.id).ok_or_else(|| {
                Error::new(
                    Code::InvalidRequest,
                    "the manifest names an unknown document",
                )
            })?;
            let parent = entry
                .parent
                .map(|parent| {
                    remap.get(&parent).copied().ok_or_else(|| {
                        Error::new(Code::InvalidRequest, "the manifest names an unknown parent")
                    })
                })
                .transpose()?;
            target.add_document_at_with_mime(
                id,
                &entry.title,
                &entry.kind,
                entry.mime.as_deref(),
                parent,
                None,
            )?;
            if let Some(at) = entry.trashed_at {
                target.trash_document(id, at, entry.trashed_by.as_deref())?;
            }
        }
        Ok((source_id, target.export_snapshot()?))
    }

    // ------------------------------------------------------------ diagnosis

    /// Bytes a workspace's database occupies. Metadata, never content.
    pub fn database_bytes(&mut self, workspace: WorkspaceId) -> Result<u64> {
        self.ensure_open(workspace)?;
        self.open[&workspace].repo.size_bytes()
    }

    /// Where one document has got to, for the debug routes.
    ///
    /// Everything here is a number, a version or an instant. Nothing in it is
    /// text somebody wrote — these values end up in bug reports.
    /// When this document last changed on disk, and which replica path
    /// supplied that change.
    ///
    /// **Two indexed reads and no payload.** The row's `updated_at` is the
    /// commit's own moment; the origin is the newest journal row's, which is
    /// `None` once a checkpoint has swallowed every update — a document that
    /// has not moved since its last checkpoint has a change time and no
    /// provenance, and saying so is more honest than naming the store.
    pub fn document_provenance(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<DocumentProvenance> {
        self.ensure_document(workspace, document)?;
        let repo = &self.open[&workspace].repo;
        let state = repo
            .document(document)?
            .ok_or_else(|| Error::bare(Code::DocumentNotFound))?;
        Ok(DocumentProvenance {
            updated_at: state.updated_at,
            origin: repo
                .latest_update_origin(document)?
                .map(|(_, origin)| origin),
        })
    }

    pub fn document_report(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<DocumentReport> {
        self.ensure_document(workspace, document)?;
        let open = &self.open[&workspace];
        let state = open
            .repo
            .document(document)?
            .ok_or_else(|| Error::bare(Code::DocumentNotFound))?;
        let checkpoints = open.repo.checkpoints(document)?;
        let pending = open
            .repo
            .updates_after(document, state.latest_checkpoint_seq)?
            .len() as u64;

        // Read from the live replica rather than from the stored row: the row
        // is where the document was at its last commit, and this is asked in
        // order to find out whether those two agree.
        let (version_vector, frontiers) = open
            .position(document)
            .unwrap_or((state.version_vector.clone(), state.frontiers.clone()));

        Ok(DocumentReport {
            kind: state.kind,
            version_vector,
            frontiers,
            latest_checkpoint: state.latest_checkpoint_seq,
            checkpoints: checkpoints.len(),
            pending_updates: pending,
            schema_version: open
                .is_manifest(document)
                .then(|| open.manifest.schema_version())
                .flatten(),
            trashed_at: open
                .repo
                .entry(document)?
                .and_then(|entry| entry.trashed_at),
            updated_at: state.updated_at,
        })
    }

    // ------------------------------------------------------------ durability

    /// Commit whatever is queued for a document.
    ///
    /// Returns the sequence now durable, or `None` if there was nothing to
    /// write. **A caller may acknowledge only after this returns `Ok`.**
    pub fn flush(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        at: Timestamp,
    ) -> Result<Option<u64>> {
        let Some(open) = self.open.get_mut(&workspace) else {
            return Err(Error::bare(Code::WorkspaceNotFound));
        };
        let Some((version_vector, frontiers)) = open.position(document) else {
            return Ok(None);
        };
        let Some(window) = open.windows.get_mut(&document) else {
            return Ok(None);
        };
        let Some(committed) = window.commit(version_vector, frontiers, at, |batch| {
            // The fsync. Nothing may be acknowledged before this returns.
            // Failure retains the original window even when Loro already knows
            // the frame, so retry cannot acknowledge memory-only state.
            let span = crate::stats::Span::start(crate::stats::names::COMMIT);
            let result = open.repo.commit_updates(batch);
            span.stop();
            result
        })?
        else {
            return Ok(None);
        };
        let head = committed.head;
        let batch = committed.batch;
        // The remaining committed-window resources outlive this batch and
        // every post-commit read/notification below.

        let is_manifest = open.is_manifest(document);

        // A checkpoint is a **separate** transaction, deliberately: it is an
        // optimisation, and it must never be able to fail the commit that
        // carries someone's work. Its failure — and the failure of every
        // bookkeeping read deciding whether one is due — is therefore
        // ignored: the journal alone remains a complete document.
        //
        // **The policy accumulates since the last checkpoint, not per batch.**
        // The thresholds bound the journal a reopen has to replay, and a
        // document edited in small steps never crossed them inside any single
        // 5ms group-commit window — which is how a manifest reached 6,466
        // journal updates without one checkpoint ever being taken.
        let since_seq = open
            .repo
            .document(document)
            .ok()
            .flatten()
            .and_then(|state| state.latest_checkpoint_seq);
        let count = head.saturating_sub(since_seq.unwrap_or(0));
        let bytes = open
            .repo
            .journal_since(document, since_seq)
            .map_or(0, |since| since.bytes);
        if self.checkpoints.due(count, bytes) {
            take_checkpoint(workspace, open, document, head, at, "flush");
        }

        // The projection is rebuilt whenever the manifest changes, so a listing
        // never drifts from the document it describes.
        if is_manifest {
            let entries = open.manifest.entries();
            let _ = open.repo.reindex(&entries);
            // A permanent delete is a manifest commit, local or synced; the
            // private rows of what it removed go with it.
            document_private::prune_private_state(workspace, &mut open.repo, &entries);
        }

        let manifest_workspace = is_manifest.then(|| manifest_workspace_projection(&open.manifest));

        // **After the commit, and after the listing it may have changed** — for
        // the same reason `Ack` and the room broadcast come after the commit:
        // derived state built from bytes a power cut could still erase describes
        // something that never happened, and a consumer that joins against the
        // listing must not be woken while the listing is a version behind.
        //
        // The early returns above are why this is not simply "flush was called":
        // `Ok(None)` means there was nothing to write, and recording that would
        // wake a worker on every idle turn.
        self.committed.insert(
            (workspace, document),
            Committed {
                workspace,
                document,
                seq: head,
                version_vector: batch.version_vector,
                is_manifest,
            },
        );
        self.sync_dirty
            .entry(workspace)
            .or_default()
            .insert(document);
        // Refresh membership while the workspace is necessarily still open.
        // Applying a manifest's lifecycle projection may close a newly deleted
        // workspace, so doing this afterwards would turn an ordinary remote
        // delete into an `expect("workspace is open")` panic and poison the
        // process-wide Store lock.
        if is_manifest {
            let _ = self.reconcile_document_locations(workspace);
        }
        // Like the document index above, the catalog name is a projection of
        // durable manifest bytes. A projection failure cannot turn a committed
        // update into an unacknowledged one; opening the workspace reconciles it
        // again, so a crash or transient catalog failure repairs naturally.
        // This comes after location repair because a delete projection is
        // allowed to evict `workspace` from `self.open`.
        if let Some(projection) = manifest_workspace {
            let _ = self.reconcile_workspace_projection(workspace, projection);
        }
        Ok(Some(head))
    }

    /// Take the documents committed since this was last called.
    ///
    /// Derived state is drained rather than computed in a callback: it must
    /// never run inside the lock a commit holds. The optional commit signal
    /// only wakes a host; it cannot consume these records. Draining lets a
    /// caller decide *when* — after a request, after a protocol message, on a
    /// tick — without core having an opinion about threads.
    ///
    /// A build that never calls this is not leaking: the queue is keyed by
    /// document, so it is bounded by how many documents have been written.
    pub fn take_committed(&mut self) -> Vec<Committed> {
        self.committed.take()
    }

    /// Install the host's content-free wake. Derived work still runs outside
    /// the Store lock; the projection feed is neither drained nor duplicated.
    pub fn set_commit_signal(
        &mut self,
        signal: std::sync::Arc<dyn crate::commit_signal::CommitSignal>,
    ) {
        self.committed.signal = Some(signal);
    }

    /// Take coalesced reminder-root wakes after their profile snapshot commit.
    pub fn take_reminder_committed(&mut self) -> Vec<ReminderCommitted> {
        self.reminder_committed
            .drain()
            .map(|(_, event)| event)
            .collect()
    }

    /// Take coalesced personal-dictionary wakes after their private-unit commit.
    pub fn take_dictionary_committed(&mut self) -> Vec<DictionaryCommitted> {
        self.dictionary_committed
            .drain()
            .map(|(_, event)| event)
            .collect()
    }

    /// Take coalesced identity wakes after an imported identity unit commit.
    pub fn take_identity_committed(&mut self) -> Vec<IdentityCommitted> {
        self.identity_committed
            .drain()
            .map(|(_, event)| event)
            .collect()
    }

    /// Where every document in a workspace has got to, **without opening any of
    /// them**.
    ///
    /// The durable position out of the journal, which is a row in SQLite — not
    /// [`version`](Self::version), which loads the document's Loro history to
    /// answer the same question. Derived state comparing revisions over a
    /// workspace would otherwise open every document to decide it had nothing to
    /// do, and nothing evicts: the store's start-up would stop costing the same
    /// on a store holding one workspace and one holding a thousand.
    ///
    /// This opens the workspace's *database*, which is the smallest thing that
    /// can answer at all.
    pub fn document_positions(
        &mut self,
        workspace: WorkspaceId,
    ) -> Result<Vec<(DocumentId, Vec<u8>)>> {
        self.ensure_open(workspace)?;
        let open = self.open.get(&workspace).expect("just opened");
        Ok(open
            .repo
            .documents()?
            .into_iter()
            .filter(|d| d.kind != DocumentKind::Manifest)
            .map(|d| (d.id, d.version_vector))
            .collect())
    }

    /// When the store last **durably wrote** one document, from the same journal
    /// row [`document_positions`](Self::document_positions) reads.
    ///
    /// This is the store's own per-document stamp, and the point of it is that
    /// it lives in `workspace.db` rather than in anything derived: it survives a
    /// rebuild of every projection, index and cache above it. Three things move
    /// it and nothing else does — the document's creation, a committed batch of
    /// its updates, and a checkpoint of it. Note the shape of that list:
    ///
    /// - It is **this document's** journal, so a manifest commit — a rename, an
    ///   icon, a bin, a restore — moves the *manifest's* row and never this one.
    ///   Listing metadata is not content and does not stamp content.
    /// - It is **when the store wrote it**, not when a person typed it. An
    ///   update authored offline stamps the moment it was accepted here. That is
    ///   the same class of answer as `koru-search`'s `observed_modified_at_ms`,
    ///   and it is described to clients that way.
    /// - A **checkpoint** of an over-long journal is taken at load as well as
    ///   after a commit, so a document that was edited and never checkpointed
    ///   can have this stamp moved forward to the load. It is bounded by having
    ///   had unflushed edits in the first place, and a reader that gates on
    ///   content — as `Projections::replace` does — never sees it.
    ///
    /// `None` means the workspace does not hold that document.
    pub fn document_updated_at(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<Option<Timestamp>> {
        self.ensure_open(workspace)?;
        let open = self.open.get(&workspace).expect("just opened");
        Ok(open.repo.document(document)?.map(|state| state.updated_at))
    }

    /// A disposable hierarchical anti-entropy root for one workspace.
    ///
    /// Leaves are derived solely from authoritative durable version vectors.
    /// The first call after startup compares every persisted leaf with the
    /// repository, repairing a missed cache update after a crash. Thereafter a
    /// durable commit invalidates only its document leaf, so changing one
    /// document never opens or re-hashes the rest of the workspace.
    pub fn workspace_sync_digest(&mut self, workspace: WorkspaceId) -> Result<Vec<u8>> {
        self.ensure_open_including_deleted(workspace)?;
        let deleted = self
            .workspace_including_deleted(workspace)?
            .deleted_at
            .is_some();
        let manifest_id = self.open[&workspace].manifest_id;

        if self.sync_verified.get(&workspace) != Some(&deleted) {
            let states = self.open[&workspace]
                .repo
                .documents()?
                .into_iter()
                .filter(|state| !deleted || state.id == manifest_id)
                .collect::<Vec<_>>();
            let cached = self.catalog.sync_document_digests(workspace)?;
            let cached_by_id = cached
                .iter()
                .map(|leaf| (leaf.document, leaf))
                .collect::<HashMap<_, _>>();
            let membership_changed = cached.len() != states.len()
                || states
                    .iter()
                    .any(|state| !cached_by_id.contains_key(&state.id));
            if membership_changed {
                let leaves = states
                    .iter()
                    .map(|state| sync_document_digest(workspace, state))
                    .collect::<Vec<_>>();
                self.catalog
                    .replace_sync_document_digests(workspace, &leaves)?;
            } else {
                for state in &states {
                    // Cache format upgrades are repaired just like missed commits.
                    if cached_by_id[&state.id].source_revision != state.version_vector
                        || cached_by_id[&state.id].digest
                            != sync_document_digest(workspace, state).digest
                    {
                        self.catalog
                            .put_sync_document_digest(&sync_document_digest(workspace, state))?;
                    }
                }
            }
            self.sync_dirty.remove(&workspace);
            self.sync_verified.insert(workspace, deleted);
        } else if let Some(dirty) = self.sync_dirty.remove(&workspace) {
            for document in dirty {
                if deleted && document != manifest_id {
                    continue;
                }
                let Some(state) = self.open[&workspace].repo.document(document)? else {
                    // Membership changed unexpectedly. Re-verify the complete
                    // authoritative set on the next request rather than
                    // guessing which cached leaf should disappear.
                    self.sync_verified.remove(&workspace);
                    return self.workspace_sync_digest(workspace);
                };
                self.catalog
                    .put_sync_document_digest(&sync_document_digest(workspace, &state))?;
            }
        }

        let leaves = self.catalog.sync_document_digests(workspace)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"koru-sync-workspace-v1\0");
        hasher.update(workspace.as_uuid().as_bytes());
        for leaf in leaves {
            hasher.update(leaf.document.as_uuid().as_bytes());
            hasher.update(&leaf.digest);
        }
        Ok(hasher.finalize().as_bytes().to_vec())
    }

    /// A read-only copy of a document, cheap enough to take under a lock.
    ///
    /// **The seam derived state reads through.** `LoroDoc::fork` is a structural
    /// share rather than a copy, so the caller takes the store's lock only long
    /// enough to fork and does its walking outside — which matters because
    /// `AppState` holds one lock for the whole store, and a walk behind it would
    /// block every other workspace's commit.
    ///
    /// **Opens the document if it is not already open**, which is what a rebuild
    /// needs and what a rebuild pays for: nothing evicts, so a sweep over every
    /// document leaves them all resident. Batch accordingly.
    ///
    /// The manifest is refused. It is a document to the store, but its shape is
    /// the store's own and it is already mirrored through
    /// [`DocumentIndex`](crate::ports::DocumentIndex) — a caller wanting the
    /// listing asks [`documents`](Self::documents).
    pub fn fork_document(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<loro::LoroDoc> {
        self.ensure_document(workspace, document)?;
        let open = self.open.get(&workspace).expect("just opened");
        if open.is_manifest(document) {
            return Err(Error::new(
                Code::InvalidRequest,
                "the manifest is not a document to project; ask for the listing instead",
            ));
        }
        let live = open
            .documents
            .get(&document)
            .ok_or_else(|| Error::bare(Code::DocumentNotFound))?;
        Ok(live.doc().fork())
    }

    /// Every document whose commit window has come due.
    pub fn due(&self, now: Timestamp) -> Vec<(WorkspaceId, DocumentId)> {
        let mut out = Vec::new();
        for (ws, open) in &self.open {
            for (doc, window) in &open.windows {
                if window.due(now) {
                    out.push((*ws, *doc));
                }
            }
        }
        out
    }

    /// When the soonest window is due, for a loop deciding how long to sleep.
    pub fn next_due(&self) -> Option<Timestamp> {
        self.open
            .values()
            .flat_map(|o| o.windows.values())
            .filter_map(|w| w.due_at())
            .min()
    }

    // ------------------------------------------------------------- internals

    fn ensure_profile_manifest(&mut self, id: ProfileId) -> Result<()> {
        self.require_profile(id)?;
        if self.open_profiles.contains_key(&id) {
            return Ok(());
        }
        let state = self
            .catalog
            .profile_unit(ProfileUnitRef::Identity { profile: id })?
            .ok_or_else(|| {
                Error::new(
                    Code::VersionUnavailable,
                    "Profile identity bytes have not arrived",
                )
            })?;
        let manifest = IdentityManifest::from_snapshot(&state.snapshot)?;
        if manifest.profile()?.id != id {
            return Err(Error::bare(Code::ProfileUnitScopeMismatch));
        }
        if !manifest.is_read_only() {
            manifest.set_peer_id(self.peer_id)?;
        }
        self.open_profiles.insert(id, manifest);
        Ok(())
    }

    fn ensure_open(&mut self, workspace: WorkspaceId) -> Result<()> {
        self.open_workspace_state(workspace, false)
    }

    fn ensure_open_including_deleted(&mut self, workspace: WorkspaceId) -> Result<()> {
        self.open_workspace_state(workspace, true)
    }

    fn open_workspace_state(
        &mut self,
        workspace: WorkspaceId,
        include_deleted: bool,
    ) -> Result<()> {
        if self.open.contains_key(&workspace) {
            return Ok(());
        }
        let span = crate::stats::Span::start(crate::stats::names::WORKSPACE_OPEN);
        let row = self.catalog.workspace(workspace)?;
        if row
            .as_ref()
            .is_none_or(|row| row.deleted_at.is_some() && !include_deleted)
        {
            return Err(Error::bare(Code::WorkspaceNotFound));
        }
        let repo = self.backend.open_workspace(workspace)?;

        // Find the manifest. Document locations are refreshed only after its
        // authoritative membership has been decoded below: physical rows can
        // intentionally outlive a remote cross-workspace move and must not
        // reclaim ownership merely because their bytes remain recoverable.
        let all = repo.documents()?;
        let manifest_id = all
            .into_iter()
            .find(|d| d.kind == DocumentKind::Manifest)
            .map(|d| d.id)
            .ok_or_else(|| {
                Error::new(
                    Code::CorruptState,
                    "this workspace has no manifest document",
                )
            })?;

        let opened = open_document(&repo, manifest_id)?;
        // A manifest recovered from an older checkpoint may be missing
        // entries added since; pruning private rows against it would delete
        // folds for documents that still exist.
        let manifest_sound = !matches!(opened, Opened::Recovered(..));
        if let Opened::Recovered(_, recovery) = &opened {
            self.recoveries.push(format!(
                "workspace {workspace}: manifest — {}",
                recovery.problem
            ));
        }
        let live = opened.into_value();
        let manifest = Manifest::from_snapshot(&live.export_snapshot()?)?;
        manifest.set_peer_id(self.peer_id)?;
        let manifest_workspace = manifest_workspace_projection(&manifest);

        // The manifest is kept as a `Manifest` and **not** also as a
        // `LiveDocument`: two copies would be two `LoroDoc`s of one document.
        self.open.insert(
            workspace,
            Open {
                repo,
                manifest,
                documents: HashMap::new(),
                windows: HashMap::new(),
                manifest_id,
            },
        );
        self.reconcile_workspace_projection(workspace, manifest_workspace)?;
        self.reconcile_document_locations(workspace)?;
        // The entries index is the same disposable kind of projection as the
        // location table, but only manifest commits rebuilt it — so a stale
        // or empty index outlived any crash or old-version bug while sync,
        // which compares version vectors and never the listing, reported
        // everything fine. Rebuild it from the manifest just decoded above:
        // open is the repair point, and a workspace that cannot reindex
        // should say so here rather than list nothing forever. A workspace
        // the projection just evicted as deleted is gone from `open` and
        // lists nothing anyway.
        if let Some(open) = self.open.get_mut(&workspace) {
            let entries = open.manifest.entries();
            open.repo.reindex(&entries)?;
            // Finish a private-state cleanup a crash interrupted.
            if manifest_sound {
                document_private::prune_private_state(workspace, &mut open.repo, &entries);
            }
            // The manifest's journal was just replayed in full to open it;
            // if that replay was overdue for a checkpoint, take it now so
            // it was the last one of its size.
            let policy = self.checkpoints;
            let manifest_id = open.manifest_id;
            checkpoint_if_overdue(&policy, workspace, open, manifest_id);
        }
        span.stop();
        Ok(())
    }

    /// Repair the derived global document-location index from one workspace's
    /// authoritative manifest without deleting recoverable orphan bytes.
    fn reconcile_document_locations(&mut self, workspace: WorkspaceId) -> Result<()> {
        let (physical, members) = {
            let open = self.open.get(&workspace).expect("workspace is open");
            let physical = open.repo.documents()?;
            let mut members = open
                .manifest
                .entries()
                .into_iter()
                .map(|entry| entry.id)
                .collect::<std::collections::HashSet<_>>();
            members.insert(open.manifest_id);
            (physical, members)
        };

        for state in physical {
            let owner = self.catalog.locate(state.id)?;
            if members.contains(&state.id) {
                match owner {
                    None => {
                        self.catalog.record_location(state.id, workspace)?;
                    }
                    Some(owner) if owner == workspace => {}
                    Some(other) => {
                        // A stale owner must not outlive its own authority:
                        // only the recorded owner's manifest may contradict
                        // this one. If that workspace is gone, or its manifest
                        // does not list the document, the row is projection
                        // garbage — re-record. The guard against a
                        // cross-workspace move in flight lives in
                        // `owner_disclaims`, not in never repairing.
                        if self.owner_disclaims(other, state.id)? {
                            self.catalog.record_location(state.id, workspace)?;
                        }
                    }
                }
            } else if owner == Some(workspace) {
                self.catalog.forget_location(state.id)?;
            }
        }
        Ok(())
    }

    /// Whether a recorded owner has lost its say over a document: the
    /// workspace is gone entirely, or its authoritative manifest does not
    /// list the document. An owner that cannot be opened keeps its claim —
    /// the repair must never steal on the strength of an error.
    fn owner_disclaims(&mut self, owner: WorkspaceId, document: DocumentId) -> Result<bool> {
        if self.catalog.workspace(owner)?.is_none() {
            return Ok(true);
        }
        if self.ensure_open_including_deleted(owner).is_err() {
            return Ok(false);
        }
        let claims = self
            .open
            .get(&owner)
            .expect("just opened")
            .manifest
            .entries()
            .into_iter()
            .any(|entry| entry.id == document);
        Ok(!claims)
    }

    /// Keep the catalog's switcher row aligned with the durable manifest.
    ///
    /// The catalog is derived here: replication moves manifest CRDT bytes, not
    /// a second last-writer-wins name register. This is called after every
    /// durable manifest commit and on open, so missed events and crashes repair
    /// by rereading authoritative state.
    fn reconcile_workspace_projection(
        &mut self,
        id: WorkspaceId,
        projection: WorkspaceManifestProjection,
    ) -> Result<()> {
        let Some(mut workspace) = self.catalog.workspace(id)? else {
            return Err(Error::bare(Code::WorkspaceNotFound));
        };
        let mut changed = false;
        if let Some(name) = projection.name
            && workspace.name != name
        {
            workspace.rename(&name)?;
            changed = true;
        }
        if let Some(icon) = projection.icon
            && workspace.icon != icon
        {
            workspace.icon = icon;
            changed = true;
        }
        if let Some(color) = projection.color
            && workspace.color != color
        {
            workspace.color = color;
            changed = true;
        }
        if let Some(pinned) = projection.pinned
            && workspace.pinned != pinned
        {
            workspace.pinned = pinned;
            changed = true;
        }
        if let Some(archived) = projection.archived
            && workspace.archived != archived
        {
            workspace.archived = archived;
            changed = true;
        }
        if changed {
            self.catalog.put_workspace(&workspace)?;
        }
        let deleted_projection = projection.deleted_at;
        let needs_deleted_change = match deleted_projection {
            Some(deleted_at) => workspace.deleted_at != deleted_at,
            // The manifest never recorded a deletion, so this workspace is
            // not deleted: a stale row — set from a bootstrap descriptor, or
            // by an earlier reconcile against a manifest that had not settled
            // — must not pin it in the trash. Skipping `None` here was how a
            // workspace stayed deleted on every device but one.
            None => workspace.deleted_at.is_some(),
        };
        if needs_deleted_change {
            let deleted_at = deleted_projection.flatten();
            let documents: Vec<_> = match self.open.get(&id) {
                Some(open) => open.repo.documents()?,
                None => self.backend.open_workspace(id)?.documents()?,
            }
            .into_iter()
            .map(|document| document.id)
            .collect();
            self.catalog
                .set_workspace_deleted(id, deleted_at, &documents)?;
            if deleted_at.is_some() {
                self.open.remove(&id);
            }
        }
        Ok(())
    }

    /// Whether a workspace physically holds a document, manifest entry or not.
    ///
    /// The counterpart of [`Self::owner_disclaims`], which asks only the
    /// manifest. Between them they cover the two halves of a document's
    /// arrival, which land in either order: a manifest entry replicates ahead
    /// of the bytes on a move, and `provision_replica_document` writes the
    /// bytes ahead of the entry. A workspace that cannot be opened answers
    /// `false` rather than erroring — `owner_disclaims` has already kept the
    /// claim in that case, so this is only ever the second opinion.
    fn workspace_holds_document(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<bool> {
        if self.catalog.workspace(workspace)?.is_none() {
            return Ok(false);
        }
        if self.ensure_open_including_deleted(workspace).is_err() {
            return Ok(false);
        }
        let Some(open) = self.open.get(&workspace) else {
            return Ok(false);
        };
        if open.is_manifest(document) {
            return Ok(true);
        }
        Ok(open.repo.document(document)?.is_some())
    }

    /// Whether an active workspace's durable state agrees that it owns a
    /// document. The manifest is the membership authority; the repository row
    /// is the bytes that make that membership openable.
    fn workspace_owns_document(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<bool> {
        if self
            .catalog
            .workspace(workspace)?
            .is_none_or(|workspace| workspace.deleted_at.is_some())
        {
            return Ok(false);
        }
        self.ensure_open(workspace)?;
        let Some(open) = self.open.get(&workspace) else {
            // Opening a manifest can discover that its workspace was deleted
            // by a remote peer and evict it from the live map.
            return Ok(false);
        };
        if open.is_manifest(document) {
            return Ok(true);
        }
        Ok(open.manifest.contains(document) && open.repo.document(document)?.is_some())
    }

    fn ensure_document(&mut self, workspace: WorkspaceId, document: DocumentId) -> Result<()> {
        let deleted = self
            .catalog
            .workspace(workspace)?
            .is_some_and(|workspace| workspace.deleted_at.is_some());
        if deleted {
            self.ensure_open_including_deleted(workspace)?;
            if self.open[&workspace].is_manifest(document) {
                return Ok(());
            }
            return Err(Error::bare(Code::DocumentNotFound));
        }
        self.ensure_open(workspace)?;
        let open = &self.open[&workspace];
        if open.is_manifest(document) || open.documents.contains_key(&document) {
            return Ok(());
        }
        let open = self.open.get_mut(&workspace).expect("just opened");
        if open.repo.document(document)?.is_none() {
            return Err(Error::bare(Code::DocumentNotFound));
        }
        let span = crate::stats::Span::start(crate::stats::names::DOCUMENT_OPEN);
        let opened = open_document(&open.repo, document)?;
        span.stop();
        if let Opened::Recovered(_, recovery) = &opened {
            self.recoveries.push(format!(
                "workspace {workspace}: document {document} — {}",
                recovery.problem
            ));
        }
        let live = opened.into_value();
        live.set_peer_id(self.peer_id)?;
        open.documents.insert(document, live);
        // Same repair as the manifest's at workspace open: the journal was
        // just replayed to load this document, so an overdue checkpoint is
        // taken here rather than waiting for an edit that may never come.
        let policy = self.checkpoints;
        checkpoint_if_overdue(&policy, workspace, open, document);
        Ok(())
    }
}

/// Persist a profile mutation or put the exact pre-mutation CRDT back in the
/// open slot. A Store error must never leave state visible that its durable
/// catalog did not acknowledge; otherwise an exact retry can mistake that
/// memory-only state for an already committed operation.
#[cfg(test)]
fn commit_profile_mutation(
    manifest: &mut ProfileManifest,
    peer_id: u64,
    before: &[u8],
    commit: impl FnOnce(&Profile, &[u8]) -> Result<()>,
) -> Result<()> {
    let result = (|| {
        let stored = manifest.profile()?;
        let snapshot = manifest.export_snapshot()?;
        commit(&stored, &snapshot)
    })();
    match result {
        Ok(()) => Ok(()),
        Err(error) => Err(restore_after_failed_profile_mutation(
            manifest, peer_id, before, error,
        )),
    }
}

#[cfg(test)]
fn restore_after_failed_profile_mutation(
    manifest: &mut ProfileManifest,
    peer_id: u64,
    before: &[u8],
    original: Error,
) -> Error {
    let restored = ProfileManifest::from_snapshot(before).and_then(|restored| {
        restored.set_peer_id(peer_id)?;
        Ok(restored)
    });
    match restored {
        Ok(restored) => {
            *manifest = restored;
            original
        }
        Err(_) => Error::new(
            Code::Internal,
            format!(
                "could not restore profile after {}",
                original.code().as_str()
            ),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn classify_reminder_changes(
    before_reminders: &BTreeMap<ReminderId, StoredReminder>,
    after_reminders: &BTreeMap<ReminderId, StoredReminder>,
    before_actions: &BTreeMap<(ReminderId, ActionId), OccurrenceAction>,
    after_actions: &BTreeMap<(ReminderId, ActionId), OccurrenceAction>,
    before_cleared: &BTreeSet<(ReminderId, ActionId)>,
    after_cleared: &BTreeSet<(ReminderId, ActionId)>,
    before_settings: &ReminderSettings,
    after_settings: &ReminderSettings,
) -> Vec<(Option<ReminderId>, ReminderChange)> {
    let mut changes = Vec::new();
    let changed_action_owners: BTreeSet<_> = before_actions
        .keys()
        .chain(after_actions.keys())
        .filter(|key| before_actions.get(key) != after_actions.get(key))
        .map(|(reminder, _)| *reminder)
        .collect();
    for reminder in before_reminders
        .keys()
        .chain(after_reminders.keys())
        .copied()
        .collect::<BTreeSet<_>>()
    {
        let before = before_reminders.get(&reminder);
        let after = after_reminders.get(&reminder);
        if before.map(|value| (&value.rule, value.deleted))
            != after.map(|value| (&value.rule, value.deleted))
        {
            changes.push((Some(reminder), ReminderChange::Rule));
        }
        if changed_action_owners.contains(&reminder) {
            changes.push((Some(reminder), ReminderChange::Action));
        }
    }
    if before_cleared != after_cleared {
        changes.push((None, ReminderChange::History));
    }
    if before_settings != after_settings {
        changes.push((None, ReminderChange::Settings));
    }
    changes
}

fn create_retry(existing: &StoredReminder, requested: &ReminderRule) -> Result<()> {
    if existing.rule == *requested && !existing.deleted {
        Ok(())
    } else {
        Err(Error::new(
            Code::IdempotencyConflict,
            "reminder id already names another rule",
        ))
    }
}

fn update_writes(existing: &StoredReminder, requested: &ReminderRule) -> Result<bool> {
    if existing.deleted {
        return Err(Error::new(
            Code::IdempotencyConflict,
            "a deleted reminder cannot be recreated",
        ));
    }
    if existing.rule == *requested {
        return Ok(false);
    }
    if existing.rule.revision == requested.revision {
        return Err(Error::new(
            Code::IdempotencyConflict,
            "rule revision already names another value",
        ));
    }
    Ok(true)
}

fn action_retry(prior: &OccurrenceAction, requested: &OccurrenceAction) -> Result<()> {
    if prior == requested {
        Ok(())
    } else {
        Err(Error::new(
            Code::IdempotencyConflict,
            "action id already names another action",
        ))
    }
}

#[cfg(test)]
mod tests;

/// Persist one update immediately — used where the store itself is the author,
/// so there is no window to wait for and nothing to acknowledge to anyone.
/// Commit one update on its own, outside the group-commit window.
///
/// **This is the store's *other* durable path**, and forgetting it is how
/// derived state silently misses whole categories of change: creating a
/// workspace, renaming one and creating a document all land here and never go
/// near [`Store::flush`]. So it records into the committed queue itself rather
/// than trusting three call sites to remember — the one place a fourth caller
/// cannot be added without inheriting the rule.
#[allow(clippy::too_many_arguments)]
fn commit_one<W: WorkspaceRepo + DocumentIndex>(
    open: &mut Open<W>,
    workspace: WorkspaceId,
    committed: &mut crate::commit_signal::Commits,
    sync_dirty: &mut HashMap<WorkspaceId, BTreeSet<DocumentId>>,
    document: DocumentId,
    bytes: Vec<u8>,
    at: Timestamp,
    origin: crate::ports::UpdateOrigin,
) -> Result<u64> {
    use crate::ports::UpdateBatch;
    // **Who changed what, and how much of it.** `from` is the client that sent
    // the update, and `None` means the store itself — a workspace being created,
    // or a manifest rename. Without this a document changing was invisible: the
    // access log shows a WebSocket frame arriving and nothing about what it did.
    tracing::debug!(
        %document,
        bytes = bytes.len(),
        origin = ?origin,
        "update to {} ({} bytes)",
        document,
        bytes.len()
    );
    let (version_vector, frontiers) = open
        .position(document)
        .ok_or_else(|| Error::bare(Code::DocumentNotFound))?;
    // The same measurement as the group-committed path: a commit is a commit,
    // and a table that showed only some of them would be worse than no table.
    let span = crate::stats::Span::start(crate::stats::names::COMMIT);
    let head = open.repo.commit_updates(&UpdateBatch {
        document,
        updates: vec![bytes],
        version_vector: version_vector.clone(),
        frontiers,
        received_at: at,
        origins: vec![origin],
    });
    span.stop();
    let head = head?;
    // After the write, never before — the rule this whole queue is for.
    committed.insert(
        (workspace, document),
        Committed {
            workspace,
            document,
            seq: head,
            version_vector,
            is_manifest: open.is_manifest(document),
        },
    );
    sync_dirty.entry(workspace).or_default().insert(document);
    Ok(head)
}

fn sync_document_digest(
    workspace: WorkspaceId,
    state: &crate::ports::DocumentState,
) -> SyncDocumentDigest {
    SyncDocumentDigest {
        workspace,
        document: state.id,
        source_revision: state.version_vector.clone(),
        digest: crate::sync_digest::document(
            workspace,
            state.id,
            crate::sync_digest::revision(&state.version_vector),
        )
        .to_vec(),
    }
}

/// A fresh Loro `PeerID`.
///
/// Random, and **never derived from a machine name, a hostname or a path** — a
/// peer id names a replica, and a replica is not a person or a computer. Taken
/// from a v4 UUID so it comes out of `getrandom` rather than a clock.
fn mint_peer_id() -> u64 {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let mut eight = [0u8; 8];
    eight.copy_from_slice(&bytes[..8]);
    // Loro reserves the top of the range; keeping to 63 bits avoids it.
    u64::from_le_bytes(eight) & (u64::MAX >> 1)
}

// **The tests for this module live in `koru-store-sqlite/tests/service.rs`.**
// Every operation here needs a `Backend`, and the only honest one is the SQLite
// implementation — a fake would prove that the service agrees with a fake. Core
// cannot depend on the persistence crate (it is the other way round), so the
// tests sit on the far side of that edge, where both are reachable.
