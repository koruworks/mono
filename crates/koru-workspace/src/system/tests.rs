// SPDX-License-Identifier: MPL-2.0

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;
use crate::store::link::fake::FakeLink;

/// A system store in memory. `pub(crate)` so the page registry's tests can
/// give a workspace a system document to record generations in.
pub(crate) struct FakeStore {
    workspace: koru_store_core::ids::WorkspaceId,
    snapshots: Mutex<HashMap<DocumentId, Vec<u8>>>,
    links: Mutex<HashMap<DocumentId, Arc<FakeLink>>>,
    join_threads: Mutex<Vec<std::thread::ThreadId>>,
    created: DocumentId,
}

impl FakeStore {
    pub(crate) fn new(docs: impl IntoIterator<Item = (DocumentId, SystemDoc)>) -> Self {
        Self {
            workspace: koru_store_core::ids::WorkspaceId::new(),
            snapshots: Mutex::new(
                docs.into_iter()
                    .map(|(id, doc)| (id, doc.export_snapshot().expect("snapshot")))
                    .collect(),
            ),
            links: Mutex::new(HashMap::new()),
            join_threads: Mutex::new(Vec::new()),
            created: DocumentId::new(),
        }
    }

    fn put(&self, id: DocumentId, doc: &SystemDoc) {
        self.snapshots
            .lock()
            .expect("snapshots")
            .insert(id, doc.export_snapshot().expect("snapshot"));
    }

    fn link(&self, id: DocumentId) -> Arc<FakeLink> {
        self.links.lock().expect("links")[&id].clone()
    }

    fn commit_sent(&self, id: DocumentId) {
        let mut doc = SystemDoc::from_snapshot(&self.snapshots.lock().expect("snapshots")[&id])
            .expect("stored system");
        doc.import_frames(&self.link(id).sent())
            .expect("apply published system updates");
        self.put(id, &doc);
    }
}

impl SystemStore for FakeStore {
    fn list(&self) -> Result<Vec<DocumentId>, String> {
        Ok(self
            .snapshots
            .lock()
            .expect("snapshots")
            .keys()
            .copied()
            .collect())
    }

    fn create(&self) -> Result<DocumentId, String> {
        self.snapshots.lock().expect("snapshots").insert(
            self.created,
            SystemDoc::create()
                .expect("system")
                .export_snapshot()
                .expect("snapshot"),
        );
        Ok(self.created)
    }

    fn join(&self, id: DocumentId) -> Result<(store::StoreSession, Vec<u8>), String> {
        self.join_threads
            .lock()
            .expect("threads")
            .push(std::thread::current().id());
        let snapshot = self
            .snapshots
            .lock()
            .expect("snapshots")
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("missing {id}"))?;
        let (session, link) =
            store::StoreSession::for_system_test(self.workspace, explore::store_document(id));
        self.links.lock().expect("links").insert(id, link);
        Ok((session, snapshot))
    }
}

fn doc_with(name: &str) -> SystemDoc {
    let mut doc = SystemDoc::create().expect("system");
    doc.create_tag(name, None, 1).expect("tag");
    doc
}

fn names(session: &SystemSession) -> Vec<String> {
    session.tags().into_iter().map(|tag| tag.name).collect()
}

fn finish(session: &mut SystemSession) {
    let until = Instant::now() + Duration::from_secs(2);
    while !session.poll_ready() {
        assert!(
            Instant::now() < until,
            "background system open did not finish"
        );
        std::thread::yield_now();
    }
}

#[test]
fn the_lowest_listed_id_is_canonical_and_losers_stay_live() {
    let mut ids: Vec<_> = (0..2).map(|_| DocumentId::new()).collect();
    ids.sort_unstable();
    let (canonical, loser) = (ids[0], ids[1]);
    let store = Arc::new(FakeStore::new([
        (canonical, doc_with("canonical")),
        (loser, doc_with("loser")),
    ]));
    let mut session = SystemSession::with_store(store.clone());

    session.reconcile(vec![loser, canonical], false);
    finish(&mut session);

    assert_eq!(session.active_id(), Some(canonical));
    assert_eq!(names(&session), vec!["canonical", "loser"]);
    assert_eq!(session.losers.len(), 1, "the loser room was discarded");
    assert_eq!(session.losers[0].id, loser);

    // L is published before it is absorbed, and C is published again with the
    // union. An acknowledgement is deliberately absent from this test.
    assert!(!store.link(loser).sent().is_empty());
    let c_sent = store.link(canonical).sent();
    assert!(c_sent.len() >= 2, "C was not published after absorbing L");
    let mut replica = SystemDoc::create().expect("replica");
    replica.import_frames(&c_sent).expect("published C");
    assert_eq!(
        replica
            .tags()
            .into_iter()
            .map(|tag| tag.name)
            .collect::<Vec<_>>(),
        vec!["canonical", "loser"]
    );
}

#[test]
fn a_running_loser_adopts_in_background_with_its_final_unacked_tag() {
    let mut ids: Vec<_> = (0..2).map(|_| DocumentId::new()).collect();
    ids.sort_unstable();
    let (canonical, loser) = (ids[0], ids[1]);
    let store = Arc::new(FakeStore::new([(loser, doc_with("before"))]));
    let caller = std::thread::current().id();
    let mut session = SystemSession::with_store(store.clone());

    session.reconcile(vec![loser], false);
    finish(&mut session);
    session.create_tag("during join").expect("local tag");

    store.put(canonical, &doc_with("from importer"));
    session.reconcile(vec![canonical, loser], false);
    finish(&mut session);

    assert_eq!(session.active_id(), Some(canonical));
    assert_eq!(
        names(&session),
        vec!["before", "during join", "from importer"]
    );
    assert!(session.losers.iter().any(|live| live.id == loser));
    assert!(
        store
            .join_threads
            .lock()
            .expect("threads")
            .iter()
            .all(|thread| *thread != caller),
        "a join ran on the answering thread"
    );

    let sent = store.link(canonical).sent();
    let mut replica = SystemDoc::create().expect("replica");
    replica.import_frames(&sent).expect("published canonical");
    assert_eq!(
        replica
            .tags()
            .into_iter()
            .map(|tag| tag.name)
            .collect::<Vec<_>>(),
        vec!["before", "during join", "from importer"]
    );
}

#[test]
fn two_running_instances_converge_without_waiting_for_an_ack() {
    let mut ids: Vec<_> = (0..2).map(|_| DocumentId::new()).collect();
    ids.sort_unstable();
    let (canonical, loser) = (ids[0], ids[1]);
    let canonical_seed = doc_with("from B").export_snapshot().expect("C snapshot");
    let store_a = Arc::new(FakeStore::new([(loser, doc_with("from A"))]));
    let store_b = Arc::new(FakeStore::new([
        (
            canonical,
            SystemDoc::from_snapshot(&canonical_seed).expect("C for B"),
        ),
        (loser, doc_with("from A")),
    ]));
    let mut a = SystemSession::with_store(store_a.clone());
    let mut b = SystemSession::with_store(store_b.clone());

    a.reconcile(vec![loser], false);
    finish(&mut a);
    a.create_tag("unacked in A").expect("tag");
    store_a.put(
        canonical,
        &SystemDoc::from_snapshot(&canonical_seed).expect("C for A"),
    );

    b.reconcile(vec![canonical, loser], false);
    finish(&mut b);
    a.reconcile(vec![canonical, loser], false);
    finish(&mut a);

    // The room carries A's canonical publication to the other running
    // instance. No acknowledgement is injected anywhere in this scenario.
    let from_a = store_a
        .link(canonical)
        .sent()
        .last()
        .cloned()
        .expect("A published C");
    store_b.link(canonical).delivers(vec![from_a]);
    assert!(b.tick(), "B did not receive A's canonical union");

    assert_eq!(a.active_id(), Some(canonical));
    assert_eq!(b.active_id(), Some(canonical));
    assert!(names(&a).contains(&"unacked in A".to_owned()));
    assert!(names(&b).contains(&"unacked in A".to_owned()));
    assert!(a.losers.iter().any(|live| live.id == loser));
    assert!(b.losers.iter().any(|live| live.id == loser));
}

#[test]
fn a_late_loser_update_is_forwarded_to_canonical() {
    let mut ids: Vec<_> = (0..2).map(|_| DocumentId::new()).collect();
    ids.sort_unstable();
    let (canonical, loser) = (ids[0], ids[1]);
    let loser_seed = doc_with("old loser");
    let loser_snapshot = loser_seed.export_snapshot().expect("L snapshot");
    let store = Arc::new(FakeStore::new([
        (canonical, doc_with("canonical")),
        (
            loser,
            SystemDoc::from_snapshot(&loser_snapshot).expect("L for store"),
        ),
    ]));
    let mut session = SystemSession::with_store(store.clone());
    session.reconcile(vec![canonical, loser], false);
    finish(&mut session);

    let mut old_replica = SystemDoc::from_snapshot(&loser_snapshot).expect("old replica");
    old_replica
        .create_tag("arrived late", None, 2)
        .expect("late tag");
    store
        .link(loser)
        .delivers(vec![old_replica.export_all_updates().expect("late update")]);

    assert!(session.tick(), "the loser update was not serviced");
    assert_eq!(
        names(&session),
        vec!["arrived late", "canonical", "old loser"]
    );
    assert!(
        store.link(canonical).sent().len() >= 3,
        "the late loser union was not published on C"
    );
}

#[test]
fn tag_creation_remains_immediate_while_the_first_open_is_pending() {
    let id = DocumentId::new();
    let store = Arc::new(FakeStore::new([(id, doc_with("existing"))]));
    let mut session = SystemSession::with_store(store);

    session.reconcile(vec![id], false);
    let tag = session.create_tag("typed now");
    assert!(tag.is_some(), "/tag waited for the background join");
    finish(&mut session);
    assert_eq!(names(&session), vec!["existing", "typed now"]);
}

#[test]
fn the_first_tag_creates_its_system_row_in_the_background() {
    let store = Arc::new(FakeStore::new([]));
    let created = store.created;
    let mut session = SystemSession::with_store(store.clone());

    session.reconcile(Vec::new(), true);
    assert!(session.create_tag("first tag").is_some());
    finish(&mut session);

    assert_eq!(session.active_id(), Some(created));
    assert_eq!(names(&session), vec!["first tag"]);
    assert!(!store.link(created).sent().is_empty());
}

#[test]
fn a_workspace_without_a_store_still_declines_tag_creation() {
    let ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("workspace");
    let mut workspace = Workspace::new(ws);
    assert!(workspace.tags_snapshot().is_empty());
    assert_eq!(workspace.create_tag("mvp"), None);
    assert!(!workspace.take_tags_changed());
}

/// The list half of the same contract. A rail built on a workspace with no
/// store draws no lists and creates none — which is what every unit test
/// of the tasks rail relies on.
#[test]
fn without_a_store_lists_are_empty_and_create_declines() {
    let ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("workspace");
    let mut workspace = Workspace::new(ws);
    assert!(workspace.lists_snapshot().is_empty());
    assert_eq!(workspace.create_list("Wedding", Some("pink")), None);
    assert!(!workspace.rename_list(koru_core::ids::ListId::new(), "Bike"));
    assert!(!workspace.set_list_color(koru_core::ids::ListId::new(), Some("teal")));
    assert!(!workspace.delete_list(koru_core::ids::ListId::new()));
    assert!(!workspace.take_lists_changed());
}

#[test]
fn import_worker_registers_once_and_updates_an_existing_system_in_place() {
    let fresh_store = Arc::new(FakeStore::new([]));
    let mut fresh = ImportSystemSession::with_store(fresh_store.clone()).expect("import session");
    assert!(fresh.snapshot().unwrap().is_none());
    let staged = doc_with("imported").export_snapshot().unwrap();
    fresh.publish_staged(&staged, true).expect("publish import");
    assert_eq!(fresh_store.snapshots.lock().unwrap().len(), 1);
    assert_eq!(
        fresh
            .snapshot()
            .unwrap()
            .as_deref()
            .map(SystemDoc::from_snapshot)
            .transpose()
            .unwrap()
            .unwrap()
            .tags()[0]
            .name,
        "imported"
    );

    let existing_id = DocumentId::new();
    let existing_store = Arc::new(FakeStore::new([(existing_id, doc_with("before"))]));
    let mut existing =
        ImportSystemSession::with_store(existing_store.clone()).expect("existing import session");
    existing
        .publish_staged(&doc_with("after").export_snapshot().unwrap(), false)
        .expect("update existing");
    assert_eq!(existing_store.snapshots.lock().unwrap().len(), 1);
    let names: Vec<_> = existing
        .snapshot()
        .unwrap()
        .as_deref()
        .map(SystemDoc::from_snapshot)
        .transpose()
        .unwrap()
        .unwrap()
        .tags()
        .into_iter()
        .map(|tag| tag.name)
        .collect();
    assert_eq!(names, ["after", "before"]);
}

#[test]
fn a_real_import_publish_still_triggers_running_loser_adoption() {
    let mut ids: Vec<_> = (0..2).map(|_| DocumentId::new()).collect();
    ids.sort_unstable();
    let (canonical, loser) = (ids[0], ids[1]);
    let store = Arc::new(FakeStore::new([
        (canonical, doc_with("canonical")),
        (loser, doc_with("loser")),
    ]));

    // Window A is still on L and has an unacknowledged local tag.
    let mut window = SystemSession::with_store(store.clone());
    window.reconcile(vec![loser], false);
    finish(&mut window);
    window.create_tag("unacked in A").expect("local tag");

    // The import worker opens the real listed system rows through the injected
    // port, stages another tag from its current canonical snapshot, and
    // publishes C without waiting for an acknowledgement.
    let mut importer = ImportSystemSession::with_store(store.clone()).expect("import session");
    let mut staged = SystemDoc::from_snapshot(
        importer
            .snapshot()
            .unwrap()
            .as_deref()
            .expect("canonical snapshot"),
    )
    .unwrap();
    staged.create_tag("from tagged import", None, 2).unwrap();
    importer
        .publish_staged(&staged.export_snapshot().unwrap(), false)
        .expect("publish staged import");
    store.commit_sent(canonical);

    window.reconcile(vec![canonical, loser], false);
    finish(&mut window);

    assert_eq!(window.active_id(), Some(canonical));
    assert_eq!(
        names(&window),
        ["canonical", "from tagged import", "loser", "unacked in A"]
    );
    assert!(window.losers.iter().any(|live| live.id == loser));
}

/// **The peer's first tag resolves without a restart.**
///
/// A workspace opened before anything in it was tagged holds no system
/// session: `ensure_system(false)` declines to build one while the manifest
/// lists no `"koru-system"` row. When a peer then tags a block, three things
/// arrive — the node's `koru.tag` attribute, the manifest row, and the system
/// document itself — and this replica has to *open* that document before any
/// chip can put a name to the id.
///
/// The recheck the manifest drain performs used to reconcile an already-open
/// session and do nothing when there was none, which is precisely this case.
/// The chip drew its unresolved placeholder — an ellipsis — beside a task chip
/// that had replicated perfectly, because a due date is a value on the node
/// and a tag is a reference into a document nothing had opened.
#[test]
fn a_peers_first_system_document_is_opened_when_its_manifest_row_arrives() {
    let system = DocumentId::new();
    let store = Arc::new(FakeStore::new([(system, doc_with("tag"))]));

    let ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("workspace");
    let mut workspace = Workspace::new(ws);
    workspace.system_store_for_tests = Some(store.clone() as Arc<dyn SystemStore>);

    // Before the peer's tag: a manifest with no system row, and no session.
    workspace.manifest = Some(
        koru_store_core::manifest::Manifest::create(
            explore::store_workspace(koru_core::WorkspaceId::new()),
            "t",
            koru_store_core::model::Timestamp::from_millis(1),
        )
        .expect("manifest"),
    );
    assert!(workspace.tags_snapshot().is_empty());
    assert!(
        workspace.system.is_none(),
        "a workspace with no tagged content opens no system document"
    );

    // The peer's manifest update lands: the row for the system document it
    // just created.
    workspace
        .manifest
        .as_mut()
        .expect("manifest")
        .add_document(
            explore::store_document(system),
            "",
            koru_core::explore::KIND_SYSTEM,
            None,
        )
        .expect("register the peer's system document");
    workspace.recheck_system_on_entries_change();

    let session = workspace
        .system
        .as_mut()
        .expect("the manifest row did not open the peer's system document");
    finish(session);
    assert_eq!(
        names(session),
        vec!["tag"],
        "the peer's tag name never reached this replica"
    );
}

/// **The turn that adopts a finished open is the turn that must say so.**
///
/// The host's turn calls `reconcile` before `tick`, so a background open that
/// landed between two turns is adopted by `reconcile` — and `tick` is the only
/// thing that sets `tags_changed`, which is the only thing that makes the
/// shell re-read the table and push it into the views. Dropping `reconcile`'s
/// answer left the freshly opened tag table sitting in memory behind chips
/// that went on drawing the placeholder.
#[test]
fn an_open_adopted_by_reconcile_is_still_reported_by_tick() {
    let id = DocumentId::new();
    let store = Arc::new(FakeStore::new([(id, doc_with("from a peer"))]));
    let mut session = SystemSession::with_store(store);

    session.reconcile(vec![id], false);
    // Let `reconcile` — never `poll_ready` directly — be what adopts it, which
    // is what the host's turn order makes happen.
    let until = Instant::now() + Duration::from_secs(2);
    while session.active_id().is_none() {
        assert!(
            Instant::now() < until,
            "background system open did not finish"
        );
        session.reconcile(vec![id], false);
        std::thread::yield_now();
    }

    assert!(
        session.tick(),
        "the adopted open was never reported, so nothing re-reads the tag table"
    );
    assert_eq!(names(&session), vec!["from a peer"]);
    assert!(
        !session.tick(),
        "and it is reported once, not on every turn after"
    );
}
