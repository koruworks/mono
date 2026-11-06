// SPDX-License-Identifier: MPL-2.0

//! What every view shares: the document, its persistence, and its undo history.
//!
//! The document used to live inside `Outline`, which was fine while there was
//! one view and impossible the moment there were two — a calendar and an
//! outline cannot each own the same `WorkspaceDoc`. It lives here instead, and
//! views borrow it for the length of a call.
//!
//! Everything on this struct is **per document, not per view**: sync, snapshots
//! and undo all belong to the document, which is why a document open in two
//! windows updates in both without either window knowing about the other.

use koru_core::node::Node;
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{Kind, NodeId};

pub mod durability;
pub mod explore;
pub mod fromstore;
pub mod open;
pub mod pages;
pub mod profile;
pub mod seed;
pub mod session;
pub mod store;
pub mod syncstats;
pub mod system;
pub mod tasks;

#[cfg(test)]
mod tests;

use session::Session;

/// How often a window looks at the store while something is outstanding.
///
/// **Not a poll of the socket** — that is the thread's job and an arriving frame
/// wakes the loop directly. This is the clock behind "how long has this been
/// reconnecting", which nothing else would advance, and it runs only while a
/// link is unhappy or an edit is unconfirmed. A quiet, connected workspace names
/// no deadline at all and the window sleeps.
const STORE_WATCHDOG: std::time::Duration = std::time::Duration::from_secs(1);

/// Re-exported so the shell can name the store it decodes blobs from without a
/// direct `koru-docstore` dependency of its own.
pub use koru_docstore::BlobStore;

/// Why this window is not reaching the store, and what that costs.
///
/// **Two facts, deliberately together.** `message` is why the connection is
/// unhappy and `owed` is how many edits the store has not confirmed — and only
/// the second is about the person's content. A socket that blipped and owes
/// nothing is not worth a word; one that is fine but owes eleven is.
///
/// **Facts, not a sentence.** This used to grow a `popover_detail` that built
/// the caption chip's second line — in English, from a crate that reaches no
/// locale and has no way to reach one, so the same window drew a translated
/// chip over an untranslated paragraph. The words are `koru_i18n::store`'s now
/// and these fields are what it is given ([ADR-262](../../../docs/decisions/README.md)).
#[derive(Clone, Debug)]
pub struct StoreTrouble {
    /// The transport's own diagnosis — an endpoint, a protocol mismatch, an OS
    /// error. **Not product copy and deliberately not translated**: it is what
    /// tells a running-but-incompatible store apart from no store at all, and a
    /// window shows it verbatim inside a localized frame.
    pub message: String,
    /// Edits handed over and not acknowledged. See [`Workspace::owed`].
    pub owed: usize,
    /// When the connection went, for a link that is retrying. `None` for one
    /// that has stopped, because that has no "since" worth showing — it is not
    /// coming back on its own.
    pub since: Option<std::time::Instant>,
    /// It will not recover without somebody doing something.
    pub fatal: bool,
    /// What is unconfirmed, and whether this machine has a copy of it.
    ///
    /// **Not the same question as `owed`, which is why both are here.** `owed`
    /// counts batches in flight; a lost connection clears those while rewinding,
    /// so it reads zero at the moment the most is at stake. This says whether
    /// anything typed is missing from the store's disk, and — the part somebody
    /// can act on — whether it is at least on theirs.
    pub unconfirmed: Unconfirmed,
}

/// Where a workspace's pictures live.
///
/// Two keepers with the same two operations, and the caller does not care which
/// it has — which is the whole point: `app.rs` stores a pasted image and decodes
/// one for drawing, and neither call should have to know whether this workspace
/// came from a folder or from the store.
///
/// **The addresses differ and that is not hidden.** A folder blob is FNV-1a and
/// a store blob is BLAKE3 ([ADR-93](../../../docs/decisions/ADR-076-100.md#adr-93-blobhash--the-store-owns-a-blobs-address-and-the-app-stops-hashing)),
/// so a hash written by one keeper is meaningless to the other. Nothing converts
/// between them: a document carries whichever address its own workspace uses,
/// and an old workspace's pictures are re-put by the import rather than
/// reinterpreted.
pub enum Blobs {
    Folder(BlobStore),
    Store(store::StoreBlobs),
}

impl Blobs {
    /// A cloneable Store loader for work that must leave the UI thread.
    /// Folder blobs remain cheap local file reads and use [`Self::get`].
    pub fn store_loader(&self) -> Option<store::StoreBlobs> {
        match self {
            Self::Store(blobs) => Some(blobs.clone()),
            Self::Folder(_) => None,
        }
    }

    /// Store bytes and return the address they are filed under.
    pub fn put(&self, bytes: &[u8]) -> Result<koru_core::node::BlobHash, String> {
        match self {
            Self::Folder(b) => b.put(bytes).map_err(|e| e.to_string()),
            // The mime is what the store files beside the bytes; the app has
            // always treated a pasted image as a PNG and the node carries the
            // real one, so this is the store's copy rather than a new fact.
            Self::Store(b) => b
                .put(bytes.to_vec(), "image/png", None)
                .map_err(|e| e.to_string()),
        }
    }

    /// The bytes behind an address.
    pub fn get(&self, hash: &koru_core::node::BlobHash) -> Result<Vec<u8>, String> {
        match self {
            Self::Folder(b) => b.get(hash).map_err(|e| e.to_string()),
            Self::Store(b) => b.get(hash).map_err(|e| e.to_string()),
        }
    }
}

pub struct Workspace {
    /// **The document being edited**, which is what almost every caller means.
    ///
    /// A workspace is becoming a *set* of documents rather than one
    /// ([ADR-91](../../../docs/decisions/ADR-076-100.md#adr-91-perdocument--a-document-keeps-a-root-node-for-its-title-and-the-manifest-mirrors-it)), and this field
    /// survives that as "the active one". Roughly 370 call sites reach through
    /// it, and every one of them means the document under the caret — so the
    /// field keeps its name and its meaning, and the several-documents story is
    /// told by [`documents`](Self::documents) and [`entries`](Self::entries)
    /// beside it.
    ///
    /// Making this an accessor over a map was the alternative. It renames 370
    /// sites to say exactly what they said before, and turns every `&mut` borrow
    /// of the active document into a map lookup the borrow checker has opinions
    /// about. The split is worth doing; that spelling of it is not.
    pub ws: WorkspaceDoc,
    /// What this workspace holds — every document, whether open or not.
    ///
    /// The store's own type, so moving this list onto the store is a change of
    /// *source* rather than a redesign; see `crates/koru-workspace/src/explore.rs`.
    /// `None` until a workspace has one, which is every workspace today.
    manifest: Option<koru_store_core::manifest::Manifest>,
    /// Documents that are open but are not the active one — the other tabs.
    ///
    /// Keyed by [`DocumentId`] rather than `NodeId`, because a `NodeId` is only
    /// meaningful *inside* the document that minted it and these are several
    /// documents.
    others: std::collections::HashMap<koru_core::DocumentId, WorkspaceDoc>,
    /// Which document [`ws`](Self::ws) is, once there is more than one to tell
    /// apart.
    active: Option<koru_core::DocumentId>,
    /// The live connection carrying the manifest, when this workspace came from
    /// a store.
    manifest_session: Option<store::StoreSession>,
    /// How to wake the event loop when a session has something to hand over.
    ///
    /// Held here, rather than passed to `StoreSession::join`, because sessions
    /// are made throughout a window's life — every document opened, every
    /// workspace switched to — and each one has to inherit it. A workspace with
    /// none simply never wakes anybody, which is what every test wants.
    waker: Option<store::link::Waker>,
    /// One connection per open document — the store gives each its own room, so
    /// a tab syncs what it is showing and nothing else.
    doc_sessions: std::collections::HashMap<koru_core::DocumentId, store::StoreSession>,
    /// Where to reach the store, so a document opened later finds the same one.
    ///
    /// The channel and an optional explicit root, which is what a test passes so
    /// several stores can run at once.
    store: Option<(String, Option<std::path::PathBuf>)>,
    /// One HTTP client for this workspace.
    ///
    /// **Built once.** `Client::open` runs the discovery handshake and the store
    /// mints a session for it, so building one per call put a
    /// *"session started"* line in the log for every document created, every
    /// rename, and every time the blob store was asked for — dozens of live
    /// credentials for one window's work.
    client: Option<koru_api_client::Client>,
    /// Disk and peer sync. `None` in tests and when no data directory could be
    /// found, in which case the workspace is an ordinary in-memory one.
    pub session: Option<Session>,
    /// Which workspace this is, once it came from a profile's list. `None` for
    /// an in-memory one, which is nobody's.
    current: Option<koru_core::WorkspaceId>,
    /// Who this window says it is, for anything it writes into a document.
    ///
    /// Set by whoever opened the profile, because the person's name lives in the
    /// *profile* document and the workspace document has no pointer to it — see
    /// [`set_actor`]. `None` is a normal state and every message built from it
    /// has a form with no name in it.
    ///
    /// [`set_actor`]: Self::set_actor
    actor: Option<String>,
    /// What could not be taken back off this machine's disk when a document was
    /// opened. See [`take_replay_trouble`](Self::take_replay_trouble).
    replay_trouble: Vec<String>,
    /// The manifest changed during the last Store turn. Kept separate from a
    /// document change so the shell can refresh Explore and pill captions
    /// without rebuilding the editor on every title keystroke.
    entries_changed: bool,
    /// Documents registered over HTTP whose manifest broadcast has not reached
    /// this replica yet. They are projected into `entries()` immediately, then
    /// discarded when the authoritative manifest row arrives.
    pending_documents: Vec<Pending>,
    /// The workspace **system document** (`"koru-system"`, ADR-176) — custom
    /// attribute definitions and the tag table — once something needed it.
    /// `None` until a tag is read or written; see `system.rs` for the lazy
    /// open, create, lowest-id adoption, and retained recovery rooms.
    system: Option<system::SystemSession>,
    /// The tag table changed — locally or from a peer — since the shell last
    /// asked. What drives re-pushing the resolved table into every view.
    tags_changed: bool,
    /// The **list** table changed, by the same rule and for the same push.
    ///
    /// A second flag rather than a share of `tags_changed`, because the two
    /// tables are read by different surfaces at different moments: the tasks
    /// rail redraws on a list rename, and the tag chooser does not. One flag
    /// would make each of them repaint for the other's news.
    lists_changed: bool,
    /// The page documents of Tasks pages this process is making exist, and
    /// the writing on disk the store has not confirmed for them. See
    /// [`pages`].
    pages: pages::PageRegistry,
    /// The `SystemStore` a test wants the lazy open to use, in place of the
    /// Store client production builds. `system.rs`'s `new_system_session` is
    /// the only reader, so a test exercises the real open path rather than
    /// installing a session past it.
    #[cfg(test)]
    pub(crate) system_store_for_tests: Option<std::sync::Arc<dyn system::SystemStore>>,
}

/// The start of the message a failed open answers once the store has
/// already made the document — the host reads it to say the right thing.
pub const CREATED_BUT_NOT_OPENED: &str = "the document was made, but could not be opened";

/// A document the store has registered and this replica's manifest has not
/// heard about yet.
///
/// `after` is carried as well as the id because the provisional row has to be
/// listed **where the real one will be**. Without it the new page appears at
/// the end of the sidebar and hops under the page it was made from a few
/// milliseconds later — the placement is right and looks like a glitch.
///
/// `kind` is carried for the same reason `after` is: the provisional row has to
/// be **what** the real one will be. A canvas projected as a page for the few
/// milliseconds before its manifest echo would appear in Explore's Documents
/// section and then vanish from it into Canvases — and a tab opened in that
/// window would be built as a page.
struct Pending {
    id: koru_core::DocumentId,
    title: String,
    after: Option<koru_core::DocumentId>,
    /// The document it was made inside, when it was
    /// ([`create_document_in`](Workspace::create_document_in)): the
    /// provisional row sits under it rather than at the root.
    parent: Option<koru_core::DocumentId>,
    kind: String,
}

/// Whether everything typed has reached the store's disk, and if not, where it
/// is instead.
///
/// **Three states, not two, and the third is the one worth a dialog.** Work the
/// store has confirmed is safe. Work it has not confirmed but this machine has
/// buffered is safe *and closing the window costs nothing*. Work that is in
/// neither place is the only case where closing loses something — and it is the
/// only case a person can act on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unconfirmed {
    /// How many open documents hold work the store has not confirmed.
    pub documents: usize,
    /// Whether the workspace's own list is unconfirmed — a document created,
    /// renamed, moved or binned. **Counted apart from `documents`**, because it
    /// is not one: saying "one document" for a rename nobody typed into is a
    /// number a person would go looking for and not find.
    pub document_list: bool,
    /// Why some of it could not be buffered, if any of it could not. `None`
    /// means everything unconfirmed is on this machine's disk, and closing
    /// costs nothing.
    pub not_buffered: Option<String>,
}

impl Unconfirmed {
    /// Nothing outstanding at all.
    pub fn is_empty(&self) -> bool {
        self.documents == 0 && !self.document_list
    }

    /// Whether closing now would lose something.
    pub fn would_lose_work(&self) -> bool {
        self.not_buffered.is_some()
    }

    /// What is outstanding, as a phrase that fits into a sentence.
    ///
    /// Here rather than in the shell because it has to agree with the fields,
    /// and a caller that formats its own is a caller that can disagree with
    /// them.
    ///
    /// **The one English sentence left on this type, and it is a native
    /// dialog's.** `may_close_with_unsent_work` builds an `rfd` message box out
    /// of it, and `rfd` draws OS chrome around English buttons — a translated
    /// body under a *Yes*/*No* pair is not the improvement it looks like. The
    /// caption chip's line, which Koru draws itself, went to
    /// `koru_i18n::store` (ADR-262) and this did not follow it.
    pub fn describe(&self) -> String {
        let documents = match self.documents {
            0 => None,
            1 => Some("1 document has changes".to_owned()),
            n => Some(format!("{n} documents have changes")),
        };
        match (documents, self.document_list) {
            (Some(d), false) => d,
            (Some(d), true) => format!("{d}, and so does this workspace's list of documents"),
            (None, true) => "This workspace's list of documents has changes".to_owned(),
            (None, false) => "Nothing has changed".to_owned(),
        }
    }
}

impl Workspace {
    pub fn new(ws: WorkspaceDoc) -> Self {
        Self {
            ws,
            manifest: None,
            others: std::collections::HashMap::new(),
            active: None,
            client: None,
            manifest_session: None,
            waker: None,
            doc_sessions: std::collections::HashMap::new(),
            store: None,
            session: None,
            current: None,
            actor: None,
            replay_trouble: Vec::new(),
            entries_changed: false,
            pending_documents: Vec::new(),
            pages: pages::PageRegistry::default(),
            system: None,
            tags_changed: false,
            lists_changed: false,
            #[cfg(test)]
            system_store_for_tests: None,
        }
    }

    /// A workspace holding one document **as its active document**, with no
    /// store behind it.
    ///
    /// **For a host's tests and for nothing else.** The task road
    /// (`run_task_command`) writes into the active document without a store
    /// and into a parked one only through the store, so a host that wants to
    /// pin what its wrapper does to a document — the guard, the activity, the
    /// undo step — needs a workspace whose active document is the one it
    /// hands in. Production opens documents through the store and never
    /// builds one of these; `cfg(test)` cannot cross a crate boundary, which
    /// is why this is a plain function rather than a test-only one.
    #[doc(hidden)]
    pub fn holding_for_tests(ws: WorkspaceDoc, id: koru_core::DocumentId) -> Self {
        let mut w = Self::with_session(ws, None);
        w.active = Some(id);
        w
    }

    pub fn with_session(ws: WorkspaceDoc, session: Option<Session>) -> Self {
        let current = ws.id();
        Self {
            ws,
            manifest: None,
            others: std::collections::HashMap::new(),
            active: None,
            client: None,
            manifest_session: None,
            waker: None,
            doc_sessions: std::collections::HashMap::new(),
            store: None,
            session,
            current,
            actor: None,
            replay_trouble: Vec::new(),
            entries_changed: false,
            pending_documents: Vec::new(),
            pages: pages::PageRegistry::default(),
            system: None,
            tags_changed: false,
            lists_changed: false,
            #[cfg(test)]
            system_store_for_tests: None,
        }
    }

    /// Open a workspace **from the Koru Store**.
    ///
    /// The counterpart of [`switch_to`](Self::switch_to), which opens one from a
    /// folder. What arrives is the manifest — the list of documents — and a live
    /// session on it, so [`entries`](Self::entries) answers from the store from
    /// this point on rather than by projecting a tree.
    ///
    /// **No document is opened here.** A workspace of two hundred documents must
    /// not cost two hundred snapshot fetches to show a sidebar; that is the
    /// whole point of a manifest existing separately
    /// ([ADR-80](../../../docs/decisions/ADR-076-100.md#adr-80-manifest--a-workspace-is-a-manifest-document-plus-opaque-documents)).
    /// The active document arrives through [`open_document`](Self::open_document)
    /// when something asks for one.
    ///
    /// `ws` is left as the empty document it was built with, and is replaced the
    /// first time a document is opened. It is *not* a workspace-shaped document
    /// with everything in it — that shape is what this change exists to end.
    pub fn open_from_store(
        &mut self,
        segment: &str,
        root: Option<&std::path::Path>,
        workspace: koru_core::WorkspaceId,
    ) -> Result<(), koru_store_core::errors::Error> {
        let (mut session, snapshot) =
            store::StoreSession::join_manifest(segment, root, explore::store_workspace(workspace))?;
        let mut manifest = koru_store_core::manifest::Manifest::from_snapshot(&snapshot)?;
        // Discover and authenticate the ordinary HTTP side too before touching
        // the outgoing workspace. A failed incoming open must leave all of its
        // rooms and documents intact.
        let client = match root {
            Some(root) => koru_api_client::Client::open_at(segment, root),
            None => koru_api_client::Client::open(segment),
        }?;
        // Creating, renaming and trashing a document is work like any other, so
        // the manifest has an outbox for the same reason a document does.
        self.replay_trouble
            .extend(session.replay_buffered(&mut manifest));
        // The incoming workspace is fully open before the outgoing one moves:
        // a failed switch must leave the current workspace untouched. Once it
        // has succeeded, however, every old document room has to go with the
        // old manifest room. Keeping `doc_sessions` here leaked invisible Store
        // connections from every workspace visited; deleting one later then
        // quite correctly failed with `workspace_in_use`, even though this
        // window had already switched away from it.
        self.close_store_sessions();
        self.manifest = Some(manifest);
        if let Some(waker) = &self.waker {
            session.set_waker(waker.clone());
        }
        self.manifest_session = Some(session);
        self.store = Some((segment.to_owned(), root.map(|r| r.to_path_buf())));
        // Built here, once, so every later call — create, rename, blobs — reuses
        // one session rather than running the handshake again.
        self.client = Some(client);
        self.current = Some(workspace);
        self.others.clear();
        self.active = None;
        self.pending_documents.clear();
        // A different workspace is a different tag table: have the shell read
        // (and lazily open) it on the next turn, and re-push into views. The
        // list table travels with it — same document, same reason.
        self.tags_changed = true;
        self.lists_changed = true;
        // Notes written above a Tasks page before a crash: taken up and
        // registered again, whatever page they were for.
        self.discover_pages();
        Ok(())
    }

    /// Switch workspace while keeping the named documents' rooms open.
    ///
    /// The keep-set is every document still open in the window strip (Main and
    /// Pinned). Closing a tab is what releases a room — a switch must not.
    pub fn open_from_store_keeping(
        &mut self,
        segment: &str,
        root: Option<&std::path::Path>,
        workspace: koru_core::WorkspaceId,
        keep: &[koru_core::DocumentId],
    ) -> Result<(), koru_store_core::errors::Error> {
        if keep.is_empty() {
            return self.open_from_store(segment, root, workspace);
        }
        if let Some(id) = self.active
            && keep.contains(&id)
        {
            self.park_active();
        }
        let (mut session, snapshot) =
            store::StoreSession::join_manifest(segment, root, explore::store_workspace(workspace))?;
        let mut manifest = koru_store_core::manifest::Manifest::from_snapshot(&snapshot)?;
        let client = match root {
            Some(root) => koru_api_client::Client::open_at(segment, root),
            None => koru_api_client::Client::open(segment),
        }?;
        self.replay_trouble
            .extend(session.replay_buffered(&mut manifest));
        self.close_store_sessions_except(keep);
        self.manifest = Some(manifest);
        if let Some(waker) = &self.waker {
            session.set_waker(waker.clone());
        }
        self.manifest_session = Some(session);
        self.store = Some((segment.to_owned(), root.map(|r| r.to_path_buf())));
        self.client = Some(client);
        self.current = Some(workspace);
        self.others.retain(|id, _| keep.contains(id));
        self.active = None;
        self.pending_documents.clear();
        self.tags_changed = true;
        self.lists_changed = true;
        self.discover_pages();
        Ok(())
    }

    /// Flush and close every live Store room owned by the current workspace.
    ///
    /// A workspace switch is a boundary for the whole set, not just its
    /// manifest. Buffer first so an acknowledgement racing the switch cannot
    /// make the last edit exist only in this process, then send the courteous
    /// leaves and drop the links. Dropping joins each socket thread, so every
    /// leave has been written when the caller continues. The Store reads them on
    /// its own time, though, and its occupancy view settles a moment later: a
    /// caller that must see these rooms gone waits for it, as the shell's
    /// delete of the workspace just left does.
    fn close_store_sessions(&mut self) {
        self.buffer_everything_owed();
        self.detach_pages();
        if let Some(session) = &self.manifest_session {
            session.leave();
        }
        for session in self.doc_sessions.values() {
            session.leave();
        }
        if let Some(system) = &self.system {
            system.leave();
        }
        self.manifest_session = None;
        self.doc_sessions.clear();
        self.system = None;
    }

    fn close_store_sessions_except(&mut self, keep: &[koru_core::DocumentId]) {
        self.buffer_everything_owed();
        self.detach_pages();
        if let Some(session) = &self.manifest_session {
            session.leave();
        }
        let drop: Vec<_> = self
            .doc_sessions
            .keys()
            .copied()
            .filter(|id| !keep.contains(id))
            .collect();
        for id in drop {
            if let Some(session) = self.doc_sessions.remove(&id) {
                session.leave();
            }
        }
        if let Some(system) = &self.system {
            system.leave();
        }
        self.manifest_session = None;
        self.system = None;
    }

    /// Create a document in this workspace and return its id.
    ///
    /// **Not the same as inserting a `Kind::Document` node.** That is what
    /// `create_document_after` does, and in a store-backed workspace it would
    /// put the new document *inside* whichever document is open — a nested page
    /// rather than a new one — because `ws` is the active document and no longer
    /// the whole workspace.
    ///
    /// The store registers it in the manifest, so the new document appears in
    /// [`entries`](Self::entries) immediately through a provisional local row.
    /// The authoritative manifest change still arrives over its socket; once it
    /// does, the provisional row is discarded rather than becoming a second
    /// collaborative manifest edit.
    pub fn create_document(
        &mut self,
        title: &str,
    ) -> Result<koru_core::DocumentId, koru_store_core::errors::Error> {
        self.create_document_after(title, None)
    }

    /// Create a document **as the next sibling of `after`**, and return its id.
    ///
    /// "Next to the page I was on" is what *New document* means — Ctrl+T from a
    /// document puts one below it, not at the bottom of a library of two
    /// hundred. `after` is normally the open document; `None` (a welcome tab, an
    /// empty workspace) appends, which is the only honest answer when there is
    /// no anchor.
    ///
    /// The store places it, rather than this creating it and moving it after:
    /// a move would be a second manifest edit racing the broadcast of the
    /// first, and every other client would watch the document appear at the end
    /// and jump
    /// ([ADR-122-place](../../../docs/decisions/ADR-101-125.md#adr-122-place--a-new-document-is-placed-by-the-store-and-ctrlaltletter-is-reachable-again)).
    pub fn create_document_after(
        &mut self,
        title: &str,
        after: Option<koru_core::DocumentId>,
    ) -> Result<koru_core::DocumentId, koru_store_core::errors::Error> {
        let placement = self.placement_after(after);
        let id = self.register_document_placed(
            title,
            placement,
            after,
            koru_core::explore::KIND_DOCUMENT,
        )?;
        self.seed_new_document(title, koru_core::explore::KIND_DOCUMENT);
        Ok(id)
    }

    /// Create a document as the **last child** of `parent` and return its id
    /// — a row's `+` in the sidebar (the review's note). Placed by the store
    /// under that parent, and shown there at once by the provisional entry,
    /// so the row does not appear at the root and then hop.
    pub fn create_document_in(
        &mut self,
        title: &str,
        parent: koru_core::DocumentId,
    ) -> Result<koru_core::DocumentId, koru_store_core::errors::Error> {
        let anchor = explore::store_document(parent);
        let index = self.manifest.as_ref().map_or(0, |manifest| {
            manifest
                .entries()
                .into_iter()
                .filter(|entry| entry.parent == Some(anchor))
                .count()
        });
        let id = self.register_document_placed(
            title,
            (Some(anchor), Some(index)),
            None,
            koru_core::explore::KIND_DOCUMENT,
        )?;
        if let Some(pending) = self.pending_documents.iter_mut().find(|p| p.id == id) {
            pending.parent = Some(parent);
        }
        self.seed_new_document(title, koru_core::explore::KIND_DOCUMENT);
        Ok(id)
    }

    /// Create a **canvas** in this workspace and return its id.
    ///
    /// The same registration a page gets, under the manifest kind
    /// [`KIND_CANVAS`](koru_core::explore::KIND_CANVAS), seeded with its root
    /// only — a canvas's content is what is put on it, and an empty paragraph
    /// under the root would be a card nobody made.
    ///
    /// **At the workspace root, at the end**, not beside the open document:
    /// canvases are a flat list of their own in Explore, and a canvas placed as
    /// a page's next sibling would carry a manifest parent that no section
    /// shows. `(None, None)` is the store's own append.
    pub fn create_canvas(
        &mut self,
        title: &str,
    ) -> Result<koru_core::DocumentId, koru_store_core::errors::Error> {
        let id = self.register_document_placed(
            title,
            (None, None),
            None,
            koru_core::explore::KIND_CANVAS,
        )?;
        self.seed_new_document(title, koru_core::explore::KIND_CANVAS);
        Ok(id)
    }

    /// Create a document **as a child of `parent`** in the explore tree, and
    /// return its id. What Shift+Enter in the move picker means: the extracted
    /// content becomes a sub-page of the page it left.
    pub fn create_document_under(
        &mut self,
        title: &str,
        parent: koru_core::DocumentId,
    ) -> Result<koru_core::DocumentId, koru_store_core::errors::Error> {
        let id = self.register_document_placed(
            title,
            (Some(explore::store_document(parent)), None),
            Some(parent),
            koru_core::explore::KIND_DOCUMENT,
        )?;
        self.seed_new_document(title, koru_core::explore::KIND_DOCUMENT);
        Ok(id)
    }

    /// The one note a file carries — created when it has none, answered either
    /// way. *Add note* and *Open note* are the same call; the store is
    /// idempotent, so a race with another window cannot mint a second.
    ///
    /// The store owns the whole change: the note, its manifest entry and the
    /// file's link to it land in one commit there, and this window's copy
    /// arrives over the manifest room. Drained opportunistically rather than
    /// awaited — the caller opens the note by id, which needs nothing from
    /// the listing.
    pub fn ensure_file_note(
        &mut self,
        file: koru_core::DocumentId,
    ) -> Result<koru_core::DocumentId, koru_store_core::errors::Error> {
        let workspace = self.current.ok_or_else(|| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::InvalidRequest,
                "this workspace is not open on a store",
            )
        })?;
        let note = self
            .client()?
            .ensure_file_note(
                explore::store_workspace(workspace),
                explore::store_document(file),
            )?
            .id;
        // Take whatever the manifest room has already said, on the chance the
        // store's broadcast has landed by now — the same opportunistic grab
        // `register_document_placed` makes, so the row's note icon does not
        // wait for the next poll.
        if let (Some(session), Some(manifest)) = (&mut self.manifest_session, &mut self.manifest)
            && session.drain(manifest).unwrap_or(false)
        {
            self.entries_changed = true;
        }
        Ok(explore::koru_document(note))
    }

    /// **A document the store made is empty, and Koru's is not.**
    ///
    /// The store registers a document and gives it a `LoroDoc` with nothing
    /// in it. A Koru document needs a `Kind::Document` root node — its text
    /// is the title, and it is row 0 of the body ([ADR-91]) — plus one empty
    /// paragraph to type into. Without them the document opens on nothing: no
    /// title row, no line, and nothing for a tab to show.
    ///
    /// Seeded here rather than in the store, which deliberately does not know
    /// what is inside a document ([ADR-80]): the bytes are opaque to it, and
    /// teaching it this shape would make it a second place that knows what a
    /// Koru document looks like.
    ///
    /// `kind` is the manifest kind it was registered under; a canvas gets the
    /// root alone ([`seed_document`]).
    ///
    /// [ADR-80]: ../../../docs/decisions/ADR-076-100.md#adr-80-manifest--a-workspace-is-a-manifest-document-plus-opaque-documents
    /// [ADR-91]: ../../../docs/decisions/ADR-076-100.md#adr-91-perdocument--a-document-keeps-a-root-node-for-its-title-and-the-manifest-mirrors-it
    fn seed_new_document(&mut self, title: &str, kind: &str) {
        if self.ws.walk().map(|w| w.is_empty()).unwrap_or(false)
            && seed_document(&mut self.ws, title, kind).is_some()
        {
            self.publish_active();
        }
    }

    /// The same seed as [`seed_new_document`](Self::seed_new_document), for a
    /// document that was registered **empty** and is only now being opened.
    ///
    /// A file's note is that document: the store registers it with no content
    /// — it deliberately does not know what is inside one (ADR-80) — and no
    /// creator seeds it, so the first open is where the root node and its one
    /// paragraph appear. The title is read from the manifest rather than
    /// guessed: a note's title is its file's, and writing anything else would
    /// be mirrored back over the manifest by `reconcile_title`. Nothing is
    /// seeded for a document the manifest does not name, and never for a
    /// file — an empty *file* is damaged, not unseeded, and inventing a page
    /// root for it would hide that.
    ///
    /// **First it heals.** The manifest entry syncs apart from the document,
    /// so two devices can each find a note empty and each seed it; the merge
    /// keeps both seeds, because there is no mergeable `LoroTree::create`. A
    /// document with two `Kind::Document` roots is not empty, so the guard
    /// below would skip it and leave the split standing — the fold runs ahead
    /// of it. That heals `create_document` pages too: their race is narrower,
    /// not absent, and every open comes through here.
    ///
    /// Returns whether it wrote anything, so the caller publishes — the seed
    /// must reach the store in the same turn, or a peer opening a beat later
    /// seeds its own and the merge keeps both.
    fn seed_empty_document(&mut self, id: koru_core::DocumentId, doc: &mut WorkspaceDoc) -> bool {
        if doc.unify_document_roots().unwrap_or(false) {
            // The fold left exactly one root, so the document is no longer
            // empty to seed — and the repair has to reach the store now, or
            // the next opener folds its own copy and publishes a racing one.
            return true;
        }
        if !doc.walk().map(|w| w.is_empty()).unwrap_or(false) {
            return false;
        }
        let Some(manifest) = &self.manifest else {
            return false;
        };
        let Some(entry) = manifest
            .entries()
            .into_iter()
            .find(|entry| explore::koru_document(entry.id) == id)
        else {
            return false;
        };
        if entry.kind == koru_core::explore::KIND_FILE {
            return false;
        }
        // A canvas registered empty gets its root and nothing under it, the
        // same seed `create_canvas` writes — see [`seed_document`].
        (|| {
            seed_document(doc, &entry.title, &entry.kind)?;
            doc.stamp_schema_version().ok()?;
            Some(())
        })()
        .is_some()
    }

    /// Copy a document of this workspace into a new one, and open it.
    ///
    /// **The other way out of the *In Trash* notice.** Restoring argues with the
    /// peer who binned it; this does not — you keep your version, they keep
    /// their decision, and the two stop being the same document.
    ///
    /// **Every node in the copy is a new node.** Not a snapshot import, which is
    /// the cheap answer and the wrong one: it would carry every `NodeId` across,
    /// so the two documents would name the same nodes and the search index —
    /// keyed by `(profile, workspace, node)` — would have two rows fighting over
    /// one key. `WorkspaceDoc::cutting` walks instead and `plant` mints fresh
    /// ids, carrying kinds, meta, text, marks, attributes and shape.
    ///
    /// **Images need nothing.** A picture is a `b3:` address into the store's
    /// content-addressed blob store ([ADR-93]), so the copy references the same
    /// bytes and there is nothing to duplicate.
    ///
    /// [ADR-93]: ../../../docs/decisions/ADR-076-100.md#adr-93-blobhash--the-store-owns-a-blobs-address-and-the-app-stops-hashing
    pub fn copy_document(
        &mut self,
        source: koru_core::DocumentId,
        title: &str,
    ) -> Result<koru_core::DocumentId, koru_store_core::errors::Error> {
        // **Cut first.** `register_document` opens the new document, which parks
        // the source and makes `ws` the empty one being written into — so a read
        // after it would read the wrong document, and find nothing.
        let cutting = {
            let doc = self.document(source).ok_or_else(|| {
                koru_store_core::errors::Error::new(
                    koru_store_core::errors::Code::InvalidRequest,
                    "that document is not open, so there is nothing to copy",
                )
            })?;
            let root = document_root(doc).ok_or_else(|| {
                koru_store_core::errors::Error::new(
                    koru_store_core::errors::Code::InvalidRequest,
                    "that document has no content to copy",
                )
            })?;
            doc.cutting(root).map_err(|e| {
                koru_store_core::errors::Error::new(
                    koru_store_core::errors::Code::Internal,
                    format!("copying that document: {e}"),
                )
            })?
        };

        // Registered without a seed, because the cutting brings its own root: a
        // seeded one would sit beside it as a second `Kind::Document` child, and
        // `document_root` answers with whichever it walks into first.
        //
        // Normally beside the source. An inherited-trash child is the
        // exception: beside it means beneath the same trashed ancestor, so the
        // new copy is invisible the instant it is created. Put that copy at the
        // workspace root, which is the explicit escape the notice promises.
        let after = self
            .manifest
            .as_ref()
            .and_then(|manifest| {
                manifest
                    .entries()
                    .into_iter()
                    .find(|entry| entry.id == explore::store_document(source))
                    .and_then(|entry| entry.parent)
                    .map(|parent| !manifest.is_trashed(parent))
            })
            .unwrap_or(true)
            .then_some(source);
        // **The copy is what the source is.** A canvas copied as a page would
        // open as a page of loose cards and be listed among the documents; the
        // kind travels with the content. A source the manifest does not name
        // is a page — the only kind a workspace without one has.
        let kind = self
            .document_kind(source)
            .unwrap_or_else(|| koru_core::explore::KIND_DOCUMENT.to_owned());
        let id = self.register_document(title, after, &kind)?;
        self.ws
            .plant(&cutting, koru_core::schema::Parent::Root, title)
            .map_err(|e| {
                koru_store_core::errors::Error::new(
                    koru_store_core::errors::Code::Internal,
                    format!("writing the copy: {e}"),
                )
            })?;
        self.publish_active();
        Ok(id)
    }

    /// Put a selection's content at the **top** of another document — the
    /// active one, an open tab, or one this window has never opened.
    ///
    /// The target-side half of *moving a selection to another document*: the
    /// caller took [`cutting`](WorkspaceDoc::cutting)s (and, for a partial
    /// row, runs) from the source, and this plants them first under `under`
    /// (a heading in the target) or the target's document root. Nothing here
    /// touches the source — cuttings are copies, so a failure anywhere leaves
    /// at worst duplicated content and never lost content; deleting the source
    /// selection is the caller's move *after* this returns `Ok`.
    ///
    /// Three routes, one per way the target can be held:
    /// - the **active** document is written directly and published;
    /// - an **open tab** is written through its parked [`WorkspaceDoc`] and its
    ///   own session, so a second replica is not raced against the tab's;
    /// - anything else joins over the store — see
    ///   [`plant_into_foreign_document`], which the caller should run off the
    ///   event loop because durability is waited on.
    pub fn plant_into_document(
        &mut self,
        workspace: koru_core::WorkspaceId,
        document: koru_core::DocumentId,
        under: Option<NodeId>,
        cuttings: &[(koru_core::schema::Cutting, Option<koru_core::ListStyle>)],
        inline: Option<(&[koru_core::TextRun], &[koru_core::node::Attr])>,
    ) -> Result<(), koru_store_core::errors::Error> {
        if self.current == Some(workspace) {
            if self.active == Some(document) {
                plant_at_top(&mut self.ws, under, cuttings, inline)?;
                self.publish_active();
                return Ok(());
            }
            if let Some(doc) = self.others.get_mut(&document) {
                plant_at_top(doc, under, cuttings, inline)?;
                if let Some(session) = self.doc_sessions.get_mut(&document) {
                    session.publish(doc)?;
                }
                return Ok(());
            }
        }
        let (segment, root) = self.store.clone().ok_or_else(|| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::InvalidRequest,
                "this workspace has no store to reach that document through",
            )
        })?;
        plant_into_foreign_document(
            &segment,
            root.as_deref(),
            workspace,
            document,
            under,
            cuttings,
            inline,
        )
    }

    /// Send the active document to the store now, rather than at the next tick.
    ///
    /// So a document that has just been made exists for everyone else the moment
    /// it exists here. A workspace with no session for it — no store — is a
    /// no-op, which is what the editor's own suite runs as.
    /// Answers whether the session took the publish. `false` on a stopped or
    /// refusing session — the write is local and the outbox will carry it —
    /// and `false` when there is no session to publish to. Most callers do not
    /// care; the task road does, because a receipt that named a write the
    /// store never got would be waited on in vain.
    fn publish_active(&mut self) -> bool {
        if let Some(id) = self.active
            && let Some(session) = self.doc_sessions.get_mut(&id)
        {
            return session.publish(&self.ws).is_ok();
        }
        false
    }

    /// Register a document with the store and open it, **leaving it empty**.
    ///
    /// The half [`create_document`](Self::create_document) and
    /// [`copy_document`](Self::copy_document) share. Split out rather than given
    /// a `seed: bool`, because what goes into a new document is a paragraph in
    /// one case and a whole subtree in the other, and neither is a flag.
    fn register_document(
        &mut self,
        title: &str,
        after: Option<koru_core::DocumentId>,
        kind: &str,
    ) -> Result<koru_core::DocumentId, koru_store_core::errors::Error> {
        let placement = self.placement_after(after);
        self.register_document_placed(title, placement, after, kind)
    }

    /// [`register_document`], with the explore-tree placement already decided:
    /// `(parent, index)` in the store's coordinates, and `near` only naming
    /// the provisional row's neighbour until the manifest broadcast lands.
    ///
    /// `kind` is the manifest kind token the store records — a page's
    /// [`KIND_DOCUMENT`](koru_core::explore::KIND_DOCUMENT) or a canvas's
    /// [`KIND_CANVAS`](koru_core::explore::KIND_CANVAS) — and the provisional
    /// row carries it too, so nothing lists the new document as the wrong kind
    /// even for the moment before the manifest echo.
    ///
    /// [`register_document`]: Self::register_document
    fn register_document_placed(
        &mut self,
        title: &str,
        (parent, index): (Option<koru_store_core::ids::DocumentId>, Option<usize>),
        near: Option<koru_core::DocumentId>,
        kind: &str,
    ) -> Result<koru_core::DocumentId, koru_store_core::errors::Error> {
        let workspace = self.current.ok_or_else(|| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::InvalidRequest,
                "this workspace is not open on a store",
            )
        })?;
        let (_segment, _root) = self.store.clone().ok_or_else(|| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::InvalidRequest,
                "this workspace has no store to create a document in",
            )
        })?;
        let client = self.client()?;
        let id = client.create_document_at(
            explore::store_workspace(workspace),
            title,
            kind,
            parent,
            index,
        )?;

        // Take whatever the manifest room has already said, on the chance the
        // store's broadcast has landed by now.
        //
        // **It usually has not, and that is fine.** The provisional entry below
        // makes the HTTP result visible immediately without minting a competing
        // CRDT tree node. This remains an opportunistic grab that can replace
        // the provisional row before anybody observes it.
        if let (Some(session), Some(manifest)) = (&mut self.manifest_session, &mut self.manifest) {
            let _ = session.drain(manifest);
        }
        let id = explore::koru_document(id);
        if !self.pending_documents.iter().any(|p| p.id == id) {
            self.pending_documents.push(Pending {
                id,
                title: title.to_owned(),
                after: near,
                parent: None,
                kind: kind.to_owned(),
            });
            self.entries_changed = true;
        }
        // The store has the document from here on, whatever happens next:
        // a failed open is not *nothing was changed*, and the message says
        // so rather than letting the window claim the opposite.
        self.open_document(id).map_err(|e| {
            koru_store_core::errors::Error::new(
                e.code(),
                format!("{CREATED_BUT_NOT_OPENED}: {e}"),
            )
        })?;
        // **The schema marker, on the document the store made.**
        //
        // `WorkspaceDoc::create` writes it and nothing here calls `create`: the
        // store hands out an empty `LoroDoc` and the seeds below fill it, so
        // every document made since the cutover records no version at all. The
        // gate that keeps an older build from re-exporting a newer document
        // reads that field, and MCP publishes it; the second is how the first
        // was noticed. Stamped here rather than in either caller, because
        // `create_document_after` and `copy_document` both come through here
        // and only one of them seeds.
        self.ws.stamp_schema_version().map_err(|e| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::Internal,
                format!("stamping the document schema version: {e}"),
            )
        })?;
        Ok(id)
    }

    /// Where a document made from `after` goes: **under that document's parent,
    /// one place below it**.
    ///
    /// The pair the store's create takes. `(None, None)` — the workspace root,
    /// at the end — whenever there is no anchor to be next to: no `after`, no
    /// manifest, or an `after` this replica has never heard of. Appending is the
    /// only answer that cannot be wrong, and a document nobody can point at is
    /// exactly the case where guessing a position would be.
    ///
    /// `ordinal` counts children of the parent in the manifest tree, trashed
    /// ones included, which is the coordinate the store's index means — the
    /// same `+ 1` a drop below a row in Explore computes.
    fn placement_after(
        &self,
        after: Option<koru_core::DocumentId>,
    ) -> (Option<koru_store_core::ids::DocumentId>, Option<usize>) {
        let Some(after) = after else {
            return (None, None);
        };
        let Some(manifest) = self.manifest.as_ref() else {
            return (None, None);
        };
        let anchor = explore::store_document(after);
        match manifest.entries().into_iter().find(|e| e.id == anchor) {
            Some(entry) => (entry.parent, Some(entry.ordinal as usize + 1)),
            None => (None, None),
        }
    }

    /// Move a document (and its explore subtree) into another workspace.
    ///
    /// `source` may differ from the open workspace (undo reverse-transfer).
    /// Publishes the active document first when it is among the moving set.
    /// Document ids are preserved by the store.
    pub fn transfer_document(
        &mut self,
        id: koru_core::DocumentId,
        source: koru_core::WorkspaceId,
        target: koru_core::WorkspaceId,
        parent: Option<koru_core::DocumentId>,
        index: usize,
    ) -> Result<Vec<koru_core::DocumentId>, koru_store_core::errors::Error> {
        if source == target {
            return Err(koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::InvalidRequest,
                "transfer is for a different workspace; use move within one",
            ));
        }
        let client = self.prepare_document_transfer()?;
        let ids = client.transfer_documents(
            explore::store_workspace(source),
            explore::store_workspace(target),
            explore::store_document(id),
            parent.map(explore::store_document),
            index,
        )?;
        self.finish_document_transfer(source, &ids);
        Ok(ids.into_iter().map(explore::koru_document).collect())
    }

    /// Flush local edits and return the already-authenticated client for a
    /// cross-workspace transfer performed off the event-loop thread.
    ///
    /// Preparation stays on the owner thread because publishing reads the live
    /// document. The returned HTTP client is cheap to clone and `Send`, so the
    /// request itself can wait on the Store without freezing the window.
    pub fn prepare_document_transfer(
        &mut self,
    ) -> Result<koru_api_client::Client, koru_store_core::errors::Error> {
        self.publish_active();
        self.client()
    }

    /// Adopt a completed Store transfer into this window's local open-tab
    /// state. `ids` use Store spelling because that is exactly what the worker
    /// receives from the transfer endpoint.
    pub fn finish_document_transfer(
        &mut self,
        source: koru_core::WorkspaceId,
        ids: &[koru_store_core::ids::DocumentId],
    ) {
        // If we were on the source, the listing no longer holds these.
        if self.current == Some(source) {
            for moved in ids {
                let kid = explore::koru_document(*moved);
                self.doc_sessions.remove(&kid);
                self.others.remove(&kid);
                if self.active == Some(kid) {
                    self.active = None;
                }
            }
            self.entries_changed = true;
        }
    }

    /// List documents in any workspace this profile owns, without switching.
    pub fn list_workspace_documents(
        &mut self,
        workspace: koru_core::WorkspaceId,
    ) -> Result<Vec<koru_core::explore::ExploreEntry>, koru_store_core::errors::Error> {
        let client = self.client()?;
        let entries = client.documents(explore::store_workspace(workspace))?;
        Ok(entries.into_iter().map(explore_entry).collect())
    }

    /// Move a document among the workspace's entries.
    ///
    /// `index` counts **entries**, not tree children — the manifest translates,
    /// the same distinction `WorkspaceDoc::move_entry` makes for the projection.
    ///
    /// A cycle is refused by Loro and leaves the tree as it was, so there is no
    /// guard here; the manifest answers with its own error code.
    pub fn move_entry(
        &mut self,
        id: koru_core::DocumentId,
        parent: Option<koru_core::DocumentId>,
        index: usize,
    ) -> Result<bool, koru_store_core::errors::Error> {
        let manifest = self.manifest.as_mut().ok_or_else(|| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::InvalidRequest,
                "this workspace is not open on a store",
            )
        })?;
        // **Past the end means the end, and the clamp is ours.**
        //
        // `WorkspaceDoc::move_entry` accepts an out-of-range index and lands the
        // document last, because a drop below everything is an ordinary gesture.
        // `Manifest::move_document` hands the index straight to `mov_to`, which
        // refuses one past the length — so `usize::MAX` (what "drop at the end"
        // is spelled as here) came back as
        // *"The index(18446744073709551615) should be <= the length"*.
        //
        // Clamped here rather than by loosening the manifest: a store API that
        // silently accepts any index cannot tell a deliberate append from an
        // arithmetic mistake, and this crate already owns the forgiving version
        // of this contract.
        //
        // The moving document is excluded from the count when it is already
        // under `parent`, because `mov_to` reads the index after lifting it out.
        let siblings = manifest
            .entries()
            .into_iter()
            .filter(|e| {
                e.parent.map(explore::koru_document) == parent && explore::koru_document(e.id) != id
            })
            .count();
        // **A drop naming a document the manifest does not hold is stale, not
        // broken.** Explore draws from a *cached* list the shell pushed; between
        // that push and the drop, the document may have been moved or binned in
        // another window, or its entry may not have arrived here yet — creating
        // one is an HTTP call whose manifest change comes back over the socket.
        //
        // Answering that with a modal is what produced *"Couldn't move that
        // document"* for a gesture nobody could reproduce: the list was simply
        // out of date, and the honest response is to correct it rather than to
        // interrupt somebody. `Ok(false)` says *nothing to do*; the shell
        // re-pushes and the row lands where the manifest says it is.
        let moving = explore::store_document(id);
        let target = parent.map(explore::store_document);
        if !manifest.contains(moving) || target.is_some_and(|p| !manifest.contains(p)) {
            return Ok(false);
        }
        // A move into its own subtree, or onto itself, is meaningless rather
        // than faulty. The drag cannot normally offer one — a carried root's
        // descendants leave the row cache for the duration — so this is the
        // belt to that braces, and it snaps back in silence like any other drop
        // that landed nowhere.
        if target == Some(moving) {
            return Ok(false);
        }
        manifest.move_document(moving, target, index.min(siblings))?;
        // Push it now rather than on the next poll: a drag that snapped back for
        // a moment would read as the drop having failed.
        if let Some(session) = &mut self.manifest_session {
            let _ = session.publish(manifest);
        }
        Ok(true)
    }

    /// This workspace's store client, built once and kept.
    fn client(&mut self) -> Result<koru_api_client::Client, koru_store_core::errors::Error> {
        if let Some(c) = &self.client {
            return Ok(c.clone());
        }
        let (segment, root) = self.store.clone().ok_or_else(|| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::InvalidRequest,
                "this workspace is not open on a store",
            )
        })?;
        let client = match &root {
            Some(root) => koru_api_client::Client::open_at(&segment, root),
            None => koru_api_client::Client::open(&segment),
        }?;
        self.client = Some(client.clone());
        Ok(client)
    }

    /// Whether this workspace is backed by the store.
    ///
    /// Asked by call sites that have both paths while the folder world is still
    /// alive. It goes when the folder world does.
    pub fn on_store(&self) -> bool {
        self.manifest.is_some()
    }

    /// The already-discovered Store client for read-only side panels and
    /// background commands. Cloning it reuses the HTTP pool and credential; it
    /// performs no discovery and opens no document.
    pub fn store_client(&self) -> Option<koru_api_client::Client> {
        self.client.clone()
    }

    /// Make `id` the active document, fetching it from the store if it is not
    /// already open.
    ///
    /// The previously active document is kept — it is another tab, not something
    /// to throw away — which is what [`others`](Self::others) is for.
    pub fn open_document(
        &mut self,
        id: koru_core::DocumentId,
    ) -> Result<(), koru_store_core::errors::Error> {
        let workspace = self.current.ok_or_else(|| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::InvalidRequest,
                "this workspace is not open on a store",
            )
        })?;
        self.open_document_in(workspace, id)
    }

    pub fn open_document_in(
        &mut self,
        workspace: koru_core::WorkspaceId,
        id: koru_core::DocumentId,
    ) -> Result<(), koru_store_core::errors::Error> {
        if self.active == Some(id) {
            return Ok(());
        }
        let _current = self.current.ok_or_else(|| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::InvalidRequest,
                "this workspace is not open on a store",
            )
        })?;

        // Already an open tab: swap it in without asking the store again.
        if let Some(doc) = self.others.remove(&id) {
            self.park_active();
            self.ws = doc;
            // Its `UndoManager` came with it — it is bound to the `LoroDoc`, and
            // parking a tab never touched the document. So a tab returned to
            // keeps the history it had, which is the behaviour a person expects
            // and the reason not to re-enable it here (that would discard it).
            self.active = Some(id);
            return Ok(());
        }

        let where_to = self.store.clone();
        let (mut session, snapshot) = match where_to {
            Some((segment, root)) => store::StoreSession::join(
                &segment,
                root.as_deref(),
                explore::store_workspace(workspace),
                explore::store_document(id),
            )?,
            None => {
                return Err(koru_store_core::errors::Error::new(
                    koru_store_core::errors::Code::InvalidRequest,
                    "this workspace has no store to open a document from",
                ));
            }
        };
        let mut doc = WorkspaceDoc::from_snapshot(&snapshot).map_err(|e| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::CorruptState,
                format!("document {id:?}: {e}"),
            )
        })?;
        // **Before anything is published, and before the tab is swapped in.**
        // The snapshot is what the store has; whatever this machine buffered
        // while the store was away is not in it, and this is the one moment the
        // two can be put back together.
        self.replay_trouble
            .extend(session.replay_buffered(&mut doc));
        // **The workspace this document is in, before undo is switched on.**
        //
        // `WorkspaceDoc::create` writes the id and nothing on this path calls
        // `create`: the store hands out an empty `LoroDoc`, so every document
        // ever opened here answered `None`. Step 0 of every canonical task
        // command's preflight is that read, so `/due` on real content refused
        // with "the document carries no workspace id" — silently, because the
        // editor discarded the error. `workspace` is the store's own answer for
        // where this document lives; nothing here derives or invents one.
        //
        // **Before `enable_undo`, and that ordering is the whole of it.** A
        // stamp is a CRDT op like any other, so with undo already on it becomes
        // the freshly opened document's first undoable step — and the person's
        // first Ctrl+Z would silently take the id back off, putting the document
        // straight back into the state this line exists to end.
        // `each_document_has_its_own_undo_history` is what says so.
        doc.stamp_workspace_id(workspace).map_err(|e| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::Internal,
                format!("stamping the document workspace id: {e}"),
            )
        })?;
        // **And which document it is**, from the same place and in the same
        // order, for the same kind of reason. The store hands out an empty
        // `LoroDoc`, so nothing inside it knows the id the manifest lists it
        // under — and that id is the middle segment of every `task:` reference
        // minted in this document. Derived from the node instead, the segment
        // was a different value for every row on one page; `id` here is the
        // store's own answer, and nothing on this path invents one.
        doc.stamp_document_id(id).map_err(|e| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::Internal,
                format!("stamping the document id: {e}"),
            )
        })?;
        // **After the replay**, so a seed this machine buffered while offline
        // lands first and the document is no longer empty to seed.
        let seeded = self.seed_empty_document(id, &mut doc);
        self.park_active();
        self.ws = doc;
        // **Undo is per document, and this is where it becomes true.**
        //
        // `loro::UndoManager` is bound to a `LoroDoc`, so splitting documents
        // splits the history whether anyone asks for it or not — a tab's Ctrl+Z
        // now reaches only what was typed in *that* tab, which is arguably where
        // it always belonged.
        //
        // What is not automatic is that a newly opened document **has** one.
        // `enable_undo` is off by default so a headless consumer pays nothing
        // for it, and `App` calls it once at startup on whatever document was
        // open then. Without this line every document opened afterwards would
        // silently refuse Ctrl+Z — no error, no history, just nothing happening.
        self.ws.enable_undo();
        self.active = Some(id);
        // Inherited, so a document opened later wakes the loop like the rest.
        if let Some(waker) = &self.waker {
            session.set_waker(waker.clone());
        }
        self.doc_sessions.insert(id, session);
        // A just-seeded document goes to the store now, not at the next tick:
        // the whole point of seeding on open is that nobody else ever opens it
        // empty (see `seed_empty_document`).
        if seeded {
            self.publish_active();
        }
        Ok(())
    }

    /// Load `id` into the parked-tabs map without making it active.
    ///
    /// Returns the document's home workspace (from Store locate when available).
    pub fn ensure_document(
        &mut self,
        id: koru_core::DocumentId,
    ) -> Result<koru_core::WorkspaceId, koru_store_core::errors::Error> {
        if self.active == Some(id) || self.others.contains_key(&id) {
            // Prefer a live locate so a background tab stamped under the wrong
            // window workspace can still learn its real home.
            let located = self.store_client().and_then(|client| {
                client
                    .locate_document(explore::store_document(id))
                    .ok()
                    .map(|location| koru_core::WorkspaceId::from_uuid(location.workspace.as_uuid()))
            });
            return Ok(located
                .or(self.current)
                .unwrap_or_else(koru_core::WorkspaceId::new));
        }
        // Locate first — the same catalog click uses. Joining `self.current`
        // with the pill's id 404s when the document lives in another workspace
        // or the pill still carries a pre-move id.
        let located = self.store_client().and_then(|client| {
            client
                .locate_document(explore::store_document(id))
                .ok()
                .map(|location| koru_core::WorkspaceId::from_uuid(location.workspace.as_uuid()))
        });
        let workspace = located.or(self.current).ok_or_else(|| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::InvalidRequest,
                "this workspace is not open on a store",
            )
        })?;
        let where_to = self.store.clone();
        let joined = match where_to {
            Some((segment, root)) => store::StoreSession::join(
                &segment,
                root.as_deref(),
                explore::store_workspace(workspace),
                explore::store_document(id),
            ),
            None => {
                return Err(koru_store_core::errors::Error::new(
                    koru_store_core::errors::Code::InvalidRequest,
                    "this workspace has no store to open a document from",
                ));
            }
        };
        match joined {
            Ok((mut session, snapshot)) => {
                let mut doc = WorkspaceDoc::from_snapshot(&snapshot).map_err(|e| {
                    koru_store_core::errors::Error::new(
                        koru_store_core::errors::Code::CorruptState,
                        format!("document {id:?}: {e}"),
                    )
                })?;
                self.replay_trouble
                    .extend(session.replay_buffered(&mut doc));
                // The parked half of the same guarantee, in the same order and
                // for the same reason — see `open_document_in`. A background tab
                // brought forward is swapped in by the already-open fast path,
                // which re-enables nothing, so a tab that opened here without an
                // id would never gain one.
                doc.stamp_workspace_id(workspace).map_err(|e| {
                    koru_store_core::errors::Error::new(
                        koru_store_core::errors::Code::Internal,
                        format!("stamping the document workspace id: {e}"),
                    )
                })?;
                // The parked half of the document stamp too — see
                // `open_document_in`. A background tab is where a burst is
                // flushed from as often as anywhere else, and a reference minted
                // there has to be the same string the foreground would mint.
                doc.stamp_document_id(id).map_err(|e| {
                    koru_store_core::errors::Error::new(
                        koru_store_core::errors::Code::Internal,
                        format!("stamping the document id: {e}"),
                    )
                })?;
                let seeded = self.seed_empty_document(id, &mut doc);
                doc.enable_undo();
                if let Some(waker) = &self.waker {
                    session.set_waker(waker.clone());
                }
                self.doc_sessions.insert(id, session);
                if seeded && let Some(session) = self.doc_sessions.get_mut(&id) {
                    let _ = session.publish(&doc);
                }
                self.others.insert(id, doc);
                Ok(workspace)
            }
            Err(_) => {
                // Click can open it, so park it the same way and switch back.
                let previous = self.active;
                self.open_document(id)?;
                if let Some(previous) = previous {
                    self.open_document(previous)?;
                }
                Ok(workspace)
            }
        }
    }

    /// Move the active document into the open-tabs map, if it is one.
    fn park_active(&mut self) {
        if let Some(was) = self.active {
            // A click can switch documents before the maintenance turn that
            // would normally publish the last keystroke. Once parked, this
            // session only drains, so send the outgoing document and reconcile
            // its title while it is still the active `WorkspaceDoc`.
            self.entries_changed |= self.reconcile_title(was);
            if let Some(session) = self.doc_sessions.get_mut(&was) {
                let _ = session.publish(&self.ws);
            }
        }
        if let Some(was) = self.active.take() {
            let parked = std::mem::replace(
                &mut self.ws,
                WorkspaceDoc::create(koru_core::WorkspaceId::new(), "")
                    .expect("an empty document is always constructible"),
            );
            self.others.insert(was, parked);
        }
    }

    pub fn current(&self) -> Option<koru_core::WorkspaceId> {
        self.current
    }

    /// Tell this workspace how to wake the event loop.
    ///
    /// Installed on every session it already has, and inherited by every one it
    /// makes afterwards — a document opened later, a workspace switched to. That
    /// inheritance is the whole reason it is held here rather than passed to
    /// `StoreSession::join`: a session is built in half a dozen places, and one
    /// that forgot the waker would be a document whose remote edits arrived only
    /// when something *else* happened to wake the window.
    pub fn set_store_waker(&mut self, waker: store::link::Waker) {
        for session in self
            .doc_sessions
            .values()
            .chain(self.manifest_session.as_ref())
        {
            session.set_waker(waker.clone());
        }
        if let Some(system) = &mut self.system {
            system.set_waker(waker.clone());
        }
        self.wake_pages_with(&waker);
        self.waker = Some(waker);
    }

    /// Edits handed to the store and not yet confirmed on its disk — across
    /// every open document **and** the manifest.
    ///
    /// **The only honest answer to "is my work safe".** A local read is not one:
    /// `entries()` and `ws` show what this replica believes, which is true the
    /// instant it is typed and says nothing about whether anybody else will ever
    /// see it. This counts what is still owed, and zero is the only value that
    /// means the window could close without losing something.
    ///
    /// It is also what a test has to wait on. Waiting on a local read passes
    /// while the bytes are still in a channel — see the rule in `AGENTS.md`
    /// about asserting on acknowledgements rather than on what was sent.
    pub fn owed(&self) -> usize {
        let ordinary: usize = self
            .doc_sessions
            .values()
            .chain(self.manifest_session.as_ref())
            .map(store::StoreSession::unacknowledged)
            .sum();
        ordinary
            + self
                .system
                .as_ref()
                .map(system::SystemSession::unacknowledged)
                .unwrap_or(0)
    }

    /// Whether this window is reaching the store, and what it is owed if not.
    ///
    /// **Worded for the content, not for the socket.** The question somebody has
    /// is *is my work safe*, and the honest answer combines two things: whether
    /// anything is getting through, and how many edits the store has not
    /// confirmed. A window that reconnected a moment ago and owes nothing is
    /// fine; one that has been reconnecting for a minute with eleven edits owed
    /// is not, and the difference is not visible from the socket alone.
    ///
    /// `None` when every session is live. Otherwise the *worst* — a session that
    /// has stopped outranks one that is retrying — because one document failing
    /// to save is the whole window failing to save as far as anybody cares.
    pub fn store_trouble(&self) -> Option<StoreTrouble> {
        let mut fatal: Option<String> = None;
        // The *earliest* start, because a link that has been down longest is the
        // one worth naming — and the one the escalation threshold is measured
        // against.
        let mut since: Option<(std::time::Instant, String)> = None;

        for session in self
            .doc_sessions
            .values()
            .chain(self.manifest_session.as_ref())
        {
            if let Some(why) = session.stopped() {
                fatal.get_or_insert_with(|| why.to_owned());
                continue;
            }
            match session.link_state() {
                store::link::LinkState::Live => {}
                store::link::LinkState::Stopped { why } => {
                    fatal.get_or_insert(why);
                }
                store::link::LinkState::Reconnecting { since: at, why } => {
                    if since.as_ref().is_none_or(|(earlier, _)| at < *earlier) {
                        since = Some((at, why));
                    }
                }
            }
        }
        if let Some(system) = &self.system {
            if let Some(why) = system.open_failure() {
                fatal.get_or_insert_with(|| why.to_owned());
            }
            for session in system.sessions() {
                if let Some(why) = session.stopped() {
                    fatal.get_or_insert_with(|| why.to_owned());
                    continue;
                }
                match session.link_state() {
                    store::link::LinkState::Live => {}
                    store::link::LinkState::Stopped { why } => {
                        fatal.get_or_insert(why);
                    }
                    store::link::LinkState::Reconnecting { since: at, why } => {
                        if since.as_ref().is_none_or(|(earlier, _)| at < *earlier) {
                            since = Some((at, why));
                        }
                    }
                }
            }
        }

        let owed = self.owed();
        // **Only once there is trouble to describe.** `unconfirmed` asks each
        // open document for its version, and this runs on every tick of every
        // window forever — a healthy one must pay nothing for a state it is not
        // in. Both arms below are already the unhappy path.
        if let Some(message) = fatal {
            return Some(StoreTrouble {
                message,
                owed,
                since: None,
                fatal: true,
                unconfirmed: self.unconfirmed(),
            });
        }
        let unconfirmed = since.is_some().then(|| self.unconfirmed());
        since.map(|(at, message)| StoreTrouble {
            message,
            owed,
            since: Some(at),
            fatal: false,
            unconfirmed: unconfirmed.unwrap_or_default(),
        })
    }

    /// Let a document go: leave its room, drop its session, forget its replica.
    ///
    /// **Not optional book-keeping any more.** While a session was a socket and
    /// a 1 ms poll, never pruning `doc_sessions` cost a poll per document ever
    /// opened — wasteful and invisible. A session now owns a *thread*, so the
    /// same omission leaks one of those per tab for as long as the window is up.
    ///
    /// The active document is not closeable: something has to be under the
    /// caret, and closing the last tab opens a welcome one rather than leaving
    /// none. A caller asking anyway is answered with `false` rather than an
    /// error, because "that one is not open" is not a failure.
    pub fn close_document(&mut self, id: koru_core::DocumentId) -> bool {
        if self.active == Some(id) {
            return false;
        }
        // A page document whose registration the registry still owes is
        // handed back to it rather than dropped — its writing exists only
        // here until the store confirms it.
        if self.take_page_back(id) {
            return true;
        }
        // **Correctness, and the case the outbox is easiest to get wrong.**
        // Closing a tab drops its session, and with it everything that knew
        // work was owed on that document. If the store has not confirmed it,
        // this is the last moment anything can write it down.
        if let Some(doc) = self.others.get(&id)
            && let Some(session) = self.doc_sessions.get_mut(&id)
        {
            session.buffer_now(doc);
        }
        // Politeness, not correctness: the store drops a connection's rooms
        // when it goes, and the socket is about to. It saves the store from
        // relaying to somebody who has stopped listening in the meantime.
        if let Some(session) = self.doc_sessions.get(&id) {
            session.leave();
        }
        // Dropping the session joins its thread, so this is where the cost is
        // reclaimed rather than merely forgotten.
        let had_session = self.doc_sessions.remove(&id).is_some();
        let had_doc = self.others.remove(&id).is_some();
        had_session || had_doc
    }

    // ------------------------------------------------- several documents
    //
    // A workspace is one `WorkspaceDoc` today and a set of them shortly. These
    // are the calls that will not change their meaning when it is — so a caller
    // written against them now keeps working, and a caller written against
    // `ws` keeps meaning "the document under the caret".

    /// Which document is being edited, if this workspace tells them apart yet.
    pub fn active(&self) -> Option<koru_core::DocumentId> {
        self.active
    }

    /// The workspace that owns the active document.
    ///
    /// This can differ from [`current`](Self::current): tabs are window-scoped,
    /// so selecting one does not switch Explore. The live Store session is the
    /// authority because it was joined with the document's catalog location;
    /// the embedded document stamp can still name a workspace it moved from.
    pub fn active_workspace(&self) -> Option<koru_core::WorkspaceId> {
        self.active
            .and_then(|document| self.doc_sessions.get(&document))
            .map(|session| koru_core::WorkspaceId::from_uuid(session.workspace().as_uuid()))
            .or_else(|| self.ws.id())
            .or(self.current)
    }

    /// What this workspace holds, in the order Explore draws it.
    ///
    /// **Answered by the manifest when there is one, and projected from the
    /// tree otherwise.** Both give tree order, a parent and an ordinal — that
    /// they agree is the whole reason `ExploreEntry` was built field-for-field
    /// as the store's `DocumentEntry`. So the day the manifest arrives, the
    /// explore pane, the tab strip and the `+` menu change nothing.
    pub fn entries(&self) -> Vec<koru_core::explore::ExploreEntry> {
        self.entries_and_trashed().0
    }

    /// The manifest kind `id` is registered under — `"page"`, `"canvas"`,
    /// `"note"` — whether it is live, in the bin or only provisionally listed.
    ///
    /// `None` for a document this workspace's manifest does not name, and for
    /// every document of a workspace with no manifest: there is no kind to
    /// read, and the caller decides what that means rather than this guessing
    /// a page. Hidden kinds answer too; this is a question about a document,
    /// not about a listing.
    pub fn document_kind(&self, id: koru_core::DocumentId) -> Option<String> {
        if let Some(pending) = self.pending_documents.iter().find(|p| p.id == id) {
            return Some(pending.kind.clone());
        }
        self.manifest
            .as_ref()?
            .entries()
            .into_iter()
            .find(|entry| explore::koru_document(entry.id) == id)
            .map(|entry| entry.kind)
    }

    /// What is in the trash, in the same shape and the same order.
    ///
    /// **Its own query rather than a flag on `entries`,** because the two are
    /// asked by different things for different reasons and a caller that passed
    /// the wrong bool would show somebody their bin as their library.
    pub fn trashed_entries(&self) -> Vec<koru_core::explore::ExploreEntry> {
        self.entries_and_trashed().1
    }

    /// Both sides of the trash, classified in one manifest walk.
    ///
    /// The inherited classification belongs to the manifest: a document is in
    /// the bin if it or any ancestor was put there. Asking for both halves at
    /// once matters to the shell, which always pushes the library and trash
    /// together and must not walk the manifest twice.
    pub fn entries_and_trashed(
        &self,
    ) -> (
        Vec<koru_core::explore::ExploreEntry>,
        Vec<koru_core::explore::ExploreEntry>,
    ) {
        match &self.manifest {
            Some(manifest) => {
                let (live, trashed) = manifest.partitioned_entries();
                // One predicate decides what a person sees. Two kinds are
                // store-listed but never person-listed — the workspace system
                // document (`"koru-system"`, ADR-176) and its tasks document
                // (`"task-list"`, ADR-272's 2026-09-01 amendment): nobody
                // opens, names or drags either, and the tasks in the second are
                // seen through the Tasks view rather than by opening its home.
                // Hiding an entry leaves a hole in its siblings' ordinals, so
                // the survivors are renumbered — `ordinal` promises "position
                // among the entries sharing this parent", and that has to
                // mean the entries a caller can see.
                let unfiltered = live.len();
                let mut live: Vec<_> = live
                    .into_iter()
                    .map(explore_entry)
                    .filter(|e| koru_core::explore::listed(&e.kind))
                    .collect();
                if live.len() != unfiltered {
                    renumber_ordinals(&mut live);
                }
                let trashed: Vec<_> = trashed
                    .into_iter()
                    .map(explore_entry)
                    .filter(|e| koru_core::explore::listed(&e.kind))
                    .collect();
                let mut ordinal = live.iter().filter(|entry| entry.parent.is_none()).count() as u32;
                for pending in &self.pending_documents {
                    if live
                        .iter()
                        .chain(&trashed)
                        .any(|entry| entry.id == pending.id)
                    {
                        continue;
                    }
                    // Where the manifest is about to put it, so the row does not
                    // appear at the end and then hop. `after` names a *live*
                    // entry or nothing: a document made from one in the bin has
                    // no place to be next to, and the end is the fallback the
                    // store used too.
                    let anchor = pending
                        .after
                        .and_then(|after| live.iter().position(|entry| entry.id == after));
                    // A document made *inside* another sits last under it.
                    let under = pending
                        .parent
                        .and_then(|parent| live.iter().position(|entry| entry.id == parent));
                    let row = |parent, ordinal| koru_core::explore::ExploreEntry {
                        id: pending.id,
                        parent,
                        ordinal,
                        title: pending.title.clone(),
                        kind: pending.kind.clone(),
                        mime: None,
                        note: None,
                        trashed_at: None,
                        trashed_by: None,
                    };
                    match (under, anchor) {
                        (Some(at), _) => {
                            let parent = live[at].id;
                            let ordinal = live
                                .iter()
                                .filter(|entry| entry.parent == Some(parent))
                                .count() as u32;
                            let entry = row(Some(parent), ordinal);
                            live.insert(subtree_end(&live, at), entry);
                        }
                        (None, Some(at)) => {
                            let entry = row(live[at].parent, live[at].ordinal + 1);
                            live.insert(subtree_end(&live, at), entry);
                        }
                        (None, None) => {
                            live.push(row(None, ordinal));
                            ordinal += 1;
                        }
                    }
                }
                (live, trashed)
            }
            // No manifest: the tree projection, which has its own hidden
            // `Kind::Trash` root and so has already answered this.
            None => (self.ws.explore().unwrap_or_default(), Vec::new()),
        }
    }

    /// Rename a document or file entry in the workspace manifest.
    ///
    /// For a page the document itself is the truth and the listing catches up
    /// on the next tick. For a file there is no title row, so this *is* the
    /// name Explore shows and Open in app uses.
    pub fn rename_document(
        &mut self,
        id: koru_core::DocumentId,
        title: &str,
    ) -> Result<(), koru_store_core::errors::Error> {
        self.edit_manifest(|manifest| manifest.rename_document(explore::store_document(id), title))
    }

    /// Put a document in the trash — a move, never a delete (ADR-69).
    ///
    /// Its children go with it by inheritance, so restoring it brings them back.
    pub fn trash_document(
        &mut self,
        id: koru_core::DocumentId,
    ) -> Result<(), koru_store_core::errors::Error> {
        let at = koru_store_core::model::Timestamp::from_millis(open::now_ms());
        // Who, as this window would say it — the same label every other trash
        // action records. `None` is normal: an unnamed profile.
        let by = self.actor.clone();
        self.edit_manifest(|m| m.trash_document(explore::store_document(id), at, by.as_deref()))
    }

    /// Take one back out. The manifest detaches an inherited-trash child to the
    /// workspace root, so an explicit restore cannot succeed invisibly beneath
    /// a parent that remains in the bin.
    pub fn restore_document(
        &mut self,
        id: koru_core::DocumentId,
    ) -> Result<(), koru_store_core::errors::Error> {
        self.edit_manifest(|manifest| manifest.restore_document(explore::store_document(id)))
    }

    /// **Destroy every trashed document.** The one irreversible operation in the
    /// product, and the only caller of `Manifest::remove_document`.
    ///
    /// Unregisters the entries; their bytes are the store's to reclaim, and
    /// keeping the two apart is what makes an accidental unregister survivable.
    /// Returns how many went.
    ///
    /// **Not `empty_trash`, which is a different bin.** That one empties the
    /// `Kind::Trash` node inside the *open document* — rows somebody deleted
    /// while writing. This one is the workspace's list of documents. They pair
    /// with `trash` (a node) and [`trash_document`](Self::trash_document), and
    /// the names have to keep saying which, because the consequence of confusing
    /// them is destroying the wrong thing.
    pub fn empty_trashed_documents(&mut self) -> Result<usize, koru_store_core::errors::Error> {
        let doomed: Vec<_> = self.trashed_entries().into_iter().map(|e| e.id).collect();
        // **Roots first, and children only if they were not swept with one.**
        // `remove_document` takes a subtree, so a child named after its parent
        // has already gone and asking again is an error about a document nobody
        // is missing.
        for id in &doomed {
            let store_id = explore::store_document(*id);
            let Some(manifest) = &mut self.manifest else {
                break;
            };
            if manifest.contains(store_id) {
                let _ = manifest.remove_document(store_id);
            }
        }
        self.publish_manifest();
        Ok(doomed.len())
    }

    /// **Destroy one trashed document**, named rather than swept.
    ///
    /// [`empty_trashed_documents`](Self::empty_trashed_documents) is the
    /// bin's own sweep and walks the listing a *person* sees, which
    /// deliberately excludes the kinds nobody opens, renames or drags — a
    /// run among them (`koru_core::explore::listed`). A run is deleted by
    /// the surface that shows runs, one at a time, after the delegate
    /// worker has removed everything device-local it left behind; this is
    /// that surface's half.
    ///
    /// Unregisters the entry; its bytes are the store's to reclaim, the
    /// same split the sweep makes. Absent is not an error: a document
    /// another window purged first is a document that is already gone.
    pub fn purge_document(
        &mut self,
        id: koru_core::DocumentId,
    ) -> Result<bool, koru_store_core::errors::Error> {
        let store_id = explore::store_document(id);
        let Some(manifest) = &mut self.manifest else {
            return Ok(false);
        };
        if !manifest.contains(store_id) {
            return Ok(false);
        }
        manifest.remove_document(store_id)?;
        self.publish_manifest();
        Ok(true)
    }

    /// Change the manifest and get the change on its way at once.
    ///
    /// Every manifest edit wants the same two lines afterwards, and a drag that
    /// snapped back for a moment because one of them was forgotten reads as the
    /// gesture having failed.
    fn edit_manifest(
        &mut self,
        edit: impl FnOnce(
            &mut koru_store_core::manifest::Manifest,
        ) -> Result<(), koru_store_core::errors::Error>,
    ) -> Result<(), koru_store_core::errors::Error> {
        let manifest = self.manifest.as_mut().ok_or_else(|| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::InvalidRequest,
                "this workspace is not open on a store",
            )
        })?;
        edit(manifest)?;
        self.publish_manifest();
        Ok(())
    }

    /// Push the manifest now rather than on the next turn.
    fn publish_manifest(&mut self) {
        if let (Some(session), Some(manifest)) = (&mut self.manifest_session, &self.manifest) {
            let _ = session.publish(manifest);
        }
    }

    /// Every document currently in memory, active one included.
    ///
    /// The walk anything workspace-wide has to make: which blobs are still
    /// referenced, what links to what. **It sees the *open* documents, not all
    /// of them**, which is a real limit rather than an oversight — once
    /// documents load lazily, "every blob this workspace references" stops being
    /// answerable without opening all of them, and the honest home for that
    /// question is the store, which already has them.
    pub fn documents(&self) -> impl Iterator<Item = &WorkspaceDoc> {
        std::iter::once(&self.ws).chain(self.others.values())
    }

    /// A document by id, whether it is the active one or another open tab.
    pub fn document(&self, id: koru_core::DocumentId) -> Option<&WorkspaceDoc> {
        if self.active == Some(id) {
            return Some(&self.ws);
        }
        self.others.get(&id)
    }

    /// Every blob referenced by anything open.
    ///
    /// `WorkspaceDoc::blob_hashes` answers for one document; the shell asks this
    /// whenever the document changes, to decide what still needs decoding. It
    /// deliberately **does not** exclude the trash, because a binned image's
    /// bytes have to stay decodable or a restore hands back a broken picture.
    pub fn blob_hashes(&self) -> Vec<String> {
        let mut all: Vec<String> = self
            .documents()
            .filter_map(|d| d.blob_hashes().ok())
            .flatten()
            .collect();
        all.sort();
        all.dedup();
        all
    }

    /// Save what is open, then open another workspace in this process.
    ///
    /// **On failure nothing changes**: the workspace you were in is still open,
    /// still saveable, and the caller shows the dialog. The tempting shape —
    /// swap first, fall back to an empty document if the open fails — is the
    /// "no in-memory fall-back" rule broken in a new place, because a workspace
    /// that cannot save looks exactly like one that can until it is closed.
    ///
    /// **The undo history is gone, deliberately.** A `loro::UndoManager` is
    /// bound to a `LoroDoc`, so a new document is a new history; there is no
    /// version of this that keeps it. `docs/deferred/` recorded this as the
    /// cost of reopening a workspace, and it is now intentional rather than
    /// incidental.
    pub fn switch_to(
        &mut self,
        dir: &std::path::Path,
        id: koru_core::WorkspaceId,
        name: &str,
    ) -> Result<session::Report, session::OpenError> {
        // The outgoing document reaches disk first, and `save` publishes before
        // it snapshots — so a peer is never left behind a snapshot it cannot
        // see the history for.
        self.save();
        // The peer names *this instance*, not this workspace, so it travels.
        let peer = match &self.session {
            Some(s) => s.peer.clone(),
            None => session::DEFAULT_PEER.to_string(),
        };
        let (session, mut ws, report) = Session::open(dir, id, name, &peer)?;
        // **Reconcile the document's own name from the profile entry.**
        // `Session::open` passes `name` in but only uses it on `Opened::Fresh`,
        // which is what stops a rename being clobbered on every open — and is
        // also why the document can be left saying something else. A workspace
        // renamed while it was closed is renamed *in the profile*; this is where
        // it catches up. Without it a rename silently reverts for whoever opens
        // that workspace next.
        if ws.name().as_deref() != Some(name) {
            let _ = ws.rename(name);
        }
        // `save` above buffered anything the Store had not yet acknowledged;
        // now that the replacement is known to open, release every old room
        // while its documents are still available to the close helper.
        self.close_store_sessions();
        self.ws = ws;
        // **Everything else open belonged to the workspace being left.** A
        // `DocumentId` names a document in *a* workspace, and a `WorkspaceDoc`
        // held past the switch would be a tab showing content from a workspace
        // that is no longer open — the same reason `App` discards its blob cache
        // here.
        self.others.clear();
        self.active = None;
        self.manifest = None;
        self.client = None;
        self.pending_documents.clear();
        self.session = Some(session);
        self.current = Some(id);
        Ok(report)
    }

    /// Start recording undo. Called once, after the document is loaded and its
    /// first line exists — what you opened on is not something Ctrl+Z should be
    /// able to erase.
    pub fn enable_undo(&mut self) {
        self.ws.enable_undo();
    }

    /// Tell the session the document changed. Every mutation path funnels
    /// through here rather than publishing directly, so a burst of keystrokes
    /// becomes one update frame rather than one per key.
    pub fn touched(&mut self) {
        if let Some(s) = &mut self.session {
            s.touch();
        }
    }

    /// One turn of the sync loop. Returns true if a remote change arrived, in
    /// which case every view over this document has to rebuild.
    pub fn tick(&mut self) -> bool {
        let folder = match &mut self.session {
            Some(s) => s.tick(&mut self.ws),
            None => false,
        };
        folder | self.tick_store()
    }

    /// Whether the document manifest changed since the shell last asked.
    pub fn take_entries_changed(&mut self) -> bool {
        std::mem::take(&mut self.entries_changed)
    }

    /// Copy the active document's title into the manifest when it has changed.
    ///
    /// **A document's title is row 0 of its own body** ([ADR-50-ondemand]) and
    /// the manifest holds a last-writer-wins copy so Explore can list a
    /// workspace without opening anything ([ADR-91]). The document is the truth
    /// and the manifest is the cache; this is where the cache catches up.
    ///
    /// **Reconciled rather than hooked**, because typing has no choke point: a
    /// title changes through `set_text`, `insert_text` and `delete_text` from
    /// roughly twenty editor call sites — every one of which would have to
    /// remember, and the first that forgot would leave Explore showing a name
    /// the document no longer has. Comparing on the tick that already runs
    /// cannot be forgotten.
    ///
    /// It costs one string compare per tick, and writes only on a difference —
    /// so an idle window sends nothing, which is what
    /// `publishing_an_unchanged_document_sends_nothing` protects one layer down.
    ///
    /// [ADR-50-ondemand]: ../../../docs/decisions/README.md
    /// [ADR-91]: ../../../docs/decisions/ADR-076-100.md#adr-91-perdocument--a-document-keeps-a-root-node-for-its-title-and-the-manifest-mirrors-it
    fn reconcile_title(&mut self, active: koru_core::DocumentId) -> bool {
        let Some(title) = self.active_title() else {
            return false;
        };
        // HTTP creation can precede its manifest echo. Keep that existing
        // provisional projection current without creating a competing tree node.
        let mut changed = false;
        if let Some(pending) = self.pending_documents.iter_mut().find(|p| p.id == active)
            && pending.title != title
        {
            pending.title.clone_from(&title);
            changed = true;
        }
        let Some(manifest) = &mut self.manifest else {
            return changed;
        };
        let listed = manifest
            .entries()
            .into_iter()
            .find(|e| explore::koru_document(e.id) == active)
            .map(|e| e.title);
        if listed.as_deref() == Some(title.as_str()) {
            return changed;
        }
        manifest
            .rename_document(explore::store_document(active), &title)
            .is_ok()
            || changed
    }

    /// The active document's own name — its root `Kind::Document` node's text.
    fn active_title(&self) -> Option<String> {
        self.ws
            .children(koru_core::schema::Parent::Root)
            .ok()?
            .into_iter()
            .find(|&id| self.ws.kind_of(id) == Some(Kind::Document))
            .and_then(|id| self.ws.read(id).ok())
            .and_then(|n| n.content.as_text().map(str::to_owned))
    }

    /// One turn of the *store* sync loop: publish what changed, take what
    /// arrived.
    ///
    /// **Publish before drain**, matching the folder session's order and for the
    /// same reason: what this replica wrote should be on its way before it
    /// starts applying somebody else's, so a round trip cannot interleave a
    /// local edit behind a remote one that was already in flight.
    ///
    /// **Every open document drains, not just the active one.** A background tab
    /// that stopped taking updates would fall arbitrarily far behind and then
    /// apply all of it at once the moment it was shown — which is the stall
    /// [ADR-46](../../../docs/decisions/ADR-026-050.md#adr-46-catchup--a-view-is-told-where-something-changed-and-only-the-tab-you-are-looking-at-acts-on-it)
    /// is about. Draining is cheap; *rebuilding* is what only the visible tab
    /// does, and that decision belongs to the view rather than here.
    ///
    /// Only the active document publishes, because it is the only one being
    /// typed into.
    fn tick_store(&mut self) -> bool {
        let mut changed = false;
        // Accumulated rather than bracketed, because the two halves interleave:
        // the active document publishes and drains, then the background ones
        // drain, then the manifest does both. A single span around the whole
        // turn would answer "how long did the store cost" and not the question
        // that decides what to fix, which is "which half".
        let mut publishing = std::time::Duration::ZERO;
        let mut draining = std::time::Duration::ZERO;

        // A manifest update may have arrived while the previous system open
        // was in flight. Re-present the current in-memory id set every turn;
        // `SystemSession` only starts work when that set has not been
        // reconciled, and all blocking work stays on its worker.
        let system_ids = self.system_ids();
        if let Some(system) = &mut self.system {
            system.reconcile(system_ids, false);
        }

        if let Some(active) = self.active
            && let Some(session) = self.doc_sessions.get_mut(&active)
        {
            // A failed publish is not fatal and not silent: the batch stays
            // unacknowledged, which is what `unacknowledged()` reports. Dropping
            // the connection here would turn a blip into a session that never
            // writes again.
            let p = syncstats::Phase::start();
            let _ = session.publish(&self.ws);
            publishing += p.stop();

            let d = syncstats::Phase::start();
            let drained = session.drain(&mut self.ws).unwrap_or(false);
            if drained {}
            changed |= drained;
            draining += d.stop();

            self.entries_changed |= self.reconcile_title(active);
        }

        for (id, doc) in self.others.iter_mut() {
            if let Some(session) = self.doc_sessions.get_mut(id) {
                let d = syncstats::Phase::start();
                let drained = session.drain(doc).unwrap_or(false);
                if drained {}
                changed |= drained;
                draining += d.stop();
            }
        }

        if let Some(system) = &mut self.system {
            let d = syncstats::Phase::start();
            if system.tick() {
                // A peer created, renamed or merged a tag, or made a list: the
                // shell re-pushes both resolved tables into every view. One
                // tick cannot say which changed, so both are marked and the
                // compare inside each view decides whether anything repaints.
                self.tags_changed = true;
                self.lists_changed = true;
            }
            self.replay_trouble.extend(system.take_trouble());
            draining += d.stop();
        }

        changed |= self.tick_pages();
        self.tick_held_pages();

        let (_, p, d) = self.sync_manifest();
        publishing += p;
        draining += d;

        // The manifest counts: it is a connection the turn had to service, and
        // leaving it out would make a one-tab window look like it had one
        // session when it has two.
        //
        // A turn with none of them is not reported at all — a folder-backed
        // window would otherwise print a line a second saying nothing happened,
        // which is how a diagnostic gets turned off and stays off.
        let sessions = self.doc_sessions.len()
            + usize::from(self.manifest_session.is_some())
            + self
                .system
                .as_ref()
                .map(system::SystemSession::session_count)
                .unwrap_or(0);
        if sessions > 0 {
            syncstats::turn(publishing, draining, sessions);
        }

        self.buffer_what_is_owed();

        changed
    }

    /// One publish-and-drain turn of the manifest's sync loop.
    ///
    /// Returns what changed and how long each half took, so [`tick_store`] can
    /// keep its per-turn accounting and an on-demand caller can report its
    /// own. The manifest's change deliberately does not join `changed` — the
    /// host reads it through `entries_changed` and pushes entries instead of
    /// re-adopting every view.
    fn sync_manifest(&mut self) -> (bool, std::time::Duration, std::time::Duration) {
        let mut publishing = std::time::Duration::ZERO;
        let mut draining = std::time::Duration::ZERO;
        let mut manifest_changed = false;
        if let Some(session) = &mut self.manifest_session
            && let Some(manifest) = &mut self.manifest
        {
            let p = syncstats::Phase::start();
            let _ = session.publish(manifest);
            publishing += p.stop();

            let d = syncstats::Phase::start();
            manifest_changed = session.drain(manifest).unwrap_or(false);
            draining += d.stop();
        }
        if manifest_changed {
            self.entries_changed = true;
            // A creation echo may introduce the row that the earlier title
            // reconcile could not rename yet. Reconcile against that row in
            // this turn, then publish only if it produced a new local edit.
            if let Some(active) = self.active
                && self.reconcile_title(active)
                && let (Some(session), Some(manifest)) =
                    (&mut self.manifest_session, &self.manifest)
            {
                let p = syncstats::Phase::start();
                let _ = session.publish(manifest);
                publishing += p.stop();
            }
            // A peer's system document may have just appeared where this
            // workspace had none — let the next tag read look again.
            self.recheck_system_on_entries_change();
            if let Some(manifest) = &self.manifest {
                let confirmed: std::collections::HashSet<_> = manifest
                    .entries()
                    .into_iter()
                    .map(|entry| explore::koru_document(entry.id))
                    .collect();
                self.pending_documents
                    .retain(|pending| !confirmed.contains(&pending.id));
            }
        }
        (manifest_changed, publishing, draining)
    }

    /// One manifest sync turn, on demand rather than on the loop's cadence.
    ///
    /// A content import writes through its own store client, so the manifest
    /// this window lists from would only meet the new entries on the next
    /// turn like this one. Running it at completion keeps the listing as
    /// immediate as the toast that announces it. Returns whether the manifest
    /// changed.
    pub fn sync_manifest_now(&mut self) -> bool {
        let (changed, publishing, draining) = self.sync_manifest();
        if self.manifest_session.is_some() {
            syncstats::turn(publishing, draining, 1);
        }
        changed
    }

    /// Put what the store has not confirmed on this machine's disk, for every
    /// session that is not reaching it.
    ///
    /// **Only while the link is unhappy.** A live session's work is on its way
    /// and will be acknowledged in milliseconds; writing a file for it would be
    /// a disk write per keystroke to insure against a window that is already
    /// insured. Each session throttles itself on top of that.
    fn buffer_what_is_owed(&mut self) {
        if let Some(active) = self.active
            && let Some(session) = self.doc_sessions.get_mut(&active)
            && !session.link_state().is_live()
        {
            session.buffer_if_due(&self.ws);
        }
        for (id, doc) in self.others.iter() {
            if let Some(session) = self.doc_sessions.get_mut(id)
                && !session.link_state().is_live()
            {
                session.buffer_if_due(doc);
            }
        }
        if let Some(session) = &mut self.manifest_session
            && let Some(manifest) = &self.manifest
            && !session.link_state().is_live()
        {
            session.buffer_if_due(manifest);
        }
        if let Some(system) = &mut self.system {
            system.buffer_if_due();
        }
    }

    /// The same, unthrottled and whatever the link is doing — on the way out.
    ///
    /// **Not conditional on the link being unhappy.** A publish that left this
    /// process a moment ago has not been acknowledged yet, and the acknowledgement
    /// is the only thing that means anything; a window closing between the two
    /// would take that work with it.
    pub fn buffer_everything_owed(&mut self) {
        if let Some(active) = self.active
            && let Some(session) = self.doc_sessions.get_mut(&active)
        {
            session.buffer_now(&self.ws);
        }
        for (id, doc) in self.others.iter() {
            if let Some(session) = self.doc_sessions.get_mut(id) {
                session.buffer_now(doc);
            }
        }
        if let Some(session) = &mut self.manifest_session
            && let Some(manifest) = &self.manifest
        {
            session.buffer_now(manifest);
        }
        if let Some(system) = &mut self.system {
            system.buffer_now();
        }
        self.buffer_pages_now();
    }

    /// Whether everything typed has reached the store's disk, and where it is
    /// if not. See [`Unconfirmed`].
    pub fn unconfirmed(&self) -> Unconfirmed {
        let mut out = Unconfirmed::default();
        let mut why_not_buffered: Option<String> = None;

        let mut owing = |session: &store::StoreSession, doc: &dyn store::Synced| {
            if !session.owes(doc) {
                return false;
            }
            if why_not_buffered.is_none()
                && let Some(why) = session.buffer_failure()
            {
                why_not_buffered = Some(why.to_owned());
            }
            true
        };

        if let Some(active) = self.active
            && let Some(session) = self.doc_sessions.get(&active)
            && owing(session, &self.ws)
        {
            out.documents += 1;
        }
        for (id, doc) in self.others.iter() {
            if let Some(session) = self.doc_sessions.get(id)
                && owing(session, doc)
            {
                out.documents += 1;
            }
        }
        if let (Some(session), Some(manifest)) = (&self.manifest_session, &self.manifest) {
            out.document_list = owing(session, manifest);
        }
        // Notes above Tasks pages the store does not hold yet. Their writing
        // is on this disk as pending revisions unless a write failed, which
        // is the one case that makes closing unsafe.
        let (pages, pages_not_buffered) = self.pages_owed();
        out.documents += pages;
        if why_not_buffered.is_none() {
            why_not_buffered = pages_not_buffered;
        }

        out.not_buffered = why_not_buffered;
        out
    }

    /// The same three states as [`unconfirmed`](Self::unconfirmed), **per open
    /// document** rather than counted across the window. See
    /// [`durability`](crate::durability) for why one number cannot answer the
    /// question a person is asking, and why nothing here aggregates.
    ///
    /// **Costs one version-vector comparison per open document**, for the
    /// reason `unconfirmed` gives above it: a healthy window must pay nothing
    /// for a state it is not in. The one caller gates this on a screen reader
    /// actually being attached.
    pub fn durability(&self) -> durability::Durability {
        durability::of(self)
    }

    /// What could not be taken back off this machine's disk when a document was
    /// opened, and is therefore still sitting there.
    ///
    /// Taken rather than borrowed, because a caller shows it once. Empty is the
    /// ordinary answer.
    pub fn take_replay_trouble(&mut self) -> Vec<String> {
        std::mem::take(&mut self.replay_trouble)
    }

    /// When [`tick`] next has something to do, so the event loop can sleep
    /// until then rather than on a clock of its own. See
    /// [`Session::next_due`].
    ///
    /// [`tick`]: Self::tick
    /// [`Session::next_due`]: session::Session::next_due
    pub fn next_due(&self) -> Option<std::time::Instant> {
        let folder = self.session.as_ref().and_then(|s| s.next_due());
        // **A quiet store-backed workspace is due at no particular time.**
        //
        // This used to answer `Instant::now()` whenever any session existed,
        // because a session *polled* a socket and there was no later moment it
        // could name. The loop's `WaitUntil` therefore collapsed to zero and it
        // spun — 151 turns a second on an idle window, paying a blocking read
        // per session on each one. The socket is on its own thread now and an
        // arriving frame **wakes** the loop, so there is nothing to poll for and
        // the window may genuinely sleep.
        //
        // What is left is a watchdog, and only while something is actually
        // owed or the connection is unhappy: `store_trouble` reads how long a
        // link has been reconnecting, and nothing else would move that clock
        // along. A second is far finer than the ten it is compared against.
        let store_due = self
            .store_busy()
            .then(|| std::time::Instant::now() + STORE_WATCHDOG);
        // A note being typed above a Tasks page owes its next revision write
        // a second after the last one.
        let pages_due = self.pages_next_due();
        [folder, store_due, pages_due].into_iter().flatten().min()
    }

    /// Whether anything about the store still needs looking at on a clock.
    ///
    /// Not "is there a session" — that is always true for a store-backed
    /// workspace and is what made the loop spin. This is *is something
    /// outstanding*: edits the store has not confirmed, or a link that is not
    /// live and whose "how long has this been going on" nobody else will
    /// re-read.
    fn store_busy(&self) -> bool {
        self.doc_sessions
            .values()
            .chain(self.manifest_session.as_ref())
            .any(|s| s.unacknowledged() > 0 || !s.link_state().is_live() || s.stopped().is_some())
            || self
                .system
                .as_ref()
                .is_some_and(system::SystemSession::busy)
            || self.pages_busy()
    }

    /// Compact to a snapshot — on exit, and whenever the app goes quiet.
    ///
    /// Under the store there is no snapshot to compact, and what "save" means
    /// instead is *make sure nothing the store has not confirmed exists only in
    /// this process*. Every caller of this is a moment the window might not come
    /// back from — closing, the tray, going quiet — which is exactly when that
    /// matters.
    pub fn save(&mut self) {
        if let Some(s) = &mut self.session {
            s.save(&self.ws);
        }
        self.buffer_everything_owed();
    }

    pub fn toggle_sharing(&mut self) {
        if let Some(s) = &mut self.session {
            s.toggle_online(&self.ws);
        }
    }

    /// Why this workspace can no longer be written, once it has failed enough
    /// times to mean it. See [`Session::write_failed`].
    pub fn write_failed(&self) -> Option<&str> {
        self.session.as_ref().and_then(Session::write_failed)
    }

    /// Why this workspace will not be written, if it will not. An in-memory
    /// workspace is not read-only — it is *unsaved*, which is a different thing
    /// and is why tests can still edit one.
    pub fn read_only(&self) -> Option<&str> {
        self.session.as_ref().and_then(Session::read_only)
    }

    /// The content-addressed blob store, if this workspace is backed by disk.
    /// `None` for an in-memory workspace (tests, or no data directory); in that
    /// case images cannot be stored, only referenced.
    pub fn blobs(&self) -> Option<Blobs> {
        if let Some(session) = self.session.as_ref()
            && self.manifest.is_none()
        {
            return Some(Blobs::Folder(session.blobs()));
        }
        let (_segment, _root) = self.store.as_ref()?;
        let workspace = self.current?;
        let client = self.client.clone()?;
        Some(Blobs::Store(store::StoreBlobs::new(
            client,
            explore::store_workspace(workspace),
        )))
    }

    /// The blob store belonging to the document under the caret.
    ///
    /// Explore and the active tab may name different workspaces. Image reads,
    /// writes and clipboard embedding follow the tab, just like document
    /// edits, so they must use its Store session's workspace rather than the
    /// manifest currently shown in Explore.
    pub fn active_blobs(&self) -> Option<Blobs> {
        if let Some(session) = self.session.as_ref()
            && self.manifest.is_none()
        {
            return Some(Blobs::Folder(session.blobs()));
        }
        let (_segment, _root) = self.store.as_ref()?;
        let workspace = self.active_workspace()?;
        let client = self.client.clone()?;
        Some(Blobs::Store(store::StoreBlobs::new(
            client,
            explore::store_workspace(workspace),
        )))
    }

    // ---------------------------------------------------------- the trash
    //
    // Thin wrappers whose only job is to supply the two things `koru-core`
    // deliberately does not have: a clock, and a name. It is the same shape
    // `open.rs` already uses to feed `now_ms()` into the profile document, and
    // this is the one layer that can see both the session and the actor.

    /// Who this window says it is, for anything it writes into a document.
    ///
    /// **Self-declared, never inferred.** Falls back to the `--peer` slot when
    /// nobody has set a name — a replica handle is a poor label, but it is one
    /// the person chose on the command line, and it is only ever a fallback.
    /// `None` when there is nothing at all to say, which is normal.
    pub fn actor_label(&self) -> Option<String> {
        self.actor
            .clone()
            .or_else(|| self.session.as_ref().map(|s| s.peer.clone()))
            .filter(|s| !s.is_empty())
    }

    /// Record who is working in this window. Called once the profile is open,
    /// because that is where a person has already told us their name.
    pub fn set_actor(&mut self, label: Option<String>) {
        self.actor = label.filter(|s| !s.trim().is_empty());
    }

    /// Move a document — with its subtree — to the trash, stamped with this
    /// machine's clock and this window's label.
    ///
    /// Callers should `checkpoint()` first if the user has been typing, so the
    /// bin does not fold into the last keystroke.
    pub fn trash(&mut self, target: NodeId) -> Result<(), koru_core::schema::StoreError> {
        let by = self.actor_label();
        self.ws.trash(target, open::now_ms(), by.as_deref())?;
        self.touched();
        Ok(())
    }

    /// Put a trashed item back, recording who did it so the window that binned
    /// it can say so.
    pub fn restore(&mut self, target: NodeId) -> Result<(), koru_core::schema::StoreError> {
        let by = self.actor_label();
        self.ws.restore(target, by.as_deref())?;
        self.touched();
        Ok(())
    }

    pub fn delete_permanently(
        &mut self,
        target: NodeId,
    ) -> Result<(), koru_core::schema::StoreError> {
        self.ws.delete_permanently(target)?;
        self.touched();
        Ok(())
    }

    /// Empty the trash. Returns how many items went.
    pub fn empty_trash(&mut self) -> Result<usize, koru_core::schema::StoreError> {
        let n = self.ws.empty_trash()?;
        if n > 0 {
            self.touched();
        }
        Ok(n)
    }
}

/// Where `at`'s subtree ends in a pre-order listing — the index its next
/// sibling occupies, or the end of the list.
///
/// A listing is parents-then-children, so a document's descendants are the run
/// immediately after it. Inserting *below* a document therefore means inserting
/// after that whole run: putting the row at `at + 1` would land it between a
/// page and its own children, which reads as a nesting nobody asked for.
fn subtree_end(entries: &[koru_core::explore::ExploreEntry], at: usize) -> usize {
    let mut inside = vec![entries[at].id];
    let mut end = at + 1;
    while end < entries.len()
        && entries[end]
            .parent
            .is_some_and(|parent| inside.contains(&parent))
    {
        inside.push(entries[end].id);
        end += 1;
    }
    end
}

/// Re-count `ordinal` per parent after a listing filter, in listing order.
///
/// `ordinal` promises "position among the entries sharing this parent, from
/// 0" — and once a hidden entry (the system document) is filtered out, the
/// manifest's numbering has a hole the caller must not see.
fn renumber_ordinals(entries: &mut [koru_core::explore::ExploreEntry]) {
    let mut counts: std::collections::HashMap<Option<koru_core::DocumentId>, u32> =
        std::collections::HashMap::new();
    for entry in entries {
        let next = counts.entry(entry.parent).or_insert(0);
        entry.ordinal = *next;
        *next += 1;
    }
}

fn explore_entry(entry: koru_store_core::model::DocumentEntry) -> koru_core::explore::ExploreEntry {
    koru_core::explore::ExploreEntry {
        id: explore::koru_document(entry.id),
        parent: entry.parent.map(explore::koru_document),
        ordinal: entry.ordinal,
        title: entry.title,
        kind: entry.kind,
        mime: entry.mime,
        note: entry.note.map(explore::koru_document),
        trashed_at: entry.trashed_at.map(|time| time.millis()),
        trashed_by: entry.trashed_by,
    }
}

/// Compose the label a person is known by in this window, from what the profile
/// says and what the command line said.
///
/// First non-empty wins: the profile's display name, then the local part of its
/// email, then the `--peer` slot. The email's local part rather than the whole
/// address, because a trash row reading `Moved to Trash by sam@example.com` is
/// an address where a name should be.
///
/// **This is not identity.** It is a label the actor chooses and writes into
/// documents it edits; nothing anywhere derives it from a peer id or a peer file
/// name, which ADR-39 forbids. See `NodeMeta::trashed_by`.
pub fn actor_label(name: Option<&str>, email: Option<&str>, peer: Option<&str>) -> Option<String> {
    fn pick(s: Option<&str>) -> Option<&str> {
        s.map(str::trim).filter(|s| !s.is_empty())
    }
    pick(name)
        .or_else(|| {
            pick(email)
                .and_then(|e| e.split('@').next())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| pick(peer))
        .map(str::to_owned)
}

// ------------------------------------------------------------- top-level nodes
//
// Find-or-create for the nodes that sit at the top of the document rather than
// inside a body. They are operations on the workspace that a view happens to
// call, which is why they are free functions and not methods on a view.
//
// Find-or-create rather than always-create is what makes two tasks naming the
// same document reference *one* node, which is the whole point of the pill
// being a `Ref`.

/// The contact node for a name, creating one if it is new.
///
/// This is why an `@` pill is a reference rather than a string: the contact
/// is a node, so a kanban can group by it and renaming the person updates
/// every task at once.
pub fn contact(ws: &mut WorkspaceDoc, name: &str) -> Option<NodeId> {
    let existing = ws.children(Parent::Root).ok()?.into_iter().find(|&id| {
        ws.read(id)
            .map(|n| {
                n.kind == koru_core::Kind::Contact
                    && n.content.as_text().unwrap_or_default() == name
            })
            .unwrap_or(false)
    });
    if let Some(id) = existing {
        return Some(id);
    }
    let node = Node::new(koru_core::Kind::Contact, koru_core::Content::text(name)).ok()?;
    ws.insert(Parent::Root, node).ok()
}

/// The document node for a title, creating one if it is new.
///
/// The same find-or-create shape as [`contact`], and for the same reason: a
/// `/document` pill is a **reference to a node**, not a remembered string, so
/// two tasks can link one document and renaming it updates both pills at once.
/// Resolving one name to two nodes would break that.
///
/// **This is a lookup, not a way to make a document.** Anything that means
/// "give me a new one" wants [`create_document`] — asking here would hand back
/// somebody's existing work under a name that happened to match, and the next
/// keystroke would land in it. See [ADR-40](../../../docs/decisions/README.md).
///
/// [`contact`]: Self::contact
pub fn document(ws: &mut WorkspaceDoc, title: &str) -> Option<NodeId> {
    let existing = ws.children(Parent::Root).ok()?.into_iter().find(|&id| {
        ws.read(id)
            .map(|n| n.kind == Kind::Document && n.content.as_text().unwrap_or_default() == title)
            .unwrap_or(false)
    });
    if let Some(id) = existing {
        return Some(id);
    }
    create_document(ws, title)
}

/// Write a new document's seed into an **empty** `doc`: its `Kind::Document`
/// root, titled `title`, and — for a page — the one empty paragraph a caret
/// lands in. Returns the root.
///
/// **A canvas is seeded with its root alone.** Its body is what is put on the
/// surface, card by card; an empty paragraph under the root would be a card
/// nobody made, waiting at no position (CANVAS §5). Every other kind that
/// reaches a seed is a page-shaped document — a page, a note — and gets the
/// paragraph, which is what both seeding paths wrote before canvases existed.
///
/// One function for the two moments a document is seeded — at creation
/// ([`Workspace::create_document_after`], [`Workspace::create_canvas`]) and at
/// the first open of one the store registered empty — so the two cannot come
/// to disagree about what a canvas starts as.
pub fn seed_document(doc: &mut WorkspaceDoc, title: &str, kind: &str) -> Option<NodeId> {
    let root = doc.insert(Parent::Root, Node::document(title).ok()?).ok()?;
    if kind != koru_core::explore::KIND_CANVAS {
        doc.insert(Parent::Node(root), Node::paragraph("").ok()?)
            .ok()?;
    }
    Some(root)
}

/// A **new** document, whatever else is called that.
///
/// Two documents may share a name. That is deliberate: a name is a label people
/// choose, not a key — identity is the node's uuid ([ADR-4]) — and refusing the
/// second *Untitled* would mean either renaming it behind somebody's back or
/// refusing to make it at all. The pane tells them apart by what is in them,
/// the way two files in two folders can share a name.
///
/// A new document starts with one empty paragraph, so opening it lands the
/// caret somewhere typable rather than on a title with nothing beneath it.
///
/// [ADR-4]: ../../../docs/decisions/README.md
pub fn create_document(ws: &mut WorkspaceDoc, title: &str) -> Option<NodeId> {
    create_document_after(ws, title, None)
}

/// A new document as the next sibling of `after`, or appended to the library
/// when `after` is missing or not a live library node.
///
/// "Next to the page I was on" is the product default for New document — the
/// library end is only the fallback when there is no anchor (welcome, empty
/// profile).
pub fn create_document_after(
    ws: &mut WorkspaceDoc,
    title: &str,
    after: Option<NodeId>,
) -> Option<NodeId> {
    let node = Node::document(title).ok()?;
    let doc = if let Some(anchor) = after.filter(|&id| ws.read(id).is_ok()) {
        ws.insert_after(anchor, node).ok()?
    } else {
        ws.insert(Parent::Root, node).ok()?
    };
    let _ = ws.insert(Parent::Node(doc), Node::paragraph("").ok()?);
    Some(doc)
}

/// The document node a `WorkspaceDoc` is *about*: its title row, and what a tab
/// is opened over.
///
/// **One document is one `LoroDoc` and holds one root** ([ADR-90]), so the
/// first `Kind::Document` child of the root is the whole answer — the walk is
/// not a search for the *right* one among several. `None` means a document the
/// store registered and nothing has seeded yet: there is nothing to show, and a
/// tab over it would be a tab over an empty view.
///
/// "First" is doing the work in one case: two devices can seed the same
/// store-registered document before either's seed replicates, and the merge
/// keeps both roots until the next open folds them
/// (`WorkspaceDoc::unify_document_roots`, run from `seed_empty_document`).
/// Answering with the first is what every reader already does, so the fold
/// keeps exactly the root they were showing.
///
/// [ADR-90]: ../../../docs/decisions/ADR-076-100.md#adr-90-onetree--one-loro-document-is-one-documents-nodes-and-the-realm-is-gone
pub fn document_root(ws: &WorkspaceDoc) -> Option<NodeId> {
    ws.children(Parent::Root)
        .ok()?
        .into_iter()
        .find(|&id| ws.kind_of(id) == Some(Kind::Document))
}

/// Plant cuttings — and, for a partial row, one paragraph of runs — as the
/// **first** content under `under`, or under the document's root when `under`
/// is `None` or no longer resolves (the heading was deleted while the picker
/// was open; falling back to the top beats refusing the move).
///
/// Each cutting travels with the list style of the `ListBlock` that enclosed
/// it at the source, because a selection selects the *items* and never their
/// invisible container — planting the items bare is how a bullet arrived as a
/// plain paragraph. Consecutive cuttings sharing a style are planted under one
/// fresh `ListBlock` wearing it.
///
/// `plant` appends, so each planted root is moved to its place in order —
/// the same read-nothing-after-the-lift care `move_entry` documents.
fn plant_at_top(
    doc: &mut WorkspaceDoc,
    under: Option<NodeId>,
    cuttings: &[(koru_core::schema::Cutting, Option<koru_core::ListStyle>)],
    inline: Option<(&[koru_core::TextRun], &[koru_core::node::Attr])>,
) -> Result<(), koru_store_core::errors::Error> {
    let internal = |e: koru_core::StoreError| {
        koru_store_core::errors::Error::new(
            koru_store_core::errors::Code::Internal,
            format!("writing into the destination document: {e}"),
        )
    };
    let anchor = under
        .filter(|n| doc.read(*n).is_ok())
        .or_else(|| document_root(doc))
        .ok_or_else(|| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::InvalidRequest,
                "the destination document has no content root to receive the selection",
            )
        })?;
    let parent = Parent::Node(anchor);
    let mut index = 0usize;
    let mut open_list: Option<(koru_core::ListStyle, NodeId)> = None;
    for (cutting, style) in cuttings {
        let Some(style) = style else {
            open_list = None;
            let planted = doc.plant(cutting, parent, "").map_err(internal)?;
            doc.move_node(planted, parent, index).map_err(internal)?;
            index += 1;
            continue;
        };
        let list = match open_list {
            Some((open_style, list)) if open_style == *style => list,
            _ => {
                let list = doc
                    .insert(
                        parent,
                        Node::list_block().map_err(|_| {
                            koru_store_core::errors::Error::new(
                                koru_store_core::errors::Code::Internal,
                                "shaping the destination list",
                            )
                        })?,
                    )
                    .map_err(internal)?;
                doc.move_node(list, parent, index).map_err(internal)?;
                index += 1;
                doc.set_list_style(list, Some(*style)).map_err(internal)?;
                open_list = Some((*style, list));
                list
            }
        };
        // Appended inside a list this move just created, so the items keep
        // their order with no second move.
        doc.plant(cutting, Parent::Node(list), "")
            .map_err(internal)?;
    }
    if let Some((runs, attrs)) = inline {
        let paragraph = doc
            .insert(
                parent,
                Node::paragraph("").map_err(|_| {
                    koru_store_core::errors::Error::new(
                        koru_store_core::errors::Code::Internal,
                        "shaping the destination paragraph",
                    )
                })?,
            )
            .map_err(internal)?;
        doc.move_node(paragraph, parent, index).map_err(internal)?;
        doc.insert_runs(paragraph, 0, runs, attrs);
    }
    Ok(())
}

/// Plant into a document this window holds no replica of, over the store.
///
/// Joins the target's room, writes at the top, and **waits for durability**
/// before answering — the caller is about to delete the moved content from
/// its source, and an `Ok` here is the only licence for that. Blocks for up
/// to ten seconds, so run it off the event loop.
pub fn plant_into_foreign_document(
    segment: &str,
    root: Option<&std::path::Path>,
    workspace: koru_core::WorkspaceId,
    document: koru_core::DocumentId,
    under: Option<NodeId>,
    cuttings: &[(koru_core::schema::Cutting, Option<koru_core::ListStyle>)],
    inline: Option<(&[koru_core::TextRun], &[koru_core::node::Attr])>,
) -> Result<(), koru_store_core::errors::Error> {
    let (mut session, snapshot) = store::StoreSession::join(
        segment,
        root,
        explore::store_workspace(workspace),
        explore::store_document(document),
    )?;
    let mut doc = WorkspaceDoc::from_snapshot(&snapshot).map_err(|e| {
        koru_store_core::errors::Error::new(
            koru_store_core::errors::Code::Internal,
            format!("opening the destination document: {e}"),
        )
    })?;
    plant_at_top(&mut doc, under, cuttings, inline)?;
    // The mcp-server's `persist` idiom: an acknowledgement means the bytes are
    // on the disk, and only a durable write licenses the source-side delete
    // that follows.
    publish_durably(&mut session, &mut doc, "move")
}

/// **Edit a document as another writer**, over the store, and answer only
/// once the store has the edit on its disk.
///
/// [`plant_into_foreign_document`]'s shape for any edit, reached through a
/// client the caller already holds rather than by discovering the store
/// again: join the document's room with a replica of its own, run `edit` on
/// it, publish once, wait for the durable acknowledgement and leave. A window
/// holding the same document receives the edit the way it receives another
/// device's — **so it never enters that window's undo history**, which is the
/// point for writes a person did not make (a canvas chain's results and
/// links, CANVAS §7 *Writing into a person's documents*).
///
/// An edit that changes nothing publishes nothing. Blocks for up to ten
/// seconds plus the join, so it belongs on a worker, never on a thread that
/// answers a person.
pub fn edit_foreign_document<T>(
    factory: store::ClientFactory,
    workspace: koru_core::WorkspaceId,
    document: koru_core::DocumentId,
    edit: impl FnOnce(&mut WorkspaceDoc) -> Result<T, koru_store_core::errors::Error>,
) -> Result<T, koru_store_core::errors::Error> {
    let (mut session, snapshot) = store::StoreSession::join_with_client_factory(
        factory,
        explore::store_workspace(workspace),
        explore::store_document(document),
    )?;
    let mut doc = match WorkspaceDoc::from_snapshot(&snapshot) {
        Ok(doc) => doc,
        Err(error) => {
            session.leave();
            return Err(koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::Internal,
                format!("opening the document: {error}"),
            ));
        }
    };
    let before = doc.version();
    let out = match edit(&mut doc) {
        Ok(out) => out,
        Err(error) => {
            session.leave();
            return Err(error);
        }
    };
    if doc.version() == before {
        session.leave();
        return Ok(out);
    }
    publish_durably(&mut session, &mut doc, "edit")?;
    Ok(out)
}

/// Publish what `doc` holds and wait, bounded, until the store calls it
/// durable; then leave the room.
fn publish_durably(
    session: &mut store::StoreSession,
    doc: &mut WorkspaceDoc,
    what: &str,
) -> Result<(), koru_store_core::errors::Error> {
    if let Some(batch) = session.publish(&*doc)? {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !session.is_durable(batch) {
            session.drain(doc)?;
            if std::time::Instant::now() >= deadline {
                session.leave();
                return Err(koru_store_core::errors::Error::new(
                    koru_store_core::errors::Code::StorageFailed,
                    format!("the store did not acknowledge the {what} within ten seconds"),
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    session.leave();
    Ok(())
}

/// What a node is called, for a window title. `Untitled` when it has no text
/// yet, because a blank titlebar reads as a broken window.
pub fn name_of(ws: &WorkspaceDoc, node: NodeId) -> String {
    ws.read(node)
        .ok()
        .and_then(|n| n.content.as_text().map(str::to_owned))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Untitled".into())
}
