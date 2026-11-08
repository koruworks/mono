// SPDX-License-Identifier: MPL-2.0

//! The one Loro shape the store defines.
//!
//! Everything else a workspace holds is opaque to the store — checkpoint bytes
//! and update bytes, whose meaning belongs to the application. The manifest is
//! the exception, and it exists for exactly one reason: **a workspace has to be
//! listable without opening every document in it.** That is what makes lazy
//! loading possible, and lazy loading is what keeps store startup independent
//! of how much a person has written.
//!
//! ```text
//! meta      : LoroMap    workspace_id, name, created_at, schema_version
//! documents : LoroTree   one node per document
//!                        meta = { doc_id, title, kind, deletions? }
//! ```
//!
//! A deletion adds a uniquely identified record to the mergeable `deletions`
//! map. Restore removes only the records visible to it, so a concurrent unseen
//! deletion remains active. The legacy `trashed_at`/`trashed_by` fields are
//! maintained as display hints but are not lifecycle truth in a v2 manifest.
//!
//! ## Why a tree, and why a map value for the title
//!
//! **`LoroTree` for the document list**, not a list: it already gives ordered,
//! movable, nestable children through `mov_to(tid, parent, index)`. A second
//! ordering mechanism beside it would be a second thing to converge.
//!
//! **The title is a map value, not a `LoroText`.** A `LoroText` would merge two
//! concurrent renames character by character, and "Quarterly Plaarterly Plann"
//! is a worse answer than either edit winning outright. A map key is a
//! last-writer-wins register, which is the semantic a title actually wants.
//! Body text, where merging *is* right, lives in the document — which is not
//! this file's business.
//!
//! ## Two Loro facts this file is built around
//!
//! - **`get_or_create_container` is deprecated and diverges.** It creates
//!   op-id children, so two peers creating the same node concurrently end up
//!   with two containers and one wins. `ensure_mergeable_map` is used wherever
//!   peers can race.
//! - **There is no mergeable `LoroTree::create`.** Two peers can each create a
//!   document entry before they meet, and nothing prevents it — which is fine,
//!   because two documents *are* two documents. What it does mean is that
//!   nothing here may assume a `DocumentId` maps to exactly one `TreeID`
//!   without checking; [`Manifest::reindex`] resolves duplicates by keeping the
//!   lowest `TreeID`, deterministically on every replica.

use std::collections::HashMap;

use loro::{ExportMode, LoroDoc, LoroMap, LoroValue, TreeID, TreeParentId, ValueOrContainer};

use crate::errors::{Code, Error, Result};
use crate::ids::{DocumentId, WorkspaceId};
use crate::model::{DocumentEntry, Timestamp};
use crate::version::{CRDT_SCHEMA_VERSION, Compatibility};

const C_META: &str = "meta";
const C_DOCUMENTS: &str = "documents";

const K_WORKSPACE: &str = "workspace_id";
const K_NAME: &str = "name";
const K_CREATED: &str = "created_at";
const K_SCHEMA: &str = "schema_version";
const K_ICON: &str = "icon";
const K_COLOR: &str = "color";
const K_PINNED: &str = "pinned";
const K_ARCHIVED: &str = "archived";
const K_WORKSPACE_DELETED_AT: &str = "workspace_deleted_at";
const K_WORKSPACE_DELETIONS: &str = "workspace_deletions";

const K_DOC_ID: &str = "doc_id";
const K_TITLE: &str = "title";
const K_KIND: &str = "kind";
const K_MIME: &str = "mime";
/// The note document attached to a file, as its `DocumentId` storage form.
/// Written only when set: a manifest from before file notes reads back with
/// the key absent, which is what makes this additive rather than a schema
/// version bump.
const K_NOTE_DOC: &str = "note_doc";
/// Milliseconds, or absent. See [`DocumentEntry::trashed_at`].
const K_TRASHED_AT: &str = "trashed_at";
/// A display label for whoever binned it. See [`DocumentEntry::trashed_by`].
const K_TRASHED_BY: &str = "trashed_by";
/// Mergeable map keyed by a unique deletion id. Restore deletes the keys it has
/// observed, which gives delete-wins semantics to a concurrent unseen delete.
const K_DELETIONS: &str = "deletions";
const K_DELETED_AT: &str = "at";
const K_DELETED_BY: &str = "by";

/// A workspace's manifest document.
///
/// Holds a live `LoroDoc` and an index from [`DocumentId`] to `TreeID`. The
/// index is a **cache rebuilt after every import** — a `TreeID` is `{peer,
/// counter}` and valid only inside one document, so it is never an identity and
/// never persisted.
pub struct Manifest {
    doc: LoroDoc,
    index: HashMap<DocumentId, TreeID>,
}

impl Manifest {
    // ------------------------------------------------------------ lifecycle

    /// A brand-new manifest for a brand-new workspace.
    pub fn create(workspace: WorkspaceId, name: &str, at: Timestamp) -> Result<Self> {
        let doc = LoroDoc::new();
        let meta = doc.get_map(C_META);
        meta.insert(K_WORKSPACE, workspace.to_storage().as_str())
            .map_err(loro_failed)?;
        meta.insert(K_NAME, name).map_err(loro_failed)?;
        meta.insert(K_CREATED, at.millis()).map_err(loro_failed)?;
        meta.insert(K_SCHEMA, CRDT_SCHEMA_VERSION)
            .map_err(loro_failed)?;
        meta.insert(K_PINNED, false).map_err(loro_failed)?;
        meta.insert(K_ARCHIVED, false).map_err(loro_failed)?;
        meta.ensure_mergeable_map(K_WORKSPACE_DELETIONS)
            .map_err(loro_failed)?;
        doc.commit();
        Ok(Self {
            doc,
            index: HashMap::new(),
        })
    }

    pub fn from_snapshot(bytes: &[u8]) -> Result<Self> {
        let doc = LoroDoc::new();
        doc.import(bytes)
            .map_err(|e| Error::new(Code::CorruptState, format!("manifest snapshot: {e}")))?;
        let mut manifest = Self {
            doc,
            index: HashMap::new(),
        };
        manifest.reindex();
        Ok(manifest)
    }

    /// Pin this replica's Loro `PeerID`.
    ///
    /// The store's durable replica calls this from a value it keeps for the
    /// life of the install. Without it a `LoroDoc`'s peer is random **per
    /// construction — including every reload from a snapshot**, so a store that
    /// restarted a hundred times would carry a hundred peers in every version
    /// vector it ever compared or encoded.
    ///
    /// The value names a *replica*, never a person and never a machine: it is
    /// minted randomly and stored, not derived from a hostname.
    pub fn set_peer_id(&self, peer: u64) -> Result<()> {
        self.doc
            .set_peer_id(peer)
            .map_err(|e| Error::new(Code::StorageFailed, format!("set_peer_id: {e}")))
    }

    pub fn peer_id(&self) -> u64 {
        self.doc.peer_id()
    }

    /// Label every commit this manifest makes, while the returned subscription
    /// lives, with `message` as its Loro commit message.
    ///
    /// **For the commits the Store makes on behalf of an app connection** —
    /// the entry a created or extracted document gets, the registration of a
    /// workspace's system document — which carry the connection's `actor/1`
    /// attribution (`contracts/connector-attribution-v1.md`). A subscription
    /// rather than `set_next_commit_message`, deliberately: a message set on
    /// the pending transaction would survive a write refused before it made
    /// an operation, and label the next commit somebody else makes. Dropping
    /// the subscription ends it.
    pub fn attribute_commits(&self, message: &str) -> loro::Subscription {
        let message: std::sync::Arc<str> = message.into();
        self.doc.subscribe_pre_commit(Box::new(move |commit| {
            commit.modifier.set_message(&message);
            true
        }))
    }

    // ------------------------------------------------------------ transport

    pub fn export_snapshot(&self) -> Result<Vec<u8>> {
        self.doc
            .export(ExportMode::snapshot())
            .map_err(|e| Error::new(Code::StorageFailed, format!("export snapshot: {e}")))
    }

    pub fn export_all_updates(&self) -> Result<Vec<u8>> {
        self.doc
            .export(ExportMode::all_updates())
            .map_err(|e| Error::new(Code::StorageFailed, format!("export updates: {e}")))
    }

    pub fn export_updates_since(&self, from: &loro::VersionVector) -> Result<Vec<u8>> {
        self.doc
            .export(ExportMode::updates(from))
            .map_err(|e| Error::new(Code::StorageFailed, format!("export updates: {e}")))
    }

    pub fn version(&self) -> loro::VersionVector {
        self.doc.oplog_vv()
    }

    pub fn frontiers(&self) -> loro::Frontiers {
        self.doc.oplog_frontiers()
    }

    /// Apply one update. **Reindexes**, so a loop around this is quadratic —
    /// use [`Manifest::import_frames`] for a batch.
    pub fn import(&mut self, bytes: &[u8]) -> Result<()> {
        self.doc
            .import(bytes)
            .map_err(|e| Error::new(Code::CrdtRejected, format!("import: {e}")))?;
        self.reindex();
        Ok(())
    }

    /// The Cloud carrier cannot acknowledge Loro's pending-ancestor queue.
    /// Mutate/reindex only after the entire staged update applies.
    pub(crate) fn import_cloud(&mut self, bytes: &[u8]) -> Result<()> {
        crate::lifecycle::validate_cloud_import(&self.doc, bytes)?;
        self.import(bytes)
    }

    /// Apply many updates, reindexing **once**.
    ///
    /// One at a time rather than `import_batch`, deliberately: a batch import
    /// resolves the frames against each other and so cannot say which one was
    /// bad, and "which frame was bad" is the whole of a recovery report.
    pub fn import_frames(&mut self, frames: &[Vec<u8>]) -> Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        for (i, frame) in frames.iter().enumerate() {
            self.doc
                .import(frame)
                .map_err(|e| Error::new(Code::CrdtRejected, format!("frame {i}: {e}")))?;
        }
        self.reindex();
        Ok(())
    }

    /// Whether this replica already has everything in a frame, read from the
    /// frame's **header alone**.
    ///
    /// The saving is the point: deciding this without decoding the body is what
    /// keeps replaying a journal proportional to what is new rather than to
    /// what exists.
    pub fn frame_is_known(&self, frame: &[u8]) -> bool {
        match LoroDoc::decode_import_blob_meta(frame, false) {
            Ok(meta) => self.version().includes_vv(&meta.partial_end_vv),
            Err(_) => false,
        }
    }

    // ------------------------------------------------------------ meta

    pub fn workspace_id(&self) -> Option<WorkspaceId> {
        WorkspaceId::parse(&self.meta_string(K_WORKSPACE)?)
    }

    pub fn name(&self) -> Option<String> {
        self.meta_string(K_NAME)
    }

    pub fn set_name(&self, name: &str) -> Result<()> {
        self.doc
            .get_map(C_META)
            .insert(K_NAME, name)
            .map_err(loro_failed)?;
        self.doc.commit();
        Ok(())
    }

    pub fn icon(&self) -> Option<String> {
        self.meta_string(K_ICON)
    }

    pub fn icon_state(&self) -> Option<Option<String>> {
        self.optional_meta_string(K_ICON)
    }

    pub fn color(&self) -> Option<String> {
        self.meta_string(K_COLOR)
    }

    pub fn color_state(&self) -> Option<Option<String>> {
        self.optional_meta_string(K_COLOR)
    }

    pub fn pinned(&self) -> Option<bool> {
        self.meta_bool(K_PINNED)
    }

    pub fn archived(&self) -> Option<bool> {
        self.meta_bool(K_ARCHIVED)
    }

    pub fn workspace_deleted_at(&self) -> Option<Timestamp> {
        self.workspace_deletion_state().flatten()
    }

    pub fn workspace_deletion_state(&self) -> Option<Option<Timestamp>> {
        let meta = self.doc.get_map(C_META);
        if deletion_map_at(&meta, K_WORKSPACE_DELETIONS).is_some() {
            return Some(
                active_deletions_at(&meta, K_WORKSPACE_DELETIONS)
                    .into_iter()
                    .map(|deletion| deletion.at)
                    .max_by_key(|at| at.millis()),
            );
        }
        meta.get(K_WORKSPACE_DELETED_AT)
            .map(|value| as_i64(value).map(Timestamp::from_millis))
    }

    /// Shared switcher appearance belongs to the workspace, not the device.
    /// Optional values are represented by absence so older manifests migrate
    /// when first edited without a schema rewrite.
    pub fn set_appearance(
        &self,
        icon: Option<&str>,
        color: Option<&str>,
        pinned: bool,
        archived: bool,
    ) -> Result<()> {
        self.writable()?;
        let meta = self.doc.get_map(C_META);
        set_optional_string(&meta, K_ICON, icon)?;
        set_optional_string(&meta, K_COLOR, color)?;
        meta.insert(K_PINNED, pinned).map_err(loro_failed)?;
        meta.insert(K_ARCHIVED, archived).map_err(loro_failed)?;
        self.doc.commit();
        Ok(())
    }

    pub fn set_workspace_deleted_at(&self, deleted_at: Option<Timestamp>) -> Result<()> {
        self.writable()?;
        let meta = self.doc.get_map(C_META);
        match deleted_at {
            Some(at) => {
                let deletions = meta
                    .ensure_mergeable_map(K_WORKSPACE_DELETIONS)
                    .map_err(loro_failed)?;
                let deletion = deletions
                    .ensure_mergeable_map(&uuid::Uuid::new_v4().to_string())
                    .map_err(loro_failed)?;
                deletion
                    .insert(K_DELETED_AT, at.millis())
                    .map_err(loro_failed)?;
                meta.insert(K_WORKSPACE_DELETED_AT, at.millis())
                    .map_err(loro_failed)?;
            }
            None => {
                if let Some(deletions) = deletion_map_at(&meta, K_WORKSPACE_DELETIONS) {
                    let observed: Vec<String> =
                        deletions.keys().map(|key| key.to_string()).collect();
                    for deletion in observed {
                        deletions.delete(&deletion).map_err(loro_failed)?;
                    }
                }
                meta.delete(K_WORKSPACE_DELETED_AT).map_err(loro_failed)?;
            }
        }
        self.doc.commit();
        Ok(())
    }

    pub fn created_at(&self) -> Option<Timestamp> {
        self.meta_i64(K_CREATED).map(Timestamp::from_millis)
    }

    pub fn schema_version(&self) -> Option<i64> {
        self.meta_i64(K_SCHEMA)
    }

    /// Whether this build may write to this manifest.
    ///
    /// A document reaches an older build through *sync*, not through a file, so
    /// this is checked on open and again before any mutation — a replica can
    /// become too new while it is already open.
    pub fn compatibility(&self) -> Compatibility {
        Compatibility::of_crdt(self.schema_version())
    }

    fn writable(&self) -> Result<()> {
        match self.compatibility() {
            Compatibility::Writable => Ok(()),
            c => Err(Error::new(Code::SchemaTooNew, c.to_string())),
        }
    }

    // ------------------------------------------------------------ documents

    /// Whether this manifest already publishes a document id.
    pub fn contains_document(&self, id: DocumentId) -> bool {
        self.index.contains_key(&id)
    }

    /// Register a document. Appended under `parent`, or at the root.
    pub fn add_document(
        &mut self,
        id: DocumentId,
        title: &str,
        kind: &str,
        parent: Option<DocumentId>,
    ) -> Result<()> {
        self.add_document_at(id, title, kind, parent, None)
    }

    /// Register a document **at a position among its siblings**.
    ///
    /// `None` appends, which is what [`add_document`](Self::add_document) is.
    /// The index counts children of `parent` — trashed ones included, since the
    /// trash is a stamp rather than a second tree — so it is the coordinate
    /// [`move_document`](Self::move_document) and `DocumentEntry::ordinal`
    /// already speak.
    ///
    /// **Past the end is the end.** A new page below the last one is an
    /// ordinary request; `mov_to` refuses an index past the length, so the
    /// clamp is here rather than at each caller.
    ///
    /// Created and then moved rather than created in place: one `commit` covers
    /// both, so a subscriber sees the document arrive where it belongs and
    /// never at the end first.
    pub fn add_document_at(
        &mut self,
        id: DocumentId,
        title: &str,
        kind: &str,
        parent: Option<DocumentId>,
        index: Option<usize>,
    ) -> Result<()> {
        self.add_document_at_with_mime(id, title, kind, None, parent, index)
    }

    /// Register a document and optional listing metadata used by file rows.
    pub fn add_document_at_with_mime(
        &mut self,
        id: DocumentId,
        title: &str,
        kind: &str,
        mime: Option<&str>,
        parent: Option<DocumentId>,
        index: Option<usize>,
    ) -> Result<()> {
        self.add_document_at_with_mime_and_trash(id, title, kind, mime, parent, index, None)
    }

    /// Register a document and, when supplied, stamp it as trashed in the same
    /// manifest update. Import observers therefore see a born-trashed row,
    /// never a live row followed by a second trash publication.
    #[allow(clippy::too_many_arguments)]
    pub fn add_document_at_with_mime_and_trash(
        &mut self,
        id: DocumentId,
        title: &str,
        kind: &str,
        mime: Option<&str>,
        parent: Option<DocumentId>,
        index: Option<usize>,
        trashed_at: Option<Timestamp>,
    ) -> Result<()> {
        self.writable()?;
        if self.index.contains_key(&id) {
            return Err(Error::new(Code::DocumentExists, "already in this manifest"));
        }
        let under = self.parent_of(parent)?;
        let tree = self.doc.get_tree(C_DOCUMENTS);
        let tid = tree.create(under).map_err(loro_failed)?;
        let meta = tree.get_meta(tid).map_err(loro_failed)?;
        meta.insert(K_DOC_ID, id.to_storage().as_str())
            .map_err(loro_failed)?;
        meta.insert(K_TITLE, title).map_err(loro_failed)?;
        meta.insert(K_KIND, kind).map_err(loro_failed)?;
        if let Some(mime) = mime {
            meta.insert(K_MIME, mime).map_err(loro_failed)?;
        }
        if let Some(at) = trashed_at {
            let deletions = meta
                .ensure_mergeable_map(K_DELETIONS)
                .map_err(loro_failed)?;
            let deletion = deletions
                .ensure_mergeable_map(&uuid::Uuid::new_v4().to_string())
                .map_err(loro_failed)?;
            deletion
                .insert(K_DELETED_AT, at.millis())
                .map_err(loro_failed)?;
            self.doc
                .get_map(C_META)
                .insert(K_SCHEMA, CRDT_SCHEMA_VERSION)
                .map_err(loro_failed)?;
            write_trash_hint(&meta, active_deletions(&meta))?;
        }
        if let Some(index) = index {
            // It is already the last child, so the length counts it — and an
            // index of `len` is where it already is.
            let last = tree.children_num(under).unwrap_or(0).saturating_sub(1);
            tree.mov_to(tid, under, index.min(last))
                .map_err(loro_failed)?;
        }
        self.doc.commit();
        self.index.insert(id, tid);
        Ok(())
    }

    /// Rename a document. Last writer wins — see the module docs.
    pub fn rename_document(&mut self, id: DocumentId, title: &str) -> Result<()> {
        self.writable()?;
        let meta = self.meta_of(id)?;
        meta.insert(K_TITLE, title).map_err(loro_failed)?;
        self.doc.commit();
        Ok(())
    }

    /// Attach a note document to a file, or detach it with `None`.
    ///
    /// The link lives on the *file's* meta; the note is an ordinary manifest
    /// entry registered as the file's child, so trash and purge already follow
    /// it. Clearing deletes the key rather than writing a null, so a manifest
    /// that never had a note and one that lost it are byte-identical.
    pub fn set_note(&mut self, id: DocumentId, note: Option<DocumentId>) -> Result<()> {
        self.writable()?;
        let meta = self.meta_of(id)?;
        match note {
            Some(note) => meta
                .insert(K_NOTE_DOC, note.to_storage().as_str())
                .map_err(loro_failed)?,
            None => meta.delete(K_NOTE_DOC).map_err(loro_failed)?,
        }
        self.doc.commit();
        Ok(())
    }

    /// The note attached to a document, when it has one. `DocumentNotFound`
    /// for an id the manifest does not hold, exactly as a rename would report.
    pub fn note_of(&self, id: DocumentId) -> Result<Option<DocumentId>> {
        let meta = self.meta_of(id)?;
        Ok(meta
            .get(K_NOTE_DOC)
            .and_then(as_string)
            .as_deref()
            .and_then(DocumentId::parse))
    }

    /// Move a document under a new parent, at a position among its siblings.
    pub fn move_document(
        &mut self,
        id: DocumentId,
        parent: Option<DocumentId>,
        index: usize,
    ) -> Result<()> {
        self.writable()?;
        if parent == Some(id) {
            return Err(Error::new(
                Code::InvalidRequest,
                "a document cannot parent itself",
            ));
        }
        if let Some(p) = parent
            && self.ancestors(p)?.contains(&id)
        {
            return Err(Error::new(
                Code::InvalidRequest,
                "that move would put a document inside its own subtree",
            ));
        }
        let tid = self.tree_id(id)?;
        let under = self.parent_of(parent)?;
        self.doc
            .get_tree(C_DOCUMENTS)
            .mov_to(tid, under, index)
            .map_err(loro_failed)?;
        self.doc.commit();
        Ok(())
    }

    /// Put a document in the trash, and take it out again.
    ///
    /// **This is what *delete* means here** ([ADR-69](../../../docs/decisions/ADR-051-075.md#adr-69--trashing-a-document-is-a-move-into-a-hidden-library-node-and-nothing-ever-purges-it)):
    /// the entry stays, the bytes stay, and only [`trashed_at`] changes. Losing
    /// content is the one unrecoverable bug, so the button labelled *Move to
    /// trash* must not be able to cause it.
    ///
    /// **A trashed parent takes its subtree with it, and does so by
    /// inheritance rather than by marking each one.** A caller asks
    /// [`is_trashed`] rather than reading the field, so restoring the parent
    /// restores the lot — and a child that was trashed *on its own* before its
    /// parent went keeps its own stamp and stays in the bin when the parent
    /// comes back. Stamping the subtree instead would lose that distinction and
    /// would need every descendant rewritten on both operations.
    ///
    /// The counterpart that really destroys is [`remove_document`], and it is
    /// reached only by *Empty trash*.
    ///
    /// [`trashed_at`]: crate::model::DocumentEntry::trashed_at
    /// [`is_trashed`]: Self::is_trashed
    /// [`remove_document`]: Self::remove_document
    pub fn trash_document(
        &mut self,
        id: DocumentId,
        at: Timestamp,
        by: Option<&str>,
    ) -> Result<()> {
        self.writable()?;
        let meta = self.meta_of(id)?;
        let deletions = meta
            .ensure_mergeable_map(K_DELETIONS)
            .map_err(loro_failed)?;
        let deletion = deletions
            .ensure_mergeable_map(&uuid::Uuid::new_v4().to_string())
            .map_err(loro_failed)?;
        deletion
            .insert(K_DELETED_AT, at.millis())
            .map_err(loro_failed)?;
        if let Some(who) = by {
            deletion.insert(K_DELETED_BY, who).map_err(loro_failed)?;
        }
        self.doc
            .get_map(C_META)
            .insert(K_SCHEMA, CRDT_SCHEMA_VERSION)
            .map_err(loro_failed)?;
        write_trash_hint(&meta, active_deletions(&meta))?;
        self.doc.commit();
        Ok(())
    }

    /// Take a document out of the trash.
    ///
    /// If its parent is still in the bin, first detach it to the workspace
    /// root. Restore is an explicit request for this document to become live;
    /// leaving it beneath a trashed ancestor would report success while keeping
    /// it invisible, and a saved copy beside it would inherit the same state.
    pub fn restore_document(&mut self, id: DocumentId) -> Result<()> {
        self.writable()?;
        let entries = self.entries();
        let inherited = entries
            .iter()
            .find(|entry| entry.id == id)
            .and_then(|entry| entry.parent)
            .is_some_and(|parent| self.is_trashed(parent));
        if inherited {
            let root_end = entries
                .iter()
                .filter(|entry| entry.id != id && entry.parent.is_none())
                .count();
            self.move_document(id, None, root_end)?;
        }
        let meta = self.meta_of(id)?;
        if let Some(deletions) = deletion_map(&meta) {
            let observed: Vec<String> = deletions.keys().map(|key| key.to_string()).collect();
            for deletion in observed {
                deletions.delete(&deletion).map_err(loro_failed)?;
            }
        } else {
            // A v1 manifest has only the legacy register. Clearing it preserves
            // the old behavior and the first subsequent delete upgrades the
            // manifest to the observed-remove representation.
            meta.delete(K_TRASHED_AT).map_err(loro_failed)?;
            meta.delete(K_TRASHED_BY).map_err(loro_failed)?;
        }
        self.doc
            .get_map(C_META)
            .insert(K_SCHEMA, CRDT_SCHEMA_VERSION)
            .map_err(loro_failed)?;
        write_trash_hint(&meta, active_deletions(&meta))?;
        self.doc.commit();
        Ok(())
    }

    /// Whether this document is in the trash — **its own stamp, or any
    /// ancestor's**.
    ///
    /// The inheritance lives here rather than in every caller, because every
    /// caller getting it right is not a thing that happens: `entries()` reports
    /// the raw field, and *this* is the question a listing actually asks.
    pub fn is_trashed(&self, id: DocumentId) -> bool {
        let entries = self.entries();
        let by_id: HashMap<DocumentId, &crate::model::DocumentEntry> =
            entries.iter().map(|e| (e.id, e)).collect();
        let mut at = Some(id);
        // Bounded by the number of entries, so a parent chain that somehow
        // cycles cannot spin here.
        for _ in 0..entries.len().saturating_add(1) {
            let Some(current) = at.and_then(|id| by_id.get(&id)) else {
                return false;
            };
            if current.trashed_at.is_some() {
                return true;
            }
            at = current.parent;
        }
        false
    }

    /// Remove a document **and its subtree** from the manifest.
    ///
    /// This unregisters it; it does not delete its bytes. Reclaiming those is
    /// the persistence layer's decision, and keeping the two separate is what
    /// makes an accidental unregister recoverable.
    pub fn remove_document(&mut self, id: DocumentId) -> Result<Vec<DocumentId>> {
        self.writable()?;
        let tid = self.tree_id(id)?;
        let mut removed = self.subtree(tid);
        removed.push(id);
        self.doc
            .get_tree(C_DOCUMENTS)
            .delete(tid)
            .map_err(loro_failed)?;
        self.doc.commit();
        self.reindex();
        Ok(removed)
    }

    pub fn contains(&self, id: DocumentId) -> bool {
        self.index.contains_key(&id)
    }

    /// Every document, in tree order, as the rows the SQL mirror holds.
    ///
    /// This is the whole reason the manifest exists: a listing that costs one
    /// walk of a small tree rather than opening every document in the
    /// workspace.
    pub fn entries(&self) -> Vec<DocumentEntry> {
        let tree = self.doc.get_tree(C_DOCUMENTS);
        let mut out = Vec::new();
        self.collect(&tree, TreeParentId::Root, None, &mut out);
        out
    }

    /// Every document split into live and trashed listings, in tree order.
    ///
    /// Trash is inherited from a parent, so classifying one row at a time with
    /// [`is_trashed`](Self::is_trashed) turns a listing into a quadratic walk.
    /// Carrying the inherited state down the tree answers both listings in one
    /// pass instead.
    pub fn partitioned_entries(&self) -> (Vec<DocumentEntry>, Vec<DocumentEntry>) {
        let tree = self.doc.get_tree(C_DOCUMENTS);
        let mut live = Vec::new();
        let mut trashed = Vec::new();
        self.collect_partitioned(
            &tree,
            TreeParentId::Root,
            None,
            false,
            &mut live,
            &mut trashed,
        );
        (live, trashed)
    }

    fn collect(
        &self,
        tree: &loro::LoroTree,
        under: TreeParentId,
        parent: Option<DocumentId>,
        out: &mut Vec<DocumentEntry>,
    ) {
        let children = tree.children(under).unwrap_or_default();
        for (ordinal, tid) in children.into_iter().enumerate() {
            if tree.is_node_deleted(&tid).unwrap_or(false) {
                continue;
            }
            let Ok(meta) = tree.get_meta(tid) else {
                continue;
            };
            let Some(id) = meta
                .get(K_DOC_ID)
                .and_then(as_string)
                .as_deref()
                .and_then(DocumentId::parse)
            else {
                // A node with no readable id is not addressable and cannot be
                // reported. Skipped rather than fatal: one malformed row must
                // not make a workspace unlistable.
                continue;
            };
            // A duplicate id would produce two rows for one document. The
            // index already picked a winner; anything else is a stray.
            if self.index.get(&id) != Some(&tid) {
                continue;
            }
            let (trashed_at, trashed_by) = trash_state(&meta);
            out.push(DocumentEntry {
                id,
                parent,
                ordinal: ordinal as u32,
                title: meta.get(K_TITLE).and_then(as_string).unwrap_or_default(),
                kind: meta.get(K_KIND).and_then(as_string).unwrap_or_default(),
                mime: meta.get(K_MIME).and_then(as_string),
                note: meta
                    .get(K_NOTE_DOC)
                    .and_then(as_string)
                    .as_deref()
                    .and_then(DocumentId::parse),
                // **Reported, not filtered.** One walk serves both the listing
                // and the trash; a `collect` that dropped trashed rows would
                // leave the bin unlistable and its children unreachable.
                trashed_at,
                trashed_by,
            });
            self.collect(tree, TreeParentId::Node(tid), Some(id), out);
        }
    }

    fn collect_partitioned(
        &self,
        tree: &loro::LoroTree,
        under: TreeParentId,
        parent: Option<DocumentId>,
        ancestor_trashed: bool,
        live: &mut Vec<DocumentEntry>,
        trashed: &mut Vec<DocumentEntry>,
    ) {
        let children = tree.children(under).unwrap_or_default();
        for (ordinal, tid) in children.into_iter().enumerate() {
            if tree.is_node_deleted(&tid).unwrap_or(false) {
                continue;
            }
            let Ok(meta) = tree.get_meta(tid) else {
                continue;
            };
            let Some(id) = meta
                .get(K_DOC_ID)
                .and_then(as_string)
                .as_deref()
                .and_then(DocumentId::parse)
            else {
                continue;
            };
            if self.index.get(&id) != Some(&tid) {
                continue;
            }
            let (trashed_at, trashed_by) = trash_state(&meta);
            let entry = DocumentEntry {
                id,
                parent,
                ordinal: ordinal as u32,
                title: meta.get(K_TITLE).and_then(as_string).unwrap_or_default(),
                kind: meta.get(K_KIND).and_then(as_string).unwrap_or_default(),
                mime: meta.get(K_MIME).and_then(as_string),
                note: meta
                    .get(K_NOTE_DOC)
                    .and_then(as_string)
                    .as_deref()
                    .and_then(DocumentId::parse),
                trashed_at,
                trashed_by,
            };
            let entry_trashed = ancestor_trashed || entry.trashed_at.is_some();
            if entry_trashed {
                trashed.push(entry);
            } else {
                live.push(entry);
            }
            self.collect_partitioned(
                tree,
                TreeParentId::Node(tid),
                Some(id),
                entry_trashed,
                live,
                trashed,
            );
        }
    }

    // ------------------------------------------------------------ internals

    /// Rebuild [`DocumentId`] → `TreeID` from the tree.
    ///
    /// Runs after every import because a remote change can create, move or
    /// delete a node, and a `TreeID` from before is not a stable handle. Where
    /// two nodes claim one `DocumentId` — possible, since there is no mergeable
    /// `LoroTree::create` — the **lowest `TreeID` wins**, which every replica
    /// computes identically, so they agree without exchanging anything.
    fn reindex(&mut self) {
        let tree = self.doc.get_tree(C_DOCUMENTS);
        let mut index: HashMap<DocumentId, TreeID> = HashMap::new();
        for tid in tree.nodes() {
            if tree.is_node_deleted(&tid).unwrap_or(false) {
                continue;
            }
            let Ok(meta) = tree.get_meta(tid) else {
                continue;
            };
            let Some(id) = meta
                .get(K_DOC_ID)
                .and_then(as_string)
                .as_deref()
                .and_then(DocumentId::parse)
            else {
                continue;
            };
            index
                .entry(id)
                .and_modify(|winner| {
                    if tid < *winner {
                        *winner = tid;
                    }
                })
                .or_insert(tid);
        }
        self.index = index;
    }

    fn tree_id(&self, id: DocumentId) -> Result<TreeID> {
        self.index
            .get(&id)
            .copied()
            .ok_or_else(|| Error::new(Code::DocumentNotFound, "not in this manifest"))
    }

    fn meta_of(&self, id: DocumentId) -> Result<LoroMap> {
        self.doc
            .get_tree(C_DOCUMENTS)
            .get_meta(self.tree_id(id)?)
            .map_err(loro_failed)
    }

    fn parent_of(&self, parent: Option<DocumentId>) -> Result<TreeParentId> {
        match parent {
            None => Ok(TreeParentId::Root),
            Some(p) => Ok(TreeParentId::Node(self.tree_id(p)?)),
        }
    }

    fn ancestors(&self, id: DocumentId) -> Result<Vec<DocumentId>> {
        let tree = self.doc.get_tree(C_DOCUMENTS);
        let mut out = Vec::new();
        let mut cur = self.tree_id(id)?;
        while let Some(TreeParentId::Node(p)) = tree.parent(cur) {
            if let Ok(meta) = tree.get_meta(p)
                && let Some(pid) = meta
                    .get(K_DOC_ID)
                    .and_then(as_string)
                    .as_deref()
                    .and_then(DocumentId::parse)
            {
                out.push(pid);
            }
            cur = p;
        }
        Ok(out)
    }

    fn subtree(&self, tid: TreeID) -> Vec<DocumentId> {
        let tree = self.doc.get_tree(C_DOCUMENTS);
        let mut out = Vec::new();
        let mut stack = vec![tid];
        while let Some(cur) = stack.pop() {
            for child in tree.children(TreeParentId::Node(cur)).unwrap_or_default() {
                if tree.is_node_deleted(&child).unwrap_or(false) {
                    continue;
                }
                if let Ok(meta) = tree.get_meta(child)
                    && let Some(id) = meta
                        .get(K_DOC_ID)
                        .and_then(as_string)
                        .as_deref()
                        .and_then(DocumentId::parse)
                {
                    out.push(id);
                }
                stack.push(child);
            }
        }
        out
    }

    fn meta_string(&self, key: &str) -> Option<String> {
        self.doc.get_map(C_META).get(key).and_then(as_string)
    }

    fn meta_i64(&self, key: &str) -> Option<i64> {
        self.doc.get_map(C_META).get(key).and_then(as_i64)
    }

    fn meta_bool(&self, key: &str) -> Option<bool> {
        let ValueOrContainer::Value(LoroValue::Bool(value)) = self.doc.get_map(C_META).get(key)?
        else {
            return None;
        };
        Some(value)
    }

    fn optional_meta_string(&self, key: &str) -> Option<Option<String>> {
        match self.doc.get_map(C_META).get(key)? {
            ValueOrContainer::Value(LoroValue::Null) => Some(None),
            ValueOrContainer::Value(LoroValue::String(value)) => Some(Some(value.to_string())),
            _ => None,
        }
    }
}

#[derive(Clone)]
struct ActiveDeletion {
    at: Timestamp,
    by: Option<String>,
}

fn set_optional_string(map: &LoroMap, key: &str, value: Option<&str>) -> Result<()> {
    match value {
        Some(value) => map.insert(key, value).map_err(loro_failed)?,
        None => map.insert(key, LoroValue::Null).map_err(loro_failed)?,
    }
    Ok(())
}

fn deletion_map(meta: &LoroMap) -> Option<LoroMap> {
    deletion_map_at(meta, K_DELETIONS)
}

fn deletion_map_at(meta: &LoroMap, key: &str) -> Option<LoroMap> {
    let ValueOrContainer::Container(container) = meta.get(key)? else {
        return None;
    };
    container.into_map().ok()
}

fn active_deletions(meta: &LoroMap) -> Vec<ActiveDeletion> {
    active_deletions_at(meta, K_DELETIONS)
}

fn active_deletions_at(meta: &LoroMap, key: &str) -> Vec<ActiveDeletion> {
    let Some(deletions) = deletion_map_at(meta, key) else {
        return Vec::new();
    };
    deletions
        .keys()
        .filter_map(|key| {
            let ValueOrContainer::Container(container) = deletions.get(&key)? else {
                return None;
            };
            let record = container.into_map().ok()?;
            Some(ActiveDeletion {
                at: Timestamp::from_millis(record.get(K_DELETED_AT).and_then(as_i64)?),
                by: record.get(K_DELETED_BY).and_then(as_string),
            })
        })
        .collect()
}

fn trash_state(meta: &LoroMap) -> (Option<Timestamp>, Option<String>) {
    if deletion_map(meta).is_some() {
        return active_deletions(meta)
            .into_iter()
            .max_by_key(|deletion| deletion.at.millis())
            .map_or((None, None), |deletion| (Some(deletion.at), deletion.by));
    }
    (
        meta.get(K_TRASHED_AT)
            .and_then(as_i64)
            .map(Timestamp::from_millis),
        meta.get(K_TRASHED_BY).and_then(as_string),
    )
}

fn write_trash_hint(meta: &LoroMap, deletions: Vec<ActiveDeletion>) -> Result<()> {
    if let Some(deletion) = deletions
        .into_iter()
        .max_by_key(|deletion| deletion.at.millis())
    {
        meta.insert(K_TRASHED_AT, deletion.at.millis())
            .map_err(loro_failed)?;
        match deletion.by {
            Some(who) => meta.insert(K_TRASHED_BY, who).map_err(loro_failed)?,
            None => meta.delete(K_TRASHED_BY).map_err(loro_failed)?,
        }
    } else {
        meta.delete(K_TRASHED_AT).map_err(loro_failed)?;
        meta.delete(K_TRASHED_BY).map_err(loro_failed)?;
    }
    Ok(())
}

fn as_string(v: ValueOrContainer) -> Option<String> {
    match v.as_value()? {
        LoroValue::String(s) => Some(s.to_string()),
        _ => None,
    }
}

fn as_i64(v: ValueOrContainer) -> Option<i64> {
    match v.as_value()? {
        LoroValue::I64(i) => Some(*i),
        _ => None,
    }
}

fn loro_failed(e: impl std::fmt::Display) -> Error {
    Error::new(Code::StorageFailed, e.to_string())
}

#[cfg(test)]
mod tests;
