// SPDX-License-Identifier: MPL-2.0

//! **A delegated run is neither walked nor captured** (P1.0 proposal F4,
//! ADR-343), proved through the real store: two hundred steps committed to a
//! `koru-run` document leave `document_versions` empty for it and never
//! reach the projector's fork, while an ordinary page beside it is projected
//! and captured as before.
//!
//! The projector is driven by hand with a fake clock, the way
//! `koru-projection`'s own tests drive it, over the daemon's real
//! [`StoreSource`] and a `Store<SqliteCatalog, SqliteBackend>` under a
//! temporary root.

use std::collections::BTreeSet;

use super::*;
use koru_delegate_core::{
    ActorRef, DelegationRequest, DeviceId, Objective, RunHeader, SessionProfileKey, StepKind,
    TimelineEntry, TimelineRefs, WorkspaceScope,
};
use koru_delegate_document::{RUN_KIND, RunDocument};
use koru_projection::worker::QUIET_MILLIS;
use koru_store_api::state::KoruStore;
use koru_store_core::model::Timestamp;
use koru_store_core::service::Store;
use koru_store_core::text::Locale;
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};

fn at(ms: i64) -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000 + ms)
}

fn a_header() -> RunHeader {
    RunHeader::from_request(
        DelegationRequest {
            run: koru_delegate_core::RunId::mint(),
            requested_by: ActorRef::Person {
                profile: koru_store_core::ProfileId::new(),
            },
            objective: Objective::new("tidy the notes").unwrap(),
            agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
            requested_model: None,
            scope: WorkspaceScope::Temporary,
            context: Vec::new(),
            grants: BTreeSet::new(),
            executing_device: DeviceId::new(),
            origin: Default::default(),
        },
        0,
    )
}

/// A page with one paragraph, as the store receives it from a client.
fn a_page(store: &mut KoruStore, ws: WorkspaceId, at_ms: i64) -> DocumentId {
    let mut page =
        koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "W").unwrap();
    let root = page
        .insert(
            koru_core::schema::Parent::Root,
            koru_core::node::Node::document("Notes").unwrap(),
        )
        .unwrap();
    page.insert(
        koru_core::schema::Parent::Node(root),
        koru_core::node::Node::paragraph("Delegated").unwrap(),
    )
    .unwrap();
    let (id, _) = store
        .import_document(
            ws,
            "Notes",
            "page",
            None,
            page.export_snapshot().unwrap(),
            at(at_ms),
        )
        .unwrap();
    id
}

/// One more paragraph on a page, applied and flushed as an editor would.
fn edit(store: &mut KoruStore, ws: WorkspaceId, page: DocumentId, at_ms: i64) {
    let mut doc =
        koru_core::schema::WorkspaceDoc::adopt(store.fork_document(ws, page).unwrap()).unwrap();
    let before = doc.version();
    let root = doc.children(koru_core::schema::Parent::Root).unwrap()[0];
    doc.insert(
        koru_core::schema::Parent::Node(root),
        koru_core::node::Node::paragraph("Edited").unwrap(),
    )
    .unwrap();
    let delta = doc.export_updates_since(&before).unwrap();
    store
        .apply_update(ws, page, delta, at(at_ms), None)
        .unwrap();
    store.flush(ws, page, at(at_ms)).unwrap();
}

/// The executing device's replica of a run: it writes locally and sends
/// the store the update since its last send, one flush per fact.
struct Executing {
    ws: WorkspaceId,
    id: DocumentId,
    run: RunDocument,
    sent: loro::VersionVector,
}

impl Executing {
    /// `create_document(ws, title, RUN_KIND, ..)`: the manifest entry. The
    /// header follows with the first `send`.
    fn register(store: &mut KoruStore, ws: WorkspaceId) -> Self {
        let (id, _) = store
            .create_document(ws, "Run", RUN_KIND, None, None, at(1))
            .unwrap();
        Self {
            ws,
            id,
            run: RunDocument::create(&a_header()).unwrap(),
            sent: loro::VersionVector::default(),
        }
    }

    fn send(&mut self, store: &mut KoruStore, at_ms: i64) {
        let delta = self.run.export_updates_since(&self.sent).unwrap();
        self.sent = self.run.version();
        store
            .apply_update(self.ws, self.id, delta, at(at_ms), None)
            .unwrap();
        store.flush(self.ws, self.id, at(at_ms)).unwrap();
    }

    fn step(&mut self, store: &mut KoruStore, i: usize) {
        self.run
            .append_step(&TimelineEntry::new(
                StepKind::ToolsCompleted {
                    calls: 1,
                    failed: 0,
                    failures: Default::default(),
                },
                TimelineRefs::default(),
                i as i64,
            ))
            .unwrap();
        self.send(store, 3 + i as i64);
    }
}

#[test]
fn a_run_is_neither_forked_nor_captured_while_a_page_beside_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let catalog = SqliteCatalog::open(&root.join("catalog.db")).unwrap();
    let mut store: KoruStore = Store::open(catalog, SqliteBackend::new(root)).unwrap();
    let profile = store
        .create_profile("Owner", None, None, Locale::parse("en").unwrap(), at(0))
        .unwrap();
    let ws = store
        .create_workspace(profile.id, "Workspace", at(0))
        .unwrap()
        .id;
    let page = a_page(&mut store, ws, 1);
    let mut executing = Executing::register(&mut store, ws);
    let run = executing.id;

    let state = Arc::new(AppState::new(
        store,
        root.display().to_string(),
        "test".into(),
        0,
    ));
    let source = StoreSource::new(state.clone(), true);
    let mut projector = Projector::new(source, Projections::open(root).unwrap());

    // The projector ticks every quarter second against steps every second
    // and a half, so each commit is drained on its own — and the manifest
    // commit that registered the run is drained before the run's first
    // commit, which is how the mirror knows the kind in time.
    // (`Store::take_committed` coalesces per document, so a projector that
    // slept through a whole run would see one commit, not two hundred.)
    let mut now = 0;
    projector.tick(now);
    // The page is edited once the projector is up, so its commit — and not
    // only the sweep that finds an imported document — is what enrols it.
    edit(&mut state.store.lock().unwrap(), ws, page, 1);
    executing.send(&mut state.store.lock().unwrap(), 2);
    projector.tick(now);
    for i in 0..200 {
        executing.step(&mut state.store.lock().unwrap(), i);
        now += 1_500;
        projector.tick(now);
    }
    // Settle the page's projection past the debounce, then move past the
    // idle history window so its capture falls due.
    for _ in 0..100 {
        if !projector.tick(now) && projector.stats().pending == 0 {
            break;
        }
        now += QUIET_MILLIS + 1;
    }
    now += IDLE_HISTORY_MS + 1;
    for _ in 0..10 {
        projector.tick(now);
    }

    let stats = projector.stats();
    // The header and every one of the two hundred steps.
    assert_eq!(
        stats.skipped, 201,
        "every commit to the run was skipped: {stats:?}"
    );
    assert!(stats.projected >= 1, "the page was projected: {stats:?}");
    assert_eq!(stats.failed, 0);
    assert!(
        projector.projections().revision(run).unwrap().is_none(),
        "the run has no projection row"
    );
    assert!(
        projector.projections().revision(page).unwrap().is_some(),
        "the page has one"
    );

    let mut store = state.store.lock().unwrap();
    let captures = |store: &mut KoruStore, document: DocumentId| {
        store
            .history_entries(ws, document, None, 100)
            .unwrap()
            .into_iter()
            .filter(|entry| {
                matches!(
                    entry.reason,
                    HistoryReason::AutomaticIdle | HistoryReason::AutomaticActive
                )
            })
            .count()
    };
    assert_eq!(
        captures(&mut store, run),
        0,
        "document_versions is empty for the run"
    );
    assert_eq!(captures(&mut store, page), 1, "the page was captured once");
}
