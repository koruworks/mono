// SPDX-License-Identifier: MPL-2.0

//! The rules that decide *when* a document is projected.
//!
//! All of it against a fake store and a fake clock. That is the whole reason
//! [`Projector::tick`] is a tick rather than a thread: "twenty edits cost one
//! projection" checked with a sleep is a test that passes on a fast machine and
//! flakes on a busy one.

use std::cell::RefCell;
use std::collections::HashMap;

use super::*;
use crate::consumer::Counting;
use koru_core::node::Node;
use koru_core::schema::{Parent, WorkspaceDoc};

mod not_projected_tests;

type RemovedEntities = Vec<(WorkspaceId, DocumentId, Vec<String>)>;

#[derive(Clone, Default)]
struct EntityRecording(std::sync::Arc<std::sync::Mutex<RemovedEntities>>);

impl EntityRecording {
    fn events(&self) -> RemovedEntities {
        self.0.lock().unwrap().clone()
    }
}

impl Consumer for EntityRecording {
    fn name(&self) -> &str {
        "entities"
    }

    fn projected(&mut self, _: &crate::model::DocumentProjection) {}

    fn removed(&mut self, _: WorkspaceId, _: DocumentId) {}

    fn entities_removed(&mut self, workspace: WorkspaceId, document: DocumentId, nodes: &[String]) {
        self.0
            .lock()
            .unwrap()
            .push((workspace, document, nodes.to_vec()));
    }
}

/// A store that holds documents and hands out forks, with no lock and no disk.
#[derive(Default)]
struct FakeStore {
    docs: RefCell<HashMap<DocumentId, (WorkspaceId, WorkspaceDoc)>>,
    committed: RefCell<Vec<Committed>>,
    /// Documents whose fork should fail, so the failure path is reachable.
    broken: RefCell<Vec<DocumentId>>,
    /// The listing, as the manifest would hold it: a title per document and a
    /// name per workspace.
    titles: RefCell<HashMap<DocumentId, String>>,
    names: RefCell<HashMap<WorkspaceId, String>>,
    /// Workspaces whose listing cannot be read, so a sweep can fail honestly.
    unreadable: RefCell<Vec<WorkspaceId>>,
    /// Workspaces authoritatively removed from the Store.
    missing: RefCell<Vec<WorkspaceId>>,
    /// Documents the **manifest** has stamped as binned. Content and journal
    /// position are untouched, which is the whole of what binning is
    /// (ADR-95) — and the reason a bin reaches this layer as a listing change
    /// and never as a commit on the document.
    trashed: RefCell<HashMap<DocumentId, i64>>,
    /// The store's **durable** per-document stamp — the row in `workspace.db`
    /// that outlives `projection.db`. Deliberately empty unless a test says
    /// otherwise, so every other test in this file goes on exercising the
    /// fallback: a source that cannot answer leaves the projector on its own
    /// clock, which is what it had before this existed.
    modified: RefCell<HashMap<DocumentId, i64>>,
    /// The manifest kind per document, for the ones that are not a page — a
    /// delegated run, a schedule. Absent means `page`.
    kinds: RefCell<HashMap<DocumentId, String>>,
    /// Every fork handed out, so a test can say a document was never forked
    /// rather than infer it from what was projected.
    forks: RefCell<Vec<DocumentId>>,
    /// What the projector said it follows, tick by tick — the daemon enrols
    /// exactly this in automatic history.
    activity: RefCell<Vec<Committed>>,
}

impl FakeStore {
    /// A document of a kind the projection does not follow: a run, in the
    /// shape `koru-delegate-document` writes — a `run` map and a `timeline`
    /// list, no node tree.
    fn add_run(&self, workspace: WorkspaceId) -> DocumentId {
        let id = DocumentId::new();
        self.titles.borrow_mut().insert(id, "Run".to_owned());
        self.kinds
            .borrow_mut()
            .insert(id, koru_core::explore::KIND_RUN.to_owned());
        self.names
            .borrow_mut()
            .entry(workspace)
            .or_insert_with(|| "Acme".to_owned());
        let doc = loro::LoroDoc::new();
        doc.get_map("run").insert("schema", 1).unwrap();
        doc.commit();
        let ws = WorkspaceDoc::adopt(doc).unwrap();
        self.docs.borrow_mut().insert(id, (workspace, ws));
        id
    }

    /// A **reading document** (OCR TECHNICAL D9): a `readings` map of words
    /// per page, no node tree, registered under its own kind.
    fn add_reading(&self, workspace: WorkspaceId) -> DocumentId {
        let id = DocumentId::new();
        self.titles.borrow_mut().insert(id, String::new());
        self.kinds
            .borrow_mut()
            .insert(id, koru_core::explore::KIND_READING.to_owned());
        self.names
            .borrow_mut()
            .entry(workspace)
            .or_insert_with(|| "Acme".to_owned());
        let doc = loro::LoroDoc::new();
        doc.get_map("readings").insert("schema", 1).unwrap();
        doc.commit();
        let ws = WorkspaceDoc::adopt(doc).unwrap();
        self.docs.borrow_mut().insert(id, (workspace, ws));
        id
    }

    /// One page of words written into a reading document, one commit
    /// announced.
    fn read_page(&self, reading: DocumentId, page: i64) {
        self.edit(reading, |ws| {
            ws.doc()
                .get_map("readings")
                .insert(&page.to_string(), "the words on a page")
                .unwrap();
            ws.doc().commit();
        });
    }

    /// One step of a run: one value appended, one commit announced.
    fn step(&self, run: DocumentId, i: i64) {
        self.edit(run, |ws| {
            ws.doc().get_list("timeline").push(i).unwrap();
            ws.doc().commit();
        });
    }

    fn add(&self, workspace: WorkspaceId, text: &str) -> DocumentId {
        let id = DocumentId::new();
        self.titles.borrow_mut().insert(id, text.to_owned());
        self.names
            .borrow_mut()
            .entry(workspace)
            .or_insert_with(|| "Acme".to_owned());
        let mut ws = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "W").unwrap();
        let root = ws
            .insert(Parent::Root, Node::document("Doc").unwrap())
            .unwrap();
        ws.insert(Parent::Node(root), Node::paragraph(text).unwrap())
            .unwrap();
        self.docs.borrow_mut().insert(id, (workspace, ws));
        id
    }

    /// Edit a document and announce the commit, as the store would.
    fn write(&self, document: DocumentId, text: &str) {
        let mut docs = self.docs.borrow_mut();
        let (workspace, ws) = docs.get_mut(&document).expect("a document");
        let root = ws.children(Parent::Root).unwrap()[0];
        ws.insert(Parent::Node(root), Node::paragraph(text).unwrap())
            .unwrap();
        self.committed.borrow_mut().push(Committed {
            workspace: *workspace,
            document,
            seq: 1,
            version_vector: ws.version().encode(),
            is_manifest: false,
        });
    }

    /// Change a document's nodes in place and announce the commit, as the
    /// store would — what an attribute edit looks like from this layer.
    fn edit(&self, document: DocumentId, change: impl FnOnce(&mut WorkspaceDoc)) {
        let mut docs = self.docs.borrow_mut();
        let (workspace, ws) = docs.get_mut(&document).expect("a document");
        change(ws);
        self.committed.borrow_mut().push(Committed {
            workspace: *workspace,
            document,
            seq: 1,
            version_vector: ws.version().encode(),
            is_manifest: false,
        });
    }

    fn announce_manifest(&self, workspace: WorkspaceId) {
        self.committed.borrow_mut().push(Committed {
            workspace,
            document: DocumentId::new(),
            seq: 1,
            version_vector: b"manifest".to_vec(),
            is_manifest: true,
        });
    }

    /// Preserve identity while changing the authoritative workspace, then
    /// announce the target document commit as a real transfer does.
    fn move_to(&self, document: DocumentId, target: WorkspaceId) {
        self.names
            .borrow_mut()
            .entry(target)
            .or_insert_with(|| "Other".to_owned());
        let mut docs = self.docs.borrow_mut();
        let (workspace, ws) = docs.get_mut(&document).expect("a document");
        *workspace = target;
        self.committed.borrow_mut().push(Committed {
            workspace: target,
            document,
            seq: 2,
            version_vector: ws.version().encode(),
            is_manifest: false,
        });
    }

    fn drop_document(&self, document: DocumentId) {
        self.docs.borrow_mut().remove(&document);
        self.titles.borrow_mut().remove(&document);
        self.trashed.borrow_mut().remove(&document);
    }

    /// Bin a document: a stamp on its manifest entry, announced as a manifest
    /// commit. **Nothing about the document itself moves** — not its content,
    /// not its journal position — which is exactly why a bin must not look
    /// like a removal to a consumer.
    fn bin(&self, workspace: WorkspaceId, document: DocumentId, at: i64) {
        self.trashed.borrow_mut().insert(document, at);
        self.announce_manifest(workspace);
    }

    /// Take it back out again. The same manifest-shaped change, reversed.
    fn restore(&self, workspace: WorkspaceId, document: DocumentId) {
        self.trashed.borrow_mut().remove(&document);
        self.announce_manifest(workspace);
    }

    fn drop_workspace(&self, workspace: WorkspaceId) {
        let documents = self
            .docs
            .borrow()
            .iter()
            .filter_map(|(document, (held_by, _))| (*held_by == workspace).then_some(*document))
            .collect::<Vec<_>>();
        for document in documents {
            self.drop_document(document);
        }
        self.names.borrow_mut().remove(&workspace);
        self.missing.borrow_mut().push(workspace);
    }

    /// Set the durable stamp, as a commit or a checkpoint would.
    fn stamp(&self, document: DocumentId, at: i64) {
        self.modified.borrow_mut().insert(document, at);
    }

    fn rename(&self, document: DocumentId, title: &str) {
        self.titles.borrow_mut().insert(document, title.to_owned());
    }

    fn set_schema(&self, document: DocumentId, version: i64) {
        let mut docs = self.docs.borrow_mut();
        let (workspace, ws) = docs.get_mut(&document).expect("a document");
        ws.doc()
            .get_map("meta")
            .insert("schema_version", version)
            .unwrap();
        ws.doc().commit();
        self.committed.borrow_mut().push(Committed {
            workspace: *workspace,
            document,
            seq: 1,
            version_vector: ws.version().encode(),
            is_manifest: false,
        });
    }
}

impl Source for &FakeStore {
    fn fork(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> std::result::Result<loro::LoroDoc, String> {
        self.forks.borrow_mut().push(document);
        if self.broken.borrow().contains(&document) {
            return Err("this one is broken".into());
        }
        let docs = self.docs.borrow();
        let (held_in, ws) = docs.get(&document).ok_or("no such document")?;
        if *held_in != workspace {
            return Err("document_not_found".into());
        }
        // The real one forks; exporting and importing is the same *content*,
        // which is all this fake owes the test.
        let doc = loro::LoroDoc::new();
        doc.import(&ws.export_snapshot().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        Ok(doc)
    }

    fn revision(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> std::result::Result<Vec<u8>, String> {
        let docs = self.docs.borrow();
        let (held_in, ws) = docs.get(&document).ok_or("no such document")?;
        if *held_in != workspace {
            return Err("document_not_found".into());
        }
        Ok(ws.version().encode())
    }

    fn positions(
        &self,
        workspace: WorkspaceId,
    ) -> std::result::Result<Vec<(DocumentId, Vec<u8>)>, PositionsError> {
        if self.missing.borrow().contains(&workspace) {
            return Err(PositionsError::WorkspaceNotFound);
        }
        if self.unreadable.borrow().contains(&workspace) {
            return Err(PositionsError::Other(
                "this workspace will not say what it holds".into(),
            ));
        }
        Ok(self
            .docs
            .borrow()
            .iter()
            .filter(|(_, (w, _))| *w == workspace)
            .map(|(d, (_, ws))| (*d, ws.version().encode()))
            .collect())
    }

    fn modified_at(
        &self,
        _workspace: WorkspaceId,
        document: DocumentId,
    ) -> std::result::Result<Option<i64>, String> {
        Ok(self.modified.borrow().get(&document).copied())
    }

    fn entries(&self, workspace: WorkspaceId) -> std::result::Result<Vec<ListedDocument>, String> {
        if self.unreadable.borrow().contains(&workspace) {
            return Err("this workspace will not say what it holds".into());
        }
        let titles = self.titles.borrow();
        let trashed = self.trashed.borrow();
        let kinds = self.kinds.borrow();
        let mut out = self
            .docs
            .borrow()
            .iter()
            .filter(|(_, (w, _))| *w == workspace)
            .map(|(d, _)| ListedDocument {
                document: *d,
                parent: None,
                ordinal: 0,
                title: titles.get(d).cloned().unwrap_or_default(),
                kind: kinds.get(d).cloned().unwrap_or_else(|| "page".into()),
                mime: None,
                trashed_at: trashed.get(d).copied(),
                trashed_by: trashed.get(d).map(|_| "Tester".to_owned()),
            })
            .collect::<Vec<_>>();
        out.sort_by_key(|d| d.document.to_storage());
        Ok(out)
    }

    fn workspaces(&self) -> std::result::Result<Vec<(WorkspaceId, String)>, String> {
        Ok(self
            .names
            .borrow()
            .iter()
            .map(|(id, name)| (*id, name.clone()))
            .collect())
    }

    fn take_committed(&self) -> Vec<Committed> {
        std::mem::take(&mut self.committed.borrow_mut())
    }

    fn record_activity(&self, followed: &[Committed], _now_millis: i64) {
        self.activity.borrow_mut().extend_from_slice(followed);
    }
}

/// A projector already asked to sweep one workspace.
///
/// **Explicit, because nothing sweeps at construction any more.** A workspace
/// is swept when it shows its first commit or when somebody asks — so a test
/// that wants a starting projection has to be one of those two things, and
/// saying which is the point.
fn projector_for(store: &FakeStore, ws: WorkspaceId) -> Projector<&FakeStore> {
    let mut p = Projector::new(store, Projections::in_memory().unwrap());
    p.request_sweep(ws);
    p
}

/// Run until nothing is left to do, or fail rather than spin forever.
///
/// **The clock only moves when it has to.** A commit drained at time T is
/// deliberately not projected at time T — that is the debounce working — so a
/// helper that ticked at one fixed instant would wait forever and read as a bug
/// in the projector rather than in the test. Advancing past the quiet window
/// only once nothing else is left to do keeps every other assertion about
/// *when* honest.
fn settle(p: &mut Projector<&FakeStore>, now: i64) {
    let mut now = now;
    for _ in 0..1000 {
        if p.tick(now) {
            continue;
        }
        if p.stats().pending > 0 {
            now += QUIET_MILLIS + 1;
            continue;
        }
        return;
    }
    panic!("the projector never settled");
}

// ------------------------------------------------------------------ debounce

/// **Twenty edits in a second cost one projection.** The projection re-reads
/// state rather than replaying what it was told, so anything else is work for
/// nothing.
#[test]
fn many_commits_before_the_worker_runs_cost_one_projection() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    let doc = store.add(ws, "start");
    let mut p = projector_for(&store, ws);
    settle(&mut p, 0);
    let before = p.stats().projected;

    for i in 0..20 {
        store.write(doc, &format!("line {i}"));
        p.tick(10 + i as i64);
    }
    assert_eq!(
        p.stats().projected,
        before,
        "it projected while the document was still being typed into"
    );

    settle(&mut p, 10 + QUIET_MILLIS);
    assert_eq!(
        p.stats().projected,
        before + 1,
        "one projection, not twenty"
    );
}

/// A transfer preserves DocumentId. If it lands while an edit is still in the
/// debounce queue, the target commit must replace both the queued revision and
/// its workspace; otherwise the projector forks the stable id from the source
/// after the Store has already removed it there.
#[test]
fn a_move_before_debounce_projects_from_the_new_workspace() {
    let store = FakeStore::default();
    let source = WorkspaceId::new();
    let target = WorkspaceId::new();
    let document = store.add(source, "before");
    let mut projector = Projector::new(&store, Projections::in_memory().unwrap());

    store.write(document, "edited before move");
    store.move_to(document, target);
    settle(&mut projector, 0);

    let projection = projector
        .projections()
        .read(document)
        .unwrap()
        .expect("the moved document was projected");
    assert_eq!(projection.workspace, target);
    assert_eq!(projector.stats().failed, 0);
}

/// Continuous typing must not hold a document back forever, or somebody who
/// never pauses is never searchable.
#[test]
fn continuous_typing_still_lands_at_the_ceiling() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    let doc = store.add(ws, "start");
    let mut p = projector_for(&store, ws);
    settle(&mut p, 0);
    let before = p.stats().projected;

    // A commit every 100 ms, so the quiet window never elapses.
    let mut now = 1_000;
    for _ in 0..(CEILING_MILLIS / 100) + 1 {
        store.write(doc, "more");
        p.tick(now);
        now += 100;
    }

    assert!(
        p.stats().projected > before,
        "the ceiling never fired; a document being typed into is never projected"
    );
}

// ------------------------------------------------------------------ the walk

#[test]
fn a_projected_document_can_be_read_back() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    let doc = store.add(ws, "hello");
    let mut p = projector_for(&store, ws);
    settle(&mut p, 0);

    let projection = p.projections().read(doc).unwrap().expect("a projection");
    assert!(projection.nodes.iter().any(|n| n.text == "hello"));
    assert_eq!(projection.workspace, ws);
}

/// **One document, always.** Editing one must not read or rewrite another —
/// this is the invariant the whole layer is built to hold.
#[test]
fn projecting_one_document_leaves_the_others_alone() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    let one = store.add(ws, "one");
    let two = store.add(ws, "two");
    let mut p = projector_for(&store, ws);
    settle(&mut p, 0);

    let untouched = p.projections().revision(two).unwrap();
    let before = p.stats().projected;

    store.write(one, "changed");
    settle(&mut p, QUIET_MILLIS * 4);

    assert_eq!(
        p.stats().projected,
        before + 1,
        "more than one document was projected for one edit"
    );
    assert_eq!(p.projections().revision(two).unwrap(), untouched);
}

#[test]
fn a_too_new_source_is_recorded_without_publishing_a_partial_projection() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    let doc = store.add(ws, "understood");
    let mut p = projector_for(&store, ws);
    settle(&mut p, 0);
    let compatible = p.projections().read(doc).unwrap().unwrap();
    let projected = p.stats().projected;

    store.set_schema(doc, koru_core::schema::SCHEMA_VERSION + 1);
    settle(&mut p, QUIET_MILLIS * 4);

    assert_eq!(p.stats().projected, projected);
    assert_eq!(p.stats().incompatible, 1);
    assert_eq!(p.projections().read(doc).unwrap().unwrap(), compatible);
    assert!(matches!(
        p.projections().source_status(doc).unwrap(),
        Some(crate::model::ProjectionSourceStatus::SchemaTooNew { found, .. })
            if found == koru_core::schema::SCHEMA_VERSION + 1
    ));
}

// -------------------------------------------------------------------- sweeps

/// A document committed while the projector was stopped is caught by comparing
/// revisions on the next start — **not** by a full rebuild, and not by having
/// been told.
#[test]
fn a_document_changed_while_stopped_is_caught_by_comparison() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    let doc = store.add(ws, "before");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("projection.db");

    {
        let mut p = Projector::new(&store, Projections::at(&path).unwrap());
        p.request_sweep(ws);
        settle(&mut p, 0);
    }

    // The store moves on with nobody listening. The event is dropped on the
    // floor, exactly as a crash would drop it.
    store.write(doc, "while it was away");
    let _ = Source::take_committed(&&store);

    // A restart. Nothing is swept until the workspace shows life — so this is
    // the commit that arrives *after* the missed one, which is what a real
    // client reconnecting produces.
    let mut p = Projector::new(&store, Projections::at(&path).unwrap());
    store.write(doc, "and then this");
    settle(&mut p, 0);

    let projection = p.projections().read(doc).unwrap().unwrap();
    assert!(
        projection
            .nodes
            .iter()
            .any(|n| n.text == "while it was away"),
        "the missed commit was never noticed"
    );
}

/// The other half: a sweep over a store nothing has changed must project
/// nothing at all, or every restart would rewrite the whole projection.
#[test]
fn a_sweep_over_an_unchanged_store_projects_nothing() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    for i in 0..5 {
        store.add(ws, &format!("doc {i}"));
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("projection.db");

    {
        let mut p = Projector::new(&store, Projections::at(&path).unwrap());
        p.request_sweep(ws);
        settle(&mut p, 0);
        assert_eq!(p.stats().projected, 5);
    }

    let mut p = Projector::new(&store, Projections::at(&path).unwrap());
    p.request_sweep(ws);
    settle(&mut p, 0);
    assert_eq!(
        p.stats().projected,
        0,
        "a restart re-projected documents nothing had touched"
    );
    assert_eq!(p.stats().sweeps, 1, "but it did sweep");
}

/// A deleted projection is rebuilt — the same mechanism, with nothing to
/// compare against.
#[test]
fn a_deleted_projection_is_rebuilt() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    for i in 0..3 {
        store.add(ws, &format!("doc {i}"));
    }
    let mut p = projector_for(&store, ws);
    settle(&mut p, 0);

    // A fresh projection over the same store: everything is missing.
    let mut p = projector_for(&store, ws);
    settle(&mut p, 0);
    assert_eq!(p.stats().projected, 3);
    assert_eq!(p.projections().documents().unwrap(), 3);
}

/// **Batched and resumable.** A sweep that had to finish in one call could not
/// be interrupted, and a rebuild over a large store would be a stall.
#[test]
fn a_sweep_is_batched_rather_than_done_in_one_go() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    for i in 0..(SWEEP_BATCH * 2 + 5) {
        store.add(ws, &format!("doc {i}"));
    }
    let mut p = projector_for(&store, ws);

    p.tick(0);
    let after_one = p.stats().projected as usize;
    assert!(
        after_one <= SWEEP_BATCH,
        "one tick projected {after_one}, which is not a batch"
    );
    assert!(after_one > 0, "a tick did nothing at all");

    settle(&mut p, 0);
    assert_eq!(p.projections().documents().unwrap(), SWEEP_BATCH * 2 + 5);
}

/// A document the store no longer holds loses its projection. Noticed by the
/// sweep, because nothing sends an event for something that is gone.
#[test]
fn a_dropped_document_loses_its_projection() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    let doc = store.add(ws, "here");
    let mut p = projector_for(&store, ws);
    settle(&mut p, 0);
    assert!(p.projections().read(doc).unwrap().is_some());

    store.drop_document(doc);
    p.request_sweep(ws);
    settle(&mut p, 0);

    assert!(p.projections().read(doc).unwrap().is_none());
    assert_eq!(p.stats().removed, 1);
}

// ----------------------------------------------------------------- consumers

#[test]
fn a_consumer_is_told_what_was_projected() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    let doc = store.add(ws, "hello");
    let mut p = projector_for(&store, ws);
    p.add_consumer(Box::new(Counting::default()));
    settle(&mut p, 0);

    // Its cursor is its own; this reads it back the way its owner would.
    assert_eq!(p.stats().projected, 1);
    let revision = p.projections().revision(doc).unwrap().unwrap();
    assert!(!revision.is_empty());
}

#[test]
fn entity_removal_requires_two_successful_current_projections() {
    let store = FakeStore::default();
    let workspace = WorkspaceId::new();
    let document = store.add(workspace, "task");
    let removed_node = {
        let docs = store.docs.borrow();
        let (_, ws) = docs.get(&document).unwrap();
        let root = ws.children(Parent::Root).unwrap()[0];
        ws.children(Parent::Node(root)).unwrap()[0]
    };
    let mut projector = projector_for(&store, workspace);
    let recording = EntityRecording::default();
    projector.add_consumer(Box::new(recording.clone()));
    settle(&mut projector, 0);
    assert!(
        recording.events().is_empty(),
        "cold projection inferred deletion"
    );

    store.edit(document, |ws| ws.delete(removed_node).unwrap());
    store.broken.borrow_mut().push(document);
    assert!(!projector.tick(10_000));
    assert!(projector.tick(10_000 + QUIET_MILLIS + 1));
    assert!(
        recording.events().is_empty(),
        "failed projection inferred deletion"
    );

    store.broken.borrow_mut().clear();
    assert!(projector.tick(10_000 + QUIET_MILLIS * 3 + 2));
    assert_eq!(
        recording.events(),
        vec![(workspace, document, vec![removed_node.to_storage()])]
    );

    store.write(document, "unrelated");
    settle(&mut projector, 20_000);
    assert_eq!(
        recording.events().len(),
        1,
        "unchanged absence was repeated"
    );
}

/// **The manifest is not a Koru document.** Nothing walks it — but a consumer
/// that joins against titles or the trash has to know the listing moved.
#[test]
fn a_manifest_commit_is_a_listing_change_and_not_a_projection() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    let doc = store.add(ws, "Roadmap");
    let mut p = projector_for(&store, ws);
    settle(&mut p, 0);
    let before = p.stats().projected;

    store.rename(doc, "Plans");
    store.announce_manifest(ws);
    settle(&mut p, QUIET_MILLIS * 4);

    assert_eq!(
        p.stats().projected,
        before,
        "something tried to walk the manifest as if it were a page"
    );
    assert_eq!(p.stats().failed, 0);
    let listing = p.projections().listing(ws).unwrap().expect("a listing");
    assert_eq!(listing.documents[0].title, "Plans");
}

// ------------------------------------------------------------------- listings

/// **A listing that did not move costs nothing downstream.** A manifest commit
/// happens for a great many reasons; only some of them change what a consumer
/// joins against, and the digest is what tells them apart.
#[test]
fn a_manifest_commit_that_changed_nothing_tells_no_consumer() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    store.add(ws, "Roadmap");
    let mut p = projector_for(&store, ws);
    settle(&mut p, 0);
    let after_first = p.stats().listings;
    assert!(after_first > 0, "the first sweep never mirrored a listing");

    store.announce_manifest(ws);
    settle(&mut p, QUIET_MILLIS * 4);

    assert_eq!(
        p.stats().listings,
        after_first,
        "an unchanged listing was republished"
    );
}

/// The listing is reconcilable, not only announced: a rename that happened
/// while this was stopped is found by the sweep, the same way a missed commit
/// is.
#[test]
fn a_rename_missed_while_stopped_is_caught_by_the_sweep() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    let doc = store.add(ws, "Roadmap");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("projection.db");

    {
        let mut p = Projector::new(&store, Projections::at(&path).unwrap());
        p.request_sweep(ws);
        settle(&mut p, 0);
    }

    store.rename(doc, "Plans");
    let _ = Source::take_committed(&&store);

    let mut p = Projector::new(&store, Projections::at(&path).unwrap());
    p.request_sweep(ws);
    settle(&mut p, 0);

    let listing = p.projections().listing(ws).unwrap().expect("a listing");
    assert_eq!(listing.documents[0].title, "Plans");
}

// --------------------------------------------------------------- the handshake

/// **The consumer's way back in.** A query about to trust a workspace nobody
/// has touched since the store came up asks for it to be compared, and finds
/// out when it has been.
#[test]
fn a_consumer_can_ask_for_a_sweep_and_see_it_finish() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    store.add(ws, "here");
    let mut p = Projector::new(&store, Projections::in_memory().unwrap());
    let sweeps = p.sweeps();

    assert_eq!(
        sweeps.finished(ws),
        0,
        "a workspace nothing has compared must not read as current"
    );

    sweeps.request(ws);
    settle(&mut p, 0);

    assert_eq!(sweeps.finished(ws), 1);
    assert_eq!(p.projections().documents().unwrap(), 1);
}

/// A reconciliation request has a shorter wait bound than the daemon's idle
/// projection poll. The request is therefore a wake, not only a flag the next
/// poll may eventually notice.
#[test]
fn a_consumer_request_wakes_an_idle_projection_worker() {
    let sweeps = Sweeps::default();
    let waiter = sweeps.clone();
    let workspace = WorkspaceId::new();
    let ready = std::sync::Arc::new(std::sync::Barrier::new(2));
    let worker_ready = ready.clone();
    let worker = std::thread::spawn(move || {
        worker_ready.wait();
        let started = std::time::Instant::now();
        let requested = waiter.wait_for_request(std::time::Duration::from_secs(2));
        (requested, started.elapsed())
    });

    ready.wait();
    sweeps.request(workspace);
    let (requested, elapsed) = worker.join().unwrap();

    assert!(requested);
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "the request waited for the idle timeout instead of waking the worker: {elapsed:?}"
    );
}

/// **A sweep that could not read the store did not happen.** Counting it would
/// tell a consumer the workspace had been compared against a store that never
/// answered — which is the whole failure the handshake exists to prevent, and
/// it would be silent.
#[test]
fn a_sweep_the_store_would_not_answer_is_not_reported_as_finished() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    store.add(ws, "here");
    store.unreadable.borrow_mut().push(ws);

    let mut p = Projector::new(&store, Projections::in_memory().unwrap());
    let sweeps = p.sweeps();
    sweeps.request(ws);
    settle(&mut p, 0);

    assert_eq!(sweeps.finished(ws), 0);
    assert!(p.stats().last_error.is_some(), "it failed quietly");
}

#[test]
fn a_completion_before_a_new_enqueue_cannot_satisfy_the_new_ticket() {
    let sweeps = Sweeps::default();
    let workspace = WorkspaceId::new();
    let first = sweeps.request(workspace);
    assert!(sweeps.take_request_and_record_for_test(workspace));
    assert_eq!(sweeps.finished(workspace), first);

    let second = sweeps.request(workspace);
    assert!(second > first);
    assert!(sweeps.finished(workspace) < second);
}

#[test]
fn a_failed_document_does_not_complete_its_sweep_ticket() {
    let store = FakeStore::default();
    let workspace = WorkspaceId::new();
    let document = store.add(workspace, "before");
    let mut projector = projector_for(&store, workspace);
    settle(&mut projector, 0);
    let completed = projector.sweeps().finished(workspace);

    store.write(document, "newer");
    let _ = Source::take_committed(&&store);
    store.broken.borrow_mut().push(document);
    let ticket = projector.sweeps().request(workspace);
    settle(&mut projector, 10_000);

    assert!(ticket > completed);
    assert_eq!(projector.sweeps().finished(workspace), completed);
    assert_eq!(
        projector
            .projections()
            .read(document)
            .unwrap()
            .unwrap()
            .nodes
            .iter()
            .filter(|node| node.text == "newer")
            .count(),
        0,
        "the failed source revision became visible"
    );
}

#[test]
fn a_failed_sweep_document_stays_before_the_restart_cursor() {
    let store = FakeStore::default();
    let workspace = WorkspaceId::new();
    let mut documents = (0..(SWEEP_BATCH + 1))
        .map(|index| store.add(workspace, &format!("document {index}")))
        .collect::<Vec<_>>();
    documents.sort_by_key(|document| document.to_storage());
    let failed = documents[0];
    let mut projector = Projector::new(&store, Projections::in_memory().unwrap());
    projector.request_sweep(workspace);
    settle(&mut projector, 0);
    let mut projections = projector.into_projections_for_test();
    store.write(failed, "must survive restart");
    let _ = Source::take_committed(&&store);
    store.broken.borrow_mut().push(failed);

    let initial_cursor = format!("{}|", workspace.to_storage());
    projections
        .set_state(K_SWEEP_CURSOR, &initial_cursor)
        .unwrap();
    let mut projector = Projector::new(&store, projections);
    projector.request_sweep(workspace);
    assert!(projector.tick(10_000));
    assert_eq!(
        projector
            .projections()
            .state(K_SWEEP_CURSOR)
            .unwrap()
            .as_deref(),
        Some(initial_cursor.as_str())
    );
    let projections = projector.into_projections_for_test();

    store
        .broken
        .borrow_mut()
        .retain(|document| *document != failed);
    let mut restarted = Projector::new(&store, projections);
    restarted.request_sweep(workspace);
    settle(&mut restarted, 20_000);
    assert!(
        restarted
            .projections()
            .read(failed)
            .unwrap()
            .unwrap()
            .nodes
            .iter()
            .any(|node| node.text == "must survive restart")
    );
}

#[test]
fn a_cursor_from_another_workspace_or_legacy_build_cannot_skip_documents() {
    for cursor in [
        format!(
            "{}|ffffffff-ffff-ffff-ffff-ffffffffffff",
            WorkspaceId::new().to_storage()
        ),
        "ffffffff-ffff-ffff-ffff-ffffffffffff".to_owned(),
    ] {
        let store = FakeStore::default();
        let workspace = WorkspaceId::new();
        let document = store.add(workspace, "must be projected");
        let mut projections = Projections::in_memory().unwrap();
        projections.set_state(K_SWEEP_CURSOR, &cursor).unwrap();
        let mut projector = Projector::new(&store, projections);
        let ticket = projector.sweeps().request(workspace);

        settle(&mut projector, 0);

        assert_eq!(projector.sweeps().finished(workspace), ticket);
        assert!(projector.projections().read(document).unwrap().is_some());
    }
}

/// A missing workspace is not a transient read failure. Its stale projection
/// must be retired and the comparison completed, or every search query asks
/// for the same impossible sweep and waits for the full handshake timeout.
#[test]
fn a_missing_workspace_is_retired_and_reported_as_finished() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    store.add(ws, "gone later");
    let mut p = projector_for(&store, ws);
    settle(&mut p, 0);
    assert_eq!(p.projections().documents().unwrap(), 1);
    assert!(p.projections().listing(ws).unwrap().is_some());

    store.drop_workspace(ws);
    let sweeps = p.sweeps();
    sweeps.request(ws);
    settle(&mut p, 1);

    assert_eq!(sweeps.finished(ws), 2);
    assert_eq!(p.projections().documents().unwrap(), 0);
    assert!(p.projections().listing(ws).unwrap().is_none());
    assert_eq!(p.stats().removed, 1);
}

// ----------------------------------------------------------- attribute events

mod attribute_events {
    use super::*;
    use crate::events::{AttributeChange, AttributeEvent, AttributeEventKind};
    use chrono::NaiveDate;
    use koru_core::NodeId;
    use koru_core::attributes::{KEY_DUE, KEY_PRIORITY};
    use koru_core::ids::TagId;
    use koru_core::node::AttrValue;

    /// The body paragraph every [`FakeStore::add`] document carries.
    fn body_node(ws: &WorkspaceDoc) -> NodeId {
        let root = ws.children(Parent::Root).unwrap()[0];
        ws.children(Parent::Node(root)).unwrap()[0]
    }

    /// A consumer that shares its record — the projector owns its consumers as
    /// boxes, and a test still has to read what one was told.
    #[derive(Default, Clone)]
    struct Recording(std::sync::Arc<std::sync::Mutex<Vec<AttributeEvent>>>);

    impl Recording {
        fn events(&self) -> Vec<AttributeEvent> {
            self.0.lock().unwrap().clone()
        }

        fn names(&self) -> Vec<&'static str> {
            self.events().iter().map(|e| e.kind.name()).collect()
        }
    }

    impl Consumer for Recording {
        fn name(&self) -> &str {
            "recording"
        }

        fn projected(&mut self, _: &crate::model::DocumentProjection) {}

        fn removed(&mut self, _: WorkspaceId, _: DocumentId) {}

        fn attribute_events(&mut self, events: &[AttributeEvent]) {
            self.0.lock().unwrap().extend_from_slice(events);
        }
    }

    fn date(y: i32, m: u32, d: u32) -> AttrValue {
        AttrValue::Date(koru_core::time::DateOnly::new(
            NaiveDate::from_ymd_opt(y, m, d).unwrap(),
        ))
    }

    /// The whole lifecycle, in the exact delivered sequence: set is `Set`, a
    /// move is `Changed`, removal is `tag.removed` / `Cleared` — one
    /// projection's events at a time, in the order the diff promises.
    #[test]
    fn attribute_edits_come_back_as_semantic_events_in_order() {
        let store = FakeStore::default();
        let ws = WorkspaceId::new();
        let doc = store.add(ws, "task");
        let mut p = projector_for(&store, ws);
        let recorder = Recording::default();
        p.add_consumer(Box::new(recorder.clone()));
        settle(&mut p, 0);
        assert_eq!(
            recorder.events(),
            vec![],
            "a document with no attrs announced something"
        );

        let tag = TagId::new();
        store.edit(doc, |ws| {
            let node = body_node(ws);
            ws.add_tag(node, tag).unwrap();
            ws.set_attribute(node, KEY_DUE, date(2026, 8, 30)).unwrap();
        });
        settle(&mut p, 10_000);
        assert_eq!(recorder.names(), ["due.changed", "tag.added"]);
        assert!(matches!(
            &recorder.events()[0].kind,
            AttributeEventKind::DueChanged {
                change: AttributeChange::Set { value }
            } if value.kind == "date" && value.value == "2026-08-30"
        ));
        assert_eq!(
            recorder.events()[1].kind,
            AttributeEventKind::TagAdded {
                tag: tag.to_storage()
            }
        );

        store.edit(doc, |ws| {
            let node = body_node(ws);
            ws.set_attribute(node, KEY_DUE, date(2026, 9, 2)).unwrap();
        });
        settle(&mut p, 20_000);
        assert_eq!(
            recorder.names(),
            ["due.changed", "tag.added", "due.changed"]
        );
        assert!(matches!(
            &recorder.events()[2].kind,
            AttributeEventKind::DueChanged {
                change: AttributeChange::Changed { value }
            } if value.value == "2026-09-02"
        ));

        store.edit(doc, |ws| {
            let node = body_node(ws);
            ws.remove_tag(node, tag).unwrap();
            ws.clear_attribute(node, KEY_DUE).unwrap();
        });
        settle(&mut p, 30_000);
        assert_eq!(
            recorder.names(),
            [
                "due.changed",
                "tag.added",
                "due.changed",
                "due.changed",
                "tag.removed"
            ]
        );
        assert_eq!(
            recorder.events()[3].kind,
            AttributeEventKind::DueChanged {
                change: AttributeChange::Cleared
            }
        );
        assert_eq!(
            recorder.events()[4].kind,
            AttributeEventKind::TagRemoved {
                tag: tag.to_storage()
            }
        );

        // The entity travelled whole on every one of them.
        for event in recorder.events() {
            assert_eq!(event.workspace, ws);
            assert_eq!(event.document, doc);
            assert!(!event.node.is_empty());
        }
    }

    /// A reprojection is not an event source: only an attr that moved is.
    #[test]
    fn a_reprojection_that_moved_no_attr_emits_nothing() {
        let store = FakeStore::default();
        let ws = WorkspaceId::new();
        let doc = store.add(ws, "task");
        let mut p = projector_for(&store, ws);
        let recorder = Recording::default();
        p.add_consumer(Box::new(recorder.clone()));
        store.edit(doc, |ws| {
            let node = body_node(ws);
            ws.set_attribute(node, KEY_PRIORITY, AttrValue::Text("high".into()))
                .unwrap();
        });
        settle(&mut p, 0);
        let told = recorder.events().len();
        assert!(told > 0, "the set itself was never announced");

        store.write(doc, "more prose, no attrs");
        settle(&mut p, 10_000);

        assert_eq!(
            recorder.events().len(),
            told,
            "a text edit re-announced attributes nothing moved"
        );
    }

    /// First sight includes the attrs a document already carries: a consumer
    /// that subscribed after the attribute was written still needs the
    /// trigger, and the reconcile path re-projects exactly such documents.
    #[test]
    fn first_sight_of_a_document_with_attrs_is_set_events() {
        let store = FakeStore::default();
        let ws = WorkspaceId::new();
        let doc = store.add(ws, "task");
        store.edit(doc, |ws| {
            let node = body_node(ws);
            ws.set_attribute(node, KEY_DUE, date(2026, 8, 30)).unwrap();
        });

        let mut p = projector_for(&store, ws);
        let recorder = Recording::default();
        p.add_consumer(Box::new(recorder.clone()));
        settle(&mut p, 0);

        assert_eq!(recorder.names(), ["due.changed"]);
        assert!(matches!(
            &recorder.events()[0].kind,
            AttributeEventKind::DueChanged {
                change: AttributeChange::Set { .. }
            }
        ));
    }

    #[test]
    fn a_transient_direct_projection_failure_retries_and_emits_due_once() {
        let store = FakeStore::default();
        let workspace = WorkspaceId::new();
        let document = store.add(workspace, "task");
        let mut projector = projector_for(&store, workspace);
        let recorder = Recording::default();
        projector.add_consumer(Box::new(recorder.clone()));
        settle(&mut projector, 0);

        store.edit(document, |ws| {
            let node = body_node(ws);
            ws.set_attribute(node, KEY_DUE, date(2026, 9, 3)).unwrap();
        });
        store.broken.borrow_mut().push(document);
        assert!(
            !projector.tick(10_000),
            "the commit should only arm debounce"
        );
        assert!(projector.tick(10_000 + QUIET_MILLIS + 1));
        assert_eq!(projector.stats().pending, 1, "the failed wake was dropped");
        assert!(recorder.events().is_empty());

        store.broken.borrow_mut().clear();
        assert!(projector.tick(10_000 + QUIET_MILLIS * 3 + 2));
        assert_eq!(projector.stats().pending, 0);
        assert_eq!(recorder.names(), ["due.changed"]);
    }

    /// **Binning clears nothing; deleting clears everything** (decision D-d).
    ///
    /// The two look alike from a list and are opposites here. A binned
    /// document keeps its content, its projection and its attributes — the
    /// bin is a place somebody searches and restores from (ADR-69/95) — so a
    /// clearing event would tell a notifier the deadline is gone when it is
    /// one click from coming back. Suppressing a reminder for binned work is
    /// the notifier's own filter over the trashed flag the rows already carry.
    /// A **permanent** deletion is the other case: the rows are going, so
    /// every attribute is cleared in the same vocabulary an edit uses, or a
    /// consumer keeps a trigger for an entity nothing can resolve.
    #[test]
    fn binning_says_nothing_and_deleting_clears_everything() {
        let store = FakeStore::default();
        let ws = WorkspaceId::new();
        let doc = store.add(ws, "task");
        let tag = TagId::new();
        store.edit(doc, |ws| {
            let node = body_node(ws);
            ws.add_tag(node, tag).unwrap();
            ws.set_attribute(node, KEY_DUE, date(2026, 8, 30)).unwrap();
        });
        let mut p = projector_for(&store, ws);
        let recorder = Recording::default();
        p.add_consumer(Box::new(recorder.clone()));
        settle(&mut p, 0);
        let told = recorder.events().len();
        assert!(told > 0, "the attributes were never announced at all");

        // ---- Binned. A manifest stamp, and nothing else.
        store.bin(ws, doc, 9);
        settle(&mut p, 10_000);

        assert_eq!(
            recorder.events().len(),
            told,
            "binning a document announced an attribute change"
        );
        let attrs = p.projections().document_attrs(doc).unwrap();
        assert_eq!(
            attrs.len(),
            2,
            "a binned document lost the attributes it still holds"
        );
        assert!(
            p.projections()
                .attribute_rows(&crate::store::AttributeRowQuery {
                    limit: 10,
                    ..Default::default()
                })
                .unwrap()
                .iter()
                .all(|row| row.trashed),
            "the row is flagged rather than hidden, which is what a notifier filters on"
        );

        // ---- Restored. Still nothing to say, and nothing to put back.
        store.restore(ws, doc);
        settle(&mut p, 20_000);

        assert_eq!(
            recorder.events().len(),
            told,
            "restoring re-announced attributes that never went anywhere"
        );
        assert_eq!(p.projections().document_attrs(doc).unwrap().len(), 2);
        assert!(
            p.projections()
                .attribute_rows(&crate::store::AttributeRowQuery {
                    limit: 10,
                    ..Default::default()
                })
                .unwrap()
                .iter()
                .all(|row| !row.trashed),
            "a restore brings the reminders back, because they never stopped standing"
        );

        // ---- Deleted for good. Now every attribute is cleared.
        store.drop_document(doc);
        p.request_sweep(ws);
        settle(&mut p, 30_000);

        let after: Vec<_> = recorder.events().split_off(told);
        assert_eq!(
            after.iter().map(|e| e.kind.name()).collect::<Vec<_>>(),
            ["due.changed", "tag.removed"]
        );
        assert_eq!(
            after[0].kind,
            AttributeEventKind::DueChanged {
                change: AttributeChange::Cleared
            }
        );
        assert_eq!(
            after[1].kind,
            AttributeEventKind::TagRemoved {
                tag: tag.to_storage()
            }
        );
        assert!(p.projections().document_attrs(doc).unwrap().is_empty());
    }

    /// A dropped document's attrs are cleared to whoever tracks them, in the
    /// same vocabulary an edit uses — after `removed`, like every delivery.
    #[test]
    fn a_dropped_document_clears_its_attrs() {
        let store = FakeStore::default();
        let ws = WorkspaceId::new();
        let doc = store.add(ws, "task");
        let tag = TagId::new();
        store.edit(doc, |ws| {
            let node = body_node(ws);
            ws.add_tag(node, tag).unwrap();
            ws.set_attribute(node, KEY_DUE, date(2026, 8, 30)).unwrap();
        });
        let mut p = projector_for(&store, ws);
        let recorder = Recording::default();
        p.add_consumer(Box::new(recorder.clone()));
        settle(&mut p, 0);
        let told = recorder.events().len();

        store.drop_document(doc);
        p.request_sweep(ws);
        settle(&mut p, 10_000);

        let after: Vec<_> = recorder.events().split_off(told);
        assert_eq!(
            after.iter().map(|e| e.kind.name()).collect::<Vec<_>>(),
            ["due.changed", "tag.removed"]
        );
        assert_eq!(
            after[0].kind,
            AttributeEventKind::DueChanged {
                change: AttributeChange::Cleared
            }
        );
    }

    /// The same on the workspace scale: retiring a missing workspace clears
    /// what its documents were known to carry.
    #[test]
    fn a_retired_workspace_clears_its_attrs() {
        let store = FakeStore::default();
        let ws = WorkspaceId::new();
        let doc = store.add(ws, "task");
        let tag = TagId::new();
        store.edit(doc, |ws| {
            let node = body_node(ws);
            ws.add_tag(node, tag).unwrap();
        });
        let mut p = projector_for(&store, ws);
        let recorder = Recording::default();
        p.add_consumer(Box::new(recorder.clone()));
        settle(&mut p, 0);
        let told = recorder.events().len();

        store.drop_workspace(ws);
        p.sweeps().request(ws);
        settle(&mut p, 10_000);

        let after: Vec<_> = recorder.events().split_off(told);
        assert_eq!(
            after,
            vec![AttributeEvent {
                workspace: ws,
                document: doc,
                node: after.first().map(|e| e.node.clone()).unwrap_or_default(),
                kind: AttributeEventKind::TagRemoved {
                    tag: tag.to_storage()
                },
            }]
        );
    }
}

// --------------------------------------------------------------- modified-at

/// **A rebuild does not restamp.** This is the whole of D-f.
///
/// Dropping `projection.db` — a schema bump, a deleted file, a corrupt one —
/// leaves the next projection with no earlier content digest to compare
/// against, so the digest gate alone takes the `NULL` branch for *every*
/// document at once and stamps them all at the moment of the rebuild. A recency
/// list would come back from a rebuild in arbitrary order and stay that way
/// until real edits differentiated it again.
///
/// The value therefore comes from the store's own durable row, which is in
/// `workspace.db` and was never dropped. The two stamps here are far apart and
/// far from any clock the projector could have used, so a restamp cannot pass
/// this by coincidence.
#[test]
fn a_rebuilt_projection_keeps_each_documents_modified_at() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    let older = store.add(ws, "written last year");
    let newer = store.add(ws, "written this morning");
    store.stamp(older, 1_000);
    store.stamp(newer, 5_000);

    let mut first = projector_for(&store, ws);
    settle(&mut first, 0);
    assert_eq!(first.projections().modified_at(older).unwrap(), Some(1_000));
    assert_eq!(first.projections().modified_at(newer).unwrap(), Some(5_000));
    drop(first);

    // The rebuild. `Projections::in_memory` is a projection database with
    // nothing in it at all, which is what the schema-mismatch path leaves
    // behind after it has dropped every projection-owned table.
    let mut rebuilt = projector_for(&store, ws);
    settle(&mut rebuilt, 900_000);

    assert_eq!(
        rebuilt.projections().modified_at(older).unwrap(),
        Some(1_000),
        "the rebuild restamped a document nobody had edited"
    );
    assert_eq!(
        rebuilt.projections().modified_at(newer).unwrap(),
        Some(5_000),
        "the rebuild restamped a document nobody had edited"
    );
    assert!(
        rebuilt.projections().modified_at(newer).unwrap()
            > rebuilt.projections().modified_at(older).unwrap(),
        "a rebuild collapsed the recency order"
    );
}

/// **The store supplies the value; the digest still decides whether it moves.**
///
/// `updated_at` is moved by a checkpoint as well as by a commit, and an
/// over-long journal is checkpointed at *load* — so a store restart can move it
/// for a document nobody edited. Taking it unconditionally would let
/// maintenance reorder a recency list. The gate is what stops it, and it is
/// still the gate when the value is the store's.
#[test]
fn a_durable_stamp_that_moved_without_the_content_does_not_move_modified_at() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    let doc = store.add(ws, "settled");
    store.stamp(doc, 1_000);
    let mut p = projector_for(&store, ws);
    settle(&mut p, 0);
    assert_eq!(p.projections().modified_at(doc).unwrap(), Some(1_000));

    // A commit that changes no node — the shape a checkpoint takes by the time
    // it reaches this layer — with the durable stamp moved.
    store.stamp(doc, 9_000);
    store.edit(doc, |_| {});
    settle(&mut p, 10_000);
    assert_eq!(
        p.projections().modified_at(doc).unwrap(),
        Some(1_000),
        "a stamp moved by maintenance reordered a list nobody had edited"
    );

    // A real edit takes the store's new stamp, and not the projector's clock.
    store.stamp(doc, 12_000);
    store.write(doc, "and then some more");
    settle(&mut p, 500_000);
    assert_eq!(p.projections().modified_at(doc).unwrap(), Some(12_000));
}

/// A source that cannot answer costs the sort its accuracy and nothing else.
/// Every other test in this file is this case, so it is worth one assertion
/// that says so on purpose.
#[test]
fn a_source_with_no_durable_stamp_falls_back_to_the_projectors_clock() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    let doc = store.add(ws, "no stamp here");
    let mut p = projector_for(&store, ws);
    settle(&mut p, 0);

    store.write(doc, "edited");
    settle(&mut p, 7_000);
    let stamp = p
        .projections()
        .modified_at(doc)
        .unwrap()
        .expect("a projected document");
    assert!(
        stamp >= 7_000,
        "the fallback stopped being the projector's own clock: {stamp}"
    );
}

// ------------------------------------------------------------------ failures

/// One document that will not project must not take the others down with it,
/// and must be visible rather than silent.
#[test]
fn a_document_that_will_not_fork_is_counted_and_skipped() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    let bad = store.add(ws, "bad");
    let good = store.add(ws, "good");
    store.broken.borrow_mut().push(bad);

    let mut p = projector_for(&store, ws);
    settle(&mut p, 0);

    assert_eq!(p.stats().failed, 1);
    assert!(p.stats().last_error.is_some());
    assert!(
        p.projections().read(good).unwrap().is_some(),
        "one broken document cost the others their projection"
    );
}

// --------------------------------------- a failure that will never come right
//
// Retrying is for transient errors. A malformed document is not one: the walk
// reads state rather than replaying events, so every attempt reaches the same
// error, and a bounded backoff alone turns that into a fork and a walk every
// `QUIET_MILLIS * 16` for as long as the process lives.

/// It stops, says so, and stops costing anything.
#[test]
fn a_document_that_can_never_project_is_left_alone_and_counted() {
    let store = FakeStore::default();
    let workspace = WorkspaceId::new();
    let document = store.add(workspace, "before");
    let mut projector = projector_for(&store, workspace);
    settle(&mut projector, 0);
    let failed_before = projector.stats().failed;

    store.broken.borrow_mut().push(document);
    store.write(document, "newer");
    settle(&mut projector, 10_000);

    assert_eq!(
        projector.stats().pending,
        0,
        "it is still queued, so it is still being retried"
    );
    assert_eq!(projector.stats().abandoned, 1);
    assert_eq!(
        projector.stats().failed - failed_before,
        u64::from(MAX_RETRY_ATTEMPTS),
        "it gave up early or late"
    );
}

/// **Giving up is not forgetting.** A commit is what says something changed, so
/// it is what earns a fresh set of attempts — otherwise the repair that fixes a
/// document could never be noticed.
#[test]
fn a_commit_after_giving_up_starts_the_attempts_again() {
    let store = FakeStore::default();
    let workspace = WorkspaceId::new();
    let document = store.add(workspace, "before");
    let mut projector = projector_for(&store, workspace);
    settle(&mut projector, 0);

    store.broken.borrow_mut().push(document);
    store.write(document, "newer");
    settle(&mut projector, 10_000);
    assert_eq!(projector.stats().abandoned, 1);

    // The document is repaired, and says so the only way a store can.
    store.broken.borrow_mut().clear();
    store.write(document, "repaired");
    settle(&mut projector, 100_000);

    assert_eq!(projector.stats().abandoned, 1, "it gave up a second time");
    assert_eq!(
        projector
            .projections()
            .read(document)
            .unwrap()
            .expect("the repaired document was projected")
            .nodes
            .iter()
            .filter(|node| node.text == "repaired")
            .count(),
        1
    );
}
