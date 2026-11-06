// SPDX-License-Identifier: MPL-2.0

//! Native tasks — the workspace's `task-list` documents.
//!
//! A task created in the Tasks view has no document to live in: nobody was
//! editing a page when they typed it. SPEC §8 gives it one — a marked node in a
//! workspace `task-list` document — and this module is the host side of that:
//! find or create the document, append the row, and run the canonical commands
//! over it.
//!
//! **The document is hidden, the way the system document is.** Its manifest
//! entry carries [`KIND_TASK_LIST`], and `koru_core::explore::listed` filters
//! that kind out of every listing a person sees (ADR-272's 2026-09-01
//! amendment; SPEC §8). Captured tasks are seen through the Tasks view, which
//! reads them by their marker through the query and never by which document
//! holds them — so the document has no row to open, rename or drag, and this
//! module's two visible choices, [`TASK_LIST_TITLE`] and *append at the end*,
//! are implementation details rather than product decisions somebody has to
//! live with. Hidden means *unlisted* and nothing else: the document syncs,
//! projects and indexes like any other, so a task's text is found by search.
//!
//! **Multiple `task-list` documents are valid, and nothing here reconciles
//! them.** That is the one place this module deliberately parts company with
//! [`crate::system`], which it otherwise reads like: two offline peers each
//! creating one is an ordinary outcome, both hold real tasks, and the read
//! projection lists tasks by their marker rather than by which document holds
//! them (`Projections::task_rows` filters on the derivation and the node kind,
//! never on `DocumentEntry.kind`). Merging them would mean moving nodes between
//! `LoroDoc`s — a copy-and-delete, per document tree — to solve a problem that
//! only exists if you insist there is one home. So creation *picks* an existing
//! document and creates one only when the workspace has none, and a workspace
//! that ends up with two simply has two.
//!
//! **The pick is the lowest [`DocumentId`]**, the same rule
//! [`SystemDoc::canonical_document`] uses and for the same reason: every
//! replica answers it identically without talking, so once two peers have seen
//! each other they agree about where the *next* task goes. Manifest order would
//! also be deterministic and is the weaker choice — it is a list somebody else's
//! gesture reorders, and the pick must not depend on anything a person can move.
//!
//! **Everything is stamped by construction.** A document created here is a
//! [`WorkspaceDoc::create`] carrying the workspace id, because step 0 of every
//! canonical command's preflight is that read and a document without one
//! refuses `/due` silently (see `Workspace::open_document_in`'s note). The
//! store's own (empty) snapshot is imported into it afterwards, which is how
//! `SystemDoc` handles the same race.
//!
//! [`SystemDoc::canonical_document`]: koru_core::system::SystemDoc::canonical_document

use koru_core::attributes::{KEY_LIST, KEY_PRIORITY, Lifecycle, Priority, TaskReason};
use koru_core::ids::ListId;
use koru_core::node::{AttrValue, Node};
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::tasks::{self, TaskOutcome};
use koru_core::time::Temporal;
use koru_core::{DocumentId, NodeId, WorkspaceId};

use crate::{Workspace, document_root, explore, store};

/// The `DocumentEntry.kind` token a workspace's tasks document carries.
///
/// The store never interprets it — it is a stable application token beside
/// `page`, `file` and `koru-system`
/// ([ADR-80](../../../docs/decisions/ADR-076-100.md#adr-80-manifest--a-workspace-is-a-manifest-document-plus-opaque-documents)).
///
/// **Declared in `koru_core::explore` and re-exported here.** It used to live
/// in this module, on the reasoning that nothing below the workspace layer had
/// an opinion about tasks having a home. `explore::listed` now has one: the
/// document is hidden from Explore the way the system document is, and the
/// predicate that decides that cannot name a constant in a crate above it.
pub use koru_core::explore::KIND_TASK_LIST;

/// The name a tasks document is registered under.
///
/// **Nobody sees it.** `explore::listed` filters the kind out of every listing
/// a person is shown, so this is a label for the manifest, a debugger and the
/// title row of a document that is never opened as a tab — not a name in the
/// library. It is a real word rather than the system document's empty string
/// because a tasks document *is* an ordinary `WorkspaceDoc` whose row 0 is its
/// title, and an untitled one would read as a bug to whoever next opens the
/// manifest. English only, per the release's string policy.
pub const TASK_LIST_TITLE: &str = "Tasks";

/// A task created straight into a `task-list` document.
///
/// Two outcomes rather than one, because two canonical commands ran: a host
/// drains `created.activity` and then `dated.activity`, in that order, and
/// appends what it finds ([ADR-286]). They are *not* merged into one outcome —
/// each carries its own causation id, and inventing a third would make the
/// desktop's grouping differ from MCP's for the same pair of commands.
///
/// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
/// **What a write landed as** — the receipt a Tasks surface holds while it
/// waits for the read model to catch up.
///
/// The read model is fed asynchronously and coalesces, so a query sent the
/// turn after a write is answered from the previous projection. A surface that
/// has the receipt can name the write to the query (`TaskQuery::covering`) and
/// ask again while the answer is behind it. The vector is read **at the moment
/// the write landed**, from the document that was written — never later, and
/// never fabricated from an intent.
///
/// `published` is whether the session took the publish. `false` is an ordinary
/// state — a stopped session, an outbox that will carry the write later — and
/// a surface reads it as *invalidate and do not wait*: waiting on a store that
/// has not been told would burn the whole retry budget every time.
#[derive(Clone, PartialEq, Debug)]
pub struct Landed {
    /// The workspace this window has open, else the one the document is
    /// stamped with. `None` for a document that carries neither — a surface
    /// then invalidates without a requirement, because a requirement it could
    /// not address to a workspace is one the read model could not answer.
    pub workspace: Option<WorkspaceId>,
    pub document: DocumentId,
    /// The node a native task was planted on — the one id `Create` does not
    /// carry in its input, so the page can turn its provisional row into the
    /// real one the moment the receipt arrives. `None` for the verbs whose
    /// node is the command's own.
    pub node: Option<NodeId>,
    /// Where the caret should go when the receipt opens a page — a new
    /// home's first paragraph, under the title that is the task. `None` for
    /// every other command.
    pub caret: Option<NodeId>,
    /// **The write this receipt is for, scoped to the session that made it** —
    /// or `None` when the command wrote nothing.
    ///
    /// It was the document's whole version vector, which grows one entry per
    /// writer the document has ever had and so grew without bound; see
    /// [`WriteReceipt`]. `None` is the **no-op** answer and is a real one: a
    /// guarded command that changed nothing has nothing for a read model to
    /// catch up to, so a surface must not arm a wait for it — and must not
    /// keep an optimistic row waiting for an event that will never come.
    pub receipt: Option<WriteReceipt>,
    pub published: bool,
}

/// **One write, named by the session that made it.**
///
/// `peer` is the writing Loro session; `counter` is the exclusive end of that
/// session's own operations at the moment the mutation committed. A read model
/// covers it when it has applied at least that many of this peer's operations.
///
/// Two integers, whatever the document's history — which is the property the
/// version vector it replaces could not have. A command that writes several
/// operations still ends at one counter, and one peer's operations are
/// contiguous, so covering the end covers all of them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WriteReceipt {
    pub peer: u64,
    pub counter: i64,
}

/// The exclusive end of this document session's **own** operations.
///
/// `VersionVector::get(peer)` rather than the whole vector: what a surface
/// waits for is its own write arriving, and every other writer's progress is
/// noise it cannot act on.
fn own_counter(doc: &WorkspaceDoc) -> i64 {
    let peer = doc.peer_id();
    i64::from(doc.version().get(&peer).copied().unwrap_or(0))
}

impl Landed {
    /// `before` is this session's own counter as it was **before** the
    /// command ran, which is the whole of how a no-op is told from a write:
    /// a command that committed nothing did not move it.
    fn of(
        current: Option<WorkspaceId>,
        document: DocumentId,
        doc: &WorkspaceDoc,
        published: bool,
        before: i64,
    ) -> Self {
        let after = own_counter(doc);
        Self {
            workspace: current.or_else(|| doc.id()),
            document,
            node: None,
            caret: None,
            receipt: (after > before).then(|| WriteReceipt {
                peer: doc.peer_id(),
                counter: after,
            }),
            published,
        }
    }
}

/// **The title of the document made for a page-made task**: *Notes* and the
/// task's own words. One rule, read by the write that makes the document and
/// by the page's echo that names it a moment earlier.
pub fn home_title(task: &str) -> String {
    let task = task.trim();
    if task.is_empty() {
        "Notes".to_owned()
    } else {
        format!("Notes {task}")
    }
}

/// What a page-made task carries, read before its page is made.
struct Lifted {
    title: String,
    due: Option<Temporal>,
    completed: bool,
    /// The day it was finished on, carried so the page it becomes says the
    /// same day the row did rather than today.
    completed_at: Option<koru_core::time::DateOnly>,
    priority: Option<Priority>,
    list: Option<ListId>,
}

/// A page-made task that now lives in a document of its own.
#[derive(Clone, PartialEq, Debug)]
pub struct HomedTask {
    /// The document made for it, at the root of the workspace.
    pub document: DocumentId,
    /// The task: the document's own title row.
    pub node: NodeId,
    /// The receipt, naming the new document and the planted node.
    pub landed: Landed,
}

#[derive(Clone, PartialEq, Debug)]
pub struct NativeTask {
    /// The `task-list` document it landed in — the one that existed, or the one
    /// this call registered.
    pub document: DocumentId,
    /// The row. A `Kind::Paragraph`, which is what SPEC §4 makes task-capable
    /// and what the editor already knows how to draw.
    pub node: NodeId,
    /// [`tasks::ensure_task`]'s answer — the `task.created` intent, reason
    /// `tasks_view`.
    pub created: TaskOutcome,
    /// [`tasks::set_due`]'s answer, when the capture row carried a date.
    pub dated: Option<TaskOutcome>,
    /// The receipt: which document, at what version, and whether the store
    /// was told.
    pub landed: Landed,
}

impl Workspace {
    /// Create a task in this workspace's tasks document — SPEC §8's native
    /// task.
    ///
    /// Finds a `task-list` document (or registers one), appends a paragraph
    /// carrying `title`, marks it with [`TaskReason::TasksView`] and, when
    /// `due` is given, dates it with the canonical [`tasks::set_due`] — **all
    /// inside one [`tasks::compose`] bracket**, so the whole capture is one
    /// Ctrl+Z rather than three.
    ///
    /// **It never changes which document the window is showing.** The tasks
    /// document is joined as a parked replica in `others`, exactly as a
    /// background tab is, so it drains and buffers with everything else and no
    /// new session bookkeeping exists; the write is published through its own
    /// room immediately, the way a cross-document move already publishes into a
    /// parked tab.
    ///
    /// **A task typed into a list carries that list** (SPEC §8). Membership is
    /// one `koru.list` value on the task — a *filter*, not a second structure —
    /// written inside the same bracket rather than by putting the task
    /// somewhere else. `None` is the ordinary case: a task captured in Today
    /// belongs to no list, and the Tasks view still shows it.
    ///
    /// **The list's own record is the caller's to have arranged.** This
    /// document cannot see the workspace list table, and an id the table has
    /// forgotten resolves to no list rather than to an error — the same
    /// contract `WorkspaceDoc::set_list` states, which is what makes deleting a
    /// list cost a task nothing.
    pub fn create_native_task(
        &mut self,
        title: &str,
        due: Option<Temporal>,
        list: Option<ListId>,
        priority: Option<Priority>,
    ) -> Result<NativeTask, koru_store_core::errors::Error> {
        self.create_native_task_ranked(title, due, list, priority, None)
    }

    /// [`create_native_task`](Self::create_native_task), placed: `rank` is
    /// where the task sits in its list's dragged order (`koru.rank`) — the
    /// top, for a task typed into a list page that is ordered by hand.
    pub fn create_native_task_ranked(
        &mut self,
        title: &str,
        due: Option<Temporal>,
        list: Option<ListId>,
        priority: Option<Priority>,
        rank: Option<i64>,
    ) -> Result<NativeTask, koru_store_core::errors::Error> {
        let document = self.ensure_task_list()?;
        let current = self.current;
        let ((node, created, dated), landed) = if self.active == Some(document) {
            let before = own_counter(&self.ws);
            let planted = plant_native_task_ranked(&mut self.ws, title, due, list, priority, rank)?;
            let published = self.publish_active();
            let landed = Landed::of(current, document, &self.ws, published, before);
            (planted, landed)
        } else {
            let doc = self.others.get_mut(&document).ok_or_else(|| {
                koru_store_core::errors::Error::new(
                    koru_store_core::errors::Code::Internal,
                    "the tasks document was opened and then lost",
                )
            })?;
            let before = own_counter(doc);
            let planted = plant_native_task_ranked(doc, title, due, list, priority, rank)?;
            // Published now rather than on the next turn: a task captured in
            // the Tasks view must exist for every other window the moment it
            // exists here, and a parked document never publishes on the tick.
            let published = match self.doc_sessions.get_mut(&document) {
                Some(session) => session.publish(doc).is_ok(),
                None => false,
            };
            let landed = Landed::of(current, document, doc, published, before);
            (planted, landed)
        };
        let landed = Landed {
            node: Some(node),
            ..landed
        };
        Ok(NativeTask {
            document,
            node,
            created,
            dated,
            landed,
        })
    }

    /// Run a canonical task command against a task in **any** document of this
    /// workspace, parking that document if it is not already open.
    ///
    /// A task lives wherever it was typed, so a Tasks surface ticking a row is
    /// a write into a document nobody is looking at. This is
    /// [`create_native_task`](Self::create_native_task)'s second half, lifted
    /// out and pointed at an arbitrary document: park through
    /// [`ensure_document`](Self::ensure_document), run, and **publish
    /// immediately** — a parked document never publishes on the tick, and a
    /// task ticked in this window has to exist for every other window the
    /// moment it exists here.
    ///
    /// The outcome comes back untouched so the caller can attempt the activity
    /// the command minted, **with the receipt beside it** — the document's
    /// version the moment the write landed, and whether the store was told.
    /// Nothing is emitted from here: that is the host's, and keeping the two
    /// apart is what makes MCP and the desktop group one cascade identically
    /// ([ADR-286]).
    ///
    /// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
    pub fn run_task_command<T>(
        &mut self,
        document: DocumentId,
        command: impl FnOnce(&mut WorkspaceDoc) -> Result<T, koru_core::StoreError>,
    ) -> Result<(T, Landed), koru_store_core::errors::Error> {
        self.edit_document(document, command)
    }

    /// **Edit any document of this workspace and publish it at once.**
    ///
    /// [`run_task_command`](Self::run_task_command)'s body, under the name of
    /// what it actually does. A task command was the first caller; a
    /// delegation's pill is the second, and it is not a task command — so the
    /// door keeps one body and gains an honest name rather than a second
    /// implementation of park-write-publish.
    ///
    /// Three cases, in the order they cost: the active document, a parked one,
    /// and one this window does not hold, which
    /// [`ensure_document`](Self::ensure_document) opens. **Publishing is
    /// immediate** because a parked document never publishes on the tick.
    pub fn edit_document<T>(
        &mut self,
        document: DocumentId,
        edit: impl FnOnce(&mut WorkspaceDoc) -> Result<T, koru_core::StoreError>,
    ) -> Result<(T, Landed), koru_store_core::errors::Error> {
        let current = self.current;
        if self.active == Some(document) {
            let before = own_counter(&self.ws);
            let out = edit(&mut self.ws).map_err(internal)?;
            let published = self.publish_active();
            return Ok((
                out,
                Landed::of(current, document, &self.ws, published, before),
            ));
        }
        self.ensure_document(document)?;
        let doc = self.others.get_mut(&document).ok_or_else(|| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::Internal,
                "that document was opened and then lost",
            )
        })?;
        let before = own_counter(doc);
        let out = edit(doc).map_err(internal)?;
        let published = match self.doc_sessions.get_mut(&document) {
            Some(session) => session.publish(doc).is_ok(),
            None => false,
        };
        Ok((out, Landed::of(current, document, doc, published, before)))
    }

    /// Read something out of a task's document, wherever this window holds
    /// it: the active document, a parked one, or one opened for the purpose.
    fn read_in<T>(
        &mut self,
        document: DocumentId,
        read: impl FnOnce(&WorkspaceDoc) -> Result<T, koru_core::StoreError>,
    ) -> Result<T, koru_store_core::errors::Error> {
        if self.active == Some(document) {
            return read(&self.ws).map_err(internal);
        }
        self.ensure_document(document)?;
        let doc = self.others.get(&document).ok_or_else(|| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::Internal,
                "that document was opened and then lost",
            )
        })?;
        read(doc).map_err(internal)
    }

    /// **Give a page-made task a document of its own — and make the document
    /// the task.** A new page named by [`home_title`] at the root of the
    /// workspace, whose title row carries the marker, the due, the level and
    /// the list the row had (a page is task-capable, SPEC §4), completed if
    /// it was; the tasks document loses the original. The row stays in the
    /// queue it was in, reading the page's name, and its origin is the page
    /// from here on. The page's one empty paragraph is where the caret goes.
    ///
    /// **Made before it is taken.** The page is created and marked first, and
    /// only then is the original deleted, so a failure half-way leaves a
    /// duplicate rather than nothing: the row somebody typed is the one thing
    /// this must not lose. Refuses a node that is not a task.
    pub fn home_native_task(
        &mut self,
        document: DocumentId,
        node: NodeId,
        today: koru_core::time::DateOnly,
    ) -> Result<HomedTask, koru_store_core::errors::Error> {
        let current = self.current;
        let lifted = self.read_in(document, |doc| {
            let read = doc.read(node)?;
            let fields = tasks::TaskFields::of(&read);
            if !tasks::is_task(&fields) {
                return Ok(None);
            }
            Ok(Some(Lifted {
                title: read.content.as_text().unwrap_or("").to_owned(),
                due: fields.due.clone(),
                completed: fields.lifecycle == Lifecycle::Completed,
                completed_at: fields.completed_at,
                priority: read.priority(),
                list: read.list_id(),
            }))
        })?;
        let Some(lifted) = lifted else {
            return Err(koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::InvalidRequest,
                "that row is not a task",
            ));
        };
        // At the root of the workspace: no anchor, which `placement_after`
        // answers with the root and the end.
        let home = self.create_document_after(&home_title(&lifted.title), None)?;
        // `create_document_after` opened the new document as `self.ws`, so the
        // counter the receipt is compared against is *that* document's — and
        // this session had written nothing to it before creating it. Zero is
        // therefore the honest `before`: the receipt covers everything this
        // session wrote for the move — the document, its root and the
        // attributes lifted onto it. Reading `own_counter` of the tasks
        // document ahead of the creation compared two documents' counters, and
        // in any session that had already written to the tasks document the
        // receipt came out absent, nothing was waited on, and the optimistic
        // row snapped back.
        let before = 0;
        let root = document_root(&self.ws).ok_or_else(|| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::Internal,
                "the new document has no title row to make the task of",
            )
        })?;
        let caret = self
            .ws
            .children(Parent::Node(root))
            .ok()
            .and_then(|children| children.first().copied());
        // One bracket: the marker, the fields and the lifecycle are one
        // change on the page, as they were one row.
        tasks::compose(&mut self.ws, |doc| -> koru_core::schema::Result<()> {
            tasks::ensure_task(doc, root, TaskReason::Explicit)?;
            if let Some(due) = lifted.due.clone() {
                tasks::set_due(doc, root, due)?;
            }
            if let Some(level) = lifted.priority {
                doc.set_attribute_on_all(
                    &[root],
                    KEY_PRIORITY,
                    AttrValue::Text(level.as_str().to_owned()),
                )?;
            }
            if let Some(list) = lifted.list {
                doc.set_attribute_on_all(&[root], KEY_LIST, AttrValue::Text(list.to_storage()))?;
            }
            if lifted.completed {
                tasks::complete_task(
                    doc,
                    root,
                    tasks::LifecycleReason::ExplicitComplete,
                    lifted.completed_at.unwrap_or(today),
                )?;
            }
            Ok(())
        })
        .map_err(internal)?;
        let published = self.publish_active();
        let landed = Landed {
            node: Some(root),
            caret,
            ..Landed::of(current, home, &self.ws, published, before)
        };
        self.run_task_command(document, |doc| doc.delete(node).map(|_| ()))?;
        Ok(HomedTask {
            document: home,
            node: root,
            landed,
        })
    }

    /// Every `task-list` document this workspace's manifest lists, sorted and
    /// deduplicated.
    ///
    /// Normally one; several after two offline peers each created one, which is
    /// a valid state rather than a race to repair. A workspace with no manifest
    /// answers nothing — it is not open on a store, and nothing here can be.
    pub fn task_list_documents(&self) -> Vec<DocumentId> {
        let Some(manifest) = &self.manifest else {
            return Vec::new();
        };
        let mut ids: Vec<DocumentId> = manifest
            .entries()
            .into_iter()
            .filter(|entry| entry.kind == KIND_TASK_LIST)
            .map(|entry| explore::koru_document(entry.id))
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// Open a tasks document, creating and registering one when the workspace
    /// has none. Answers the document a native task should be written into.
    fn ensure_task_list(&mut self) -> Result<DocumentId, koru_store_core::errors::Error> {
        // A tasks document this window already holds needs no store to be
        // found — which is also what lets the receipt path be pinned in
        // memory, where no store is dialled.
        let listed = pick_task_list(&self.task_list_documents());
        if let Some(id) = listed
            && (self.active == Some(id) || self.others.contains_key(&id))
        {
            return Ok(id);
        }
        let workspace = self.current.ok_or_else(|| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::InvalidRequest,
                "this workspace is not open on a store",
            )
        })?;
        let (segment, root) = self.store.clone().ok_or_else(|| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::InvalidRequest,
                "this workspace has no store to keep tasks in",
            )
        })?;
        let store_ws = explore::store_workspace(workspace);

        if let Some(id) = listed {
            let (mut session, snapshot) = store::StoreSession::join(
                &segment,
                root.as_deref(),
                store_ws,
                explore::store_document(id),
            )?;
            let mut doc = WorkspaceDoc::from_snapshot(&snapshot).map_err(|e| {
                koru_store_core::errors::Error::new(
                    koru_store_core::errors::Code::CorruptState,
                    format!("the tasks document {id:?}: {e}"),
                )
            })?;
            self.replay_trouble
                .extend(session.replay_buffered(&mut doc));
            // Before undo, and for the reason `open_document_in` states at
            // length: a stamp is an op like any other, and with undo already on
            // the first Ctrl+Z would take the workspace id back off.
            doc.stamp_workspace_id(workspace).map_err(internal)?;
            doc.enable_undo();
            if let Some(waker) = &self.waker {
                session.set_waker(waker.clone());
            }
            self.doc_sessions.insert(id, session);
            self.others.insert(id, doc);
            return Ok(id);
        }

        // None yet. Register one, join its room, and seed it locally — the
        // created document carries the workspace id by construction, and the
        // store's snapshot is imported on top so a peer that won the same race
        // in the same room is not overwritten.
        let client = self.client()?;
        let store_id =
            client.create_document_at(store_ws, TASK_LIST_TITLE, KIND_TASK_LIST, None, None)?;
        let id = explore::koru_document(store_id);
        let (mut session, snapshot) =
            store::StoreSession::join(&segment, root.as_deref(), store_ws, store_id)?;
        let mut doc = WorkspaceDoc::create(workspace, TASK_LIST_TITLE).map_err(internal)?;
        let _ = doc.import(&snapshot);
        seed_task_list(&mut doc)?;
        doc.stamp_schema_version().map_err(internal)?;
        let _ = session.publish(&doc);
        doc.enable_undo();
        if let Some(waker) = &self.waker {
            session.set_waker(waker.clone());
        }
        self.doc_sessions.insert(id, session);
        self.others.insert(id, doc);
        // **No provisional row.** `register_document_placed` keeps one so a
        // document a person just made appears in Explore before the manifest
        // broadcast lands; this document never appears in Explore at all, and a
        // `Pending` carries no kind for `explore::listed` to filter — so the
        // row would be the one moment the hidden document was visible, and it
        // would linger until the broadcast arrived.
        Ok(id)
    }
}

/// Which of several `task-list` documents new tasks go into: the lowest id.
///
/// Pure, and shared by every replica — see the module note. `None` for a
/// workspace that has none yet.
pub fn pick_task_list(ids: &[DocumentId]) -> Option<DocumentId> {
    ids.iter().copied().min()
}

/// Give a freshly created tasks document the shape every Koru document has: a
/// `Kind::Document` root whose text is the title, and one empty paragraph.
///
/// The same seed `Workspace::seed_new_document` writes, and for the same
/// reason — a tasks document is an ordinary document that can be opened as a
/// tab, and one without a root has no title row and nothing to show.
fn seed_task_list(doc: &mut WorkspaceDoc) -> Result<(), koru_store_core::errors::Error> {
    if document_root(doc).is_some() {
        return Ok(());
    }
    let root = doc
        .insert(
            Parent::Root,
            Node::document(TASK_LIST_TITLE).map_err(shape)?,
        )
        .map_err(internal)?;
    doc.insert(Parent::Node(root), Node::paragraph("").map_err(shape)?)
        .map_err(internal)?;
    Ok(())
}

/// Append a native task to a document that is already a tasks document, and run
/// the canonical commands over it.
///
/// **One undo step for the whole capture.** The row is inserted *inside* the
/// [`tasks::compose`] bracket rather than before it: a Ctrl+Z after typing a
/// task means "un-type that task", and leaving an unmarked empty paragraph
/// behind would be a half-undone gesture nobody asked for.
///
/// **Nothing half-written.** A refusal inside a `compose` bracket leaves the
/// commands before it applied — that is what the bracket promises, and it is
/// the right answer for a gesture whose steps are each a person's choice. This
/// is not one of those: a rejected date would leave a task the caller was told
/// it did not create, and a retry would make a second. So the group is undone
/// when any step of it refuses, and the caller gets an error over an unchanged
/// document. It is a real `undo()` rather than a compensating write, and it
/// runs only when the row actually landed — an insert that never happened has
/// nothing in the group, and undoing *then* would take back whatever the person
/// did before this call.
///
/// Separate from [`Workspace::create_native_task`] because it names no store,
/// no manifest and no session — which is what makes it testable in memory, and
/// what lets a second host (an agent capture, later) reuse the write without
/// reusing the plumbing.
pub fn plant_native_task(
    doc: &mut WorkspaceDoc,
    title: &str,
    due: Option<Temporal>,
    list: Option<ListId>,
    priority: Option<Priority>,
) -> Result<(NodeId, TaskOutcome, Option<TaskOutcome>), koru_store_core::errors::Error> {
    plant_native_task_ranked(doc, title, due, list, priority, None)
}

/// [`plant_native_task`] with the task's place in its list's dragged order,
/// written in the same undo step as the rest of it.
pub fn plant_native_task_ranked(
    doc: &mut WorkspaceDoc,
    title: &str,
    due: Option<Temporal>,
    list: Option<ListId>,
    priority: Option<Priority>,
    rank: Option<i64>,
) -> Result<(NodeId, TaskOutcome, Option<TaskOutcome>), koru_store_core::errors::Error> {
    let root = document_root(doc).ok_or_else(|| {
        koru_store_core::errors::Error::new(
            koru_store_core::errors::Code::InvalidRequest,
            "that tasks document has no content root to write into",
        )
    })?;
    let mut planted = false;
    let out = tasks::compose(doc, |doc| {
        let node = doc
            .insert(Parent::Node(root), Node::paragraph(title).map_err(shape)?)
            .map_err(internal)?;
        planted = true;
        let created = tasks::ensure_task(doc, node, TaskReason::TasksView).map_err(internal)?;
        let dated = match due {
            Some(due) => Some(tasks::set_due(doc, node, due).map_err(internal)?),
            None => None,
        };
        if let Some(list) = list {
            // **Not `set_list`, and the difference is the undo step.**
            // `set_attribute_defined` opens the door's *own* group with a
            // `checkpoint()`, which closes this bracket — the membership
            // landed as a second Ctrl+Z and one undo left a listless task
            // behind. The `*_on_all` form is the in-group door: it validates
            // the same way and only `atomic`s, so it joins whatever group is
            // open. One node is a legitimate use of it.
            doc.set_attribute_on_all(&[node], KEY_LIST, AttrValue::Text(list.to_storage()))
                .map_err(internal)?;
        }
        if let Some(rank) = rank {
            // The in-group door again: one undo step takes the task back
            // whole, its place in the list with it.
            doc.set_attribute_on_all(
                &[node],
                koru_core::attributes::KEY_RANK,
                AttrValue::Number(rank as f64),
            )
            .map_err(internal)?;
        }
        if let Some(level) = priority {
            // The same in-group door, for the same reason: one undo step.
            doc.set_attribute_on_all(
                &[node],
                KEY_PRIORITY,
                AttrValue::Text(level.as_str().to_owned()),
            )
            .map_err(internal)?;
        }
        Ok((node, created, dated))
    });
    if out.is_err() && planted {
        let _ = doc.undo();
    }
    out
}

/// A document error, told as a store error a caller can act on.
///
/// **A refusal is not an internal failure.** The canonical commands validate
/// before they write, and the two refusals they can answer —
/// `TaskRejected` (SPEC §4's excluded kinds) and `AttributeRejected` (a due
/// that carries no zone) — are a caller's mistake, not this crate's. Reporting
/// them as `Internal` would make a wrong argument indistinguishable from a
/// corrupt document, which is the difference between "fix the call" and "the
/// workspace is broken". `TaskRefusal` travels as its **stable token**, because
/// that is what MCP maps and what a test asserts on ([ADR-286]).
///
/// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
fn internal(e: koru_core::StoreError) -> koru_store_core::errors::Error {
    use koru_core::StoreError as E;
    match e {
        E::TaskRejected { why, .. } => koru_store_core::errors::Error::new(
            koru_store_core::errors::Code::InvalidRequest,
            why.as_str(),
        ),
        E::AttributeRejected { key, why } => koru_store_core::errors::Error::new(
            koru_store_core::errors::Code::InvalidRequest,
            format!("{key}: {why}"),
        ),
        other => koru_store_core::errors::Error::new(
            koru_store_core::errors::Code::Internal,
            format!("the tasks document: {other}"),
        ),
    }
}

fn shape(e: koru_core::node::ShapeError) -> koru_store_core::errors::Error {
    koru_store_core::errors::Error::new(
        koru_store_core::errors::Code::Internal,
        format!("shaping the task row: {e}"),
    )
}

#[cfg(test)]
mod tests;
