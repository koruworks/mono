// SPDX-License-Identifier: MPL-2.0

//! When a document is indexed, and how the index catches up when it was not
//! told.
//!
//! Against a real `Projections` in memory and a fake clock. No store and no
//! daemon: what is being tested is the consumer's own rules.

use super::*;
use koru_projection::model::{DocumentProjection, ListedDocument, ProjectedNode};
use koru_store_core::ids::{DocumentId, WorkspaceId};

fn a_node(text: &str) -> ProjectedNode {
    ProjectedNode {
        id: "n1".to_owned(),
        parent: None,
        ordinal: 0,
        depth: 0,
        kind: "paragraph".to_owned(),
        text: text.to_owned(),
        heading: None,
        list_style: None,
        done: false,
        level_name: None,
        name: None,
        icon: None,
        colour: None,
        language: None,
        blob: None,
        mime: None,
        embed: None,
        // See `fragments/tests.rs`: the indexer reads the live document, so a
        // binned node never reaches it.
        in_trash: false,
        runs: Vec::new(),
        attrs: Vec::new(),
    }
}

fn listed(document: DocumentId, title: &str) -> ListedDocument {
    ListedDocument {
        document,
        parent: None,
        ordinal: 0,
        title: title.to_owned(),
        kind: "page".to_owned(),
        mime: None,
        trashed_at: None,
        trashed_by: None,
    }
}

/// A projection holding one workspace with one document.
struct Fixture {
    workspace: WorkspaceId,
    document: DocumentId,
    projections: Projections,
}

impl Fixture {
    fn new(text: &str) -> Self {
        let workspace = WorkspaceId::new();
        let document = DocumentId::new();
        let mut projections = Projections::in_memory().unwrap();
        projections
            .set_listing(workspace, "Acme", &[listed(document, "Roadmap")], 1)
            .unwrap();
        projections
            .replace(
                &DocumentProjection {
                    workspace,
                    document,
                    source_revision: b"r1".to_vec(),
                    nodes: vec![a_node(text)],
                },
                1,
                None,
            )
            .unwrap();
        Self {
            workspace,
            document,
            projections,
        }
    }

    fn indexer(self) -> Indexer {
        Indexer::new(
            SearchIndex::in_memory().unwrap(),
            self.projections,
            Queue::default(),
        )
    }
}

/// Run until there is nothing left to do, or fail rather than spin.
fn settle(indexer: &mut Indexer, now: i64) {
    for _ in 0..1000 {
        if !indexer.tick(now) && indexer.queue.is_empty() {
            return;
        }
    }
    panic!("the indexer never settled");
}

fn matches(indexer: &Indexer, text: &str) -> usize {
    indexer
        .index()
        .query(
            &crate::model::SearchQuery {
                text: text.into(),
                ..Default::default()
            },
            &[],
        )
        .unwrap()
        .hits
        .len()
}

/// **Nobody told it anything.** A fresh index over a projection that already
/// holds documents finds them by comparison — which is also what a deleted
/// `search.db` and a schema bump both look like.
#[test]
fn a_fresh_index_catches_up_by_comparison() {
    let fixture = Fixture::new("the quarterly plan");
    let mut indexer = fixture.indexer();

    settle(&mut indexer, 0);

    assert_eq!(matches(&indexer, "quarterly"), 1);
    assert_eq!(indexer.queue().stats().indexed, 1);
    assert!(indexer.queue().stats().caught_up > 0);
}

/// **A hidden document is hidden from Explore and from nothing else.** The app
/// keeps two kinds out of the listing a person sees — `koru-system` and, since
/// ADR-272's 2026-09-01 amendment, the `task-list` document a task captured in
/// the Tasks view lands in (`koru_core::explore::listed`). That predicate lives
/// four crates above this one and the indexer has never asked it: a kind is
/// interesting here only as *is this the tag table* (`KIND_SYSTEM`). So a native
/// task's text is found by search like any other line — which is the whole
/// reason hiding was safe to do at the listing.
#[test]
fn a_task_in_a_hidden_task_list_document_is_still_searchable() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let mut projections = Projections::in_memory().unwrap();
    projections
        .set_listing(
            workspace,
            "Acme",
            &[ListedDocument {
                // The literal rather than a constant: this crate deliberately
                // links no node model, and mirroring a second token here would
                // imply the indexer has an opinion about it. It does not.
                kind: "task-list".to_owned(),
                ..listed(document, "Tasks")
            }],
            1,
        )
        .unwrap();
    projections
        .replace(
            &DocumentProjection {
                workspace,
                document,
                source_revision: b"r1".to_vec(),
                nodes: vec![a_node("buy oat milk")],
            },
            1,
            None,
        )
        .unwrap();

    let mut indexer = Indexer::new(
        SearchIndex::in_memory().unwrap(),
        projections,
        Queue::default(),
    );
    settle(&mut indexer, 0);

    assert_eq!(matches(&indexer, "oat"), 1, "the task's own words");
    assert_eq!(matches(&indexer, "Tasks"), 1, "and the document it is in");
}

/// **A reading document yields no search result** (OCR TECHNICAL D14, §7:
/// "its title is shown nowhere … the plan picks the value and proves it
/// produces no row in any listing and no search result"). The projector never
/// walks one, so it has no node fragments. But the listing is indexed on its
/// own — every listed document gets a title fragment, which is how a document
/// nobody has opened is still findable — so the title is what decides it.
/// `Store::ensure_reading` registers it with an empty title, and an empty
/// title makes no fragment. The second listing row shows the value is
/// load-bearing: an unprojected document *with* a title is found by it.
///
/// The kind is a literal for the reason
/// `a_task_in_a_hidden_task_list_document_is_still_searchable` gives.
#[test]
fn an_untitled_reading_document_is_never_a_result() {
    let fixture = Fixture::new("the quarterly plan");
    let reading = DocumentId::new();
    let titled = DocumentId::new();
    let mut projections = fixture.projections;
    projections
        .set_listing(
            fixture.workspace,
            "Acme",
            &[
                listed(fixture.document, "Roadmap"),
                ListedDocument {
                    parent: Some(fixture.document),
                    kind: "koru-reading".to_owned(),
                    ..listed(reading, "")
                },
                ListedDocument {
                    kind: "koru-reading".to_owned(),
                    ..listed(titled, "Scanned")
                },
            ],
            2,
        )
        .unwrap();
    let mut indexer = Indexer::new(
        SearchIndex::in_memory().unwrap(),
        projections,
        Queue::default(),
    );
    indexer.queue().push_listing(fixture.workspace);
    settle(&mut indexer, 0);
    settle(&mut indexer, 60_000);

    assert_eq!(matches(&indexer, "quarterly"), 1, "the page is indexed");
    assert_eq!(
        matches(&indexer, "Scanned"),
        1,
        "a title on an unprojected listing row is found by it"
    );
    let hits = indexer
        .index()
        .query(
            &crate::model::SearchQuery {
                text: "Acme".into(),
                ..Default::default()
            },
            &[],
        )
        .unwrap()
        .hits;
    assert!(
        hits.iter()
            .all(|hit| hit.document.as_deref() != Some(reading.to_storage().as_str())),
        "the untitled reading document is in no result"
    );
    assert_eq!(
        indexer
            .index()
            .connection
            .query_row(
                "SELECT COUNT(*) FROM fragments WHERE document_id = ?1",
                [reading.to_storage()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0,
        "and has no fragment for any query to find"
    );
}

/// The public `behind` counter is set on the reconcile that discovers missed
/// work, before the bounded repair consumes it. A fast restart may repair the
/// queue before an HTTP observer can sample that transient value, so pin the
/// value itself against the deterministic worker clock.
#[test]
fn a_reconcile_reports_the_work_it_found_before_repairing_it() {
    let fixture = Fixture::new("the quarterly plan");
    let mut indexer = fixture.indexer();

    assert!(indexer.tick(0));
    let stats = indexer.queue().stats();
    assert!(stats.behind >= 1, "{stats:?}");
    assert!(stats.pending >= 1, "{stats:?}");
    assert_eq!(stats.caught_up, 1);
}

/// The trigger path: a consumer told about a projection indexes exactly that
/// document.
#[test]
fn what_the_projector_announces_is_indexed() {
    let fixture = Fixture::new("the quarterly plan");
    let (workspace, document) = (fixture.workspace, fixture.document);
    let mut indexer = fixture.indexer();
    settle(&mut indexer, 0);

    let queue = indexer.queue();
    let mut feed = Feed::new(queue.clone());
    // As the projector would, after writing a new revision.
    let projection = DocumentProjection {
        workspace,
        document,
        source_revision: b"r2".to_vec(),
        nodes: vec![a_node("something else entirely")],
    };
    feed.projected(&projection);
    assert_eq!(queue.pending_workspaces(), vec![workspace]);

    // The indexer re-reads the projection rather than trusting what it was
    // handed, so the store has to have it.
    indexer.projections.replace(&projection, 2, None).unwrap();
    settle(&mut indexer, 100);

    assert_eq!(matches(&indexer, "quarterly"), 0);
    assert_eq!(matches(&indexer, "entirely"), 1);
}

/// Twenty announcements for one document are one index write. The projector
/// already coalesces; this is the same rule one layer down, and it is what
/// keeps a burst of edits from queueing a burst of work.
#[test]
fn repeated_announcements_for_one_document_queue_it_once() {
    let fixture = Fixture::new("start");
    let (workspace, document) = (fixture.workspace, fixture.document);
    let queue = Queue::default();
    let mut feed = Feed::new(queue.clone());
    let projection = DocumentProjection {
        workspace,
        document,
        source_revision: b"r2".to_vec(),
        nodes: vec![a_node("start")],
    };
    for _ in 0..20 {
        feed.projected(&projection);
    }
    assert_eq!(queue.stats().pending, 1);
}

/// A document the store dropped loses its rows. **Not a document in the
/// trash** — that one keeps them.
#[test]
fn a_dropped_document_loses_its_rows() {
    let fixture = Fixture::new("the quarterly plan");
    let (workspace, document) = (fixture.workspace, fixture.document);
    let mut indexer = fixture.indexer();
    settle(&mut indexer, 0);
    assert_eq!(matches(&indexer, "quarterly"), 1);

    let mut feed = Feed::new(indexer.queue());
    feed.removed(workspace, document);
    settle(&mut indexer, 100);

    assert_eq!(matches(&indexer, "quarterly"), 0);
    assert_eq!(indexer.queue().stats().removed, 1);
}

/// A document dropped while the indexer was not listening is found the same
/// way a new one is: by comparison, not by having been told.
#[test]
fn a_removal_missed_while_stopped_is_caught_by_comparison() {
    let fixture = Fixture::new("the quarterly plan");
    let document = fixture.document;
    let mut indexer = fixture.indexer();
    settle(&mut indexer, 0);
    assert_eq!(matches(&indexer, "quarterly"), 1);

    indexer.projections.remove(document).unwrap();
    settle(&mut indexer, RECONCILE_MILLIS * 2);

    assert_eq!(matches(&indexer, "quarterly"), 0);
}

#[test]
fn an_incompatible_source_is_removed_and_a_later_projection_returns_individually() {
    let fixture = Fixture::new("future wording");
    let (workspace, document) = (fixture.workspace, fixture.document);
    let mut indexer = fixture.indexer();
    settle(&mut indexer, 0);
    assert_eq!(matches(&indexer, "future"), 1);

    indexer
        .projections
        .mark_schema_too_new(workspace, document, b"r2", 99, 3, 2)
        .unwrap();
    let mut feed = Feed::new(indexer.queue());
    feed.source_incompatible(workspace, document);
    settle(&mut indexer, 100);
    assert_eq!(matches(&indexer, "future"), 0);

    let supported = DocumentProjection {
        workspace,
        document,
        source_revision: b"r3".to_vec(),
        nodes: vec![a_node("supported again")],
    };
    indexer.projections.replace(&supported, 3, None).unwrap();
    // No callback: an upgrade/restart must recover by cursor comparison.
    settle(&mut indexer, RECONCILE_MILLIS * 2);

    assert_eq!(matches(&indexer, "supported"), 1);
    assert_eq!(indexer.index().indexed_documents().unwrap(), 1);
}

#[test]
fn a_full_rebuild_indexes_only_one_bounded_batch_per_tick() {
    let workspace = WorkspaceId::new();
    let mut projections = Projections::in_memory().unwrap();
    let mut listing = Vec::new();
    for i in 0..(BATCH + 5) {
        let document = DocumentId::new();
        listing.push(listed(document, &format!("Doc {i}")));
        projections
            .replace(
                &DocumentProjection {
                    workspace,
                    document,
                    source_revision: vec![i as u8],
                    nodes: vec![a_node(&format!("content {i}"))],
                },
                1,
                None,
            )
            .unwrap();
    }
    projections
        .set_listing(workspace, "Acme", &listing, 1)
        .unwrap();
    let mut indexer = Indexer::new(
        SearchIndex::in_memory().unwrap(),
        projections,
        Queue::default(),
    );

    assert!(indexer.tick(0), "the initial comparison queued the rebuild");
    assert!(indexer.tick(0), "the listing cursor was rebuilt first");
    assert!(indexer.tick(0), "the first indexing batch ran");

    assert_eq!(indexer.queue().stats().indexed, BATCH as u64);
    assert_eq!(indexer.index().indexed_documents().unwrap(), BATCH);
    assert_eq!(indexer.queue().stats().pending, 5);
}

/// A rename reaches the index through the listing, and costs no re-index.
#[test]
fn a_listing_change_is_consumed_without_touching_a_document() {
    let fixture = Fixture::new("the quarterly plan");
    let (workspace, document) = (fixture.workspace, fixture.document);
    let mut indexer = fixture.indexer();
    settle(&mut indexer, 0);
    let indexed = indexer.queue().stats().indexed;

    indexer
        .projections
        .set_listing(workspace, "Acme", &[listed(document, "Plans")], 2)
        .unwrap();
    let mut feed = Feed::new(indexer.queue());
    feed.listing_changed(workspace);
    settle(&mut indexer, 100);

    let hits = indexer
        .index()
        .query(
            &crate::model::SearchQuery {
                text: "quarterly".into(),
                ..Default::default()
            },
            &[],
        )
        .unwrap()
        .hits;
    assert_eq!(hits[0].breadcrumb, "Acme / Plans");
    assert_eq!(
        indexer.queue().stats().indexed,
        indexed,
        "a rename re-indexed a document"
    );
}

/// **A document with no listing row is not indexed under a guess.** It would be
/// a result nobody can open. The indexer mirrors the listing and comes back to
/// it.
#[test]
fn a_document_with_no_listing_is_listed_first() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let mut projections = Projections::in_memory().unwrap();
    projections
        .replace(
            &DocumentProjection {
                workspace,
                document,
                source_revision: b"r1".to_vec(),
                nodes: vec![a_node("the quarterly plan")],
            },
            1,
            None,
        )
        .unwrap();
    // The listing arrives in the projection, but nothing announces it.
    projections
        .set_listing(workspace, "Acme", &[listed(document, "Roadmap")], 1)
        .unwrap();
    let mut indexer = Indexer::new(
        SearchIndex::in_memory().unwrap(),
        projections,
        Queue::default(),
    );

    settle(&mut indexer, 0);

    assert_eq!(matches(&indexer, "quarterly"), 1);
    assert_eq!(
        indexer
            .index()
            .listed(&document.to_storage())
            .unwrap()
            .unwrap()
            .title,
        "Roadmap"
    );
}

/// **A document no listing names is deferred, not re-queued forever.**
///
/// The loop this pins was found on a real installation: 5,746 of 6,754
/// projected documents belonged to no listing, so every reconcile read each of
/// them, queued it, mirrored its workspace's listing again and wrote nothing.
/// The indexer never idled — 88% of a core, indefinitely — and because the
/// queue never emptied, every search reported every workspace as *updating*
/// and real work waited behind thousands of documents that could never be
/// indexed.
///
/// The clock has to advance: with a frozen one the reconcile interval hides
/// the loop, which is why `settle` alone would have passed throughout.
#[test]
fn a_document_no_listing_names_is_deferred_rather_than_re_queued_forever() {
    let workspace = WorkspaceId::new();
    let listed_document = DocumentId::new();
    let orphan = DocumentId::new();
    let mut projections = Projections::in_memory().unwrap();
    projections
        .set_listing(workspace, "Acme", &[listed(listed_document, "Roadmap")], 1)
        .unwrap();
    for (document, text) in [
        (listed_document, "the quarterly plan"),
        (orphan, "nothing names me"),
    ] {
        projections
            .replace(
                &DocumentProjection {
                    workspace,
                    document,
                    source_revision: b"r1".to_vec(),
                    nodes: vec![a_node(text)],
                },
                1,
                None,
            )
            .unwrap();
    }
    let mut indexer = Indexer::new(
        SearchIndex::in_memory().unwrap(),
        projections,
        Queue::default(),
    );

    let mut now = 0;
    for _ in 0..10 {
        settle(&mut indexer, now);
        now += RECONCILE_MILLIS * 2;
    }

    assert_eq!(matches(&indexer, "quarterly"), 1);
    assert_eq!(
        matches(&indexer, "names"),
        0,
        "it was indexed under a guess"
    );
    let stats = indexer.queue().stats();
    assert_eq!(stats.unlisted, 1, "the orphan was not remembered");
    assert_eq!(
        stats.behind, 0,
        "a document nothing can open counted as the index trailing the store"
    );
    assert_eq!(stats.pending, 0, "the queue never emptied");
    // Ten reconciles later, one document indexed and one listing consumed.
    assert_eq!(stats.indexed, 1, "the orphan was read and re-read");
    assert_eq!(
        stats.listings, 1,
        "the listing was mirrored again per attempt"
    );
}

/// And the deferral is a wait, not a verdict: the listing that names it is what
/// ends it, with nothing else announced.
#[test]
fn a_deferred_document_is_indexed_when_a_listing_finally_names_it() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let mut projections = Projections::in_memory().unwrap();
    projections.set_listing(workspace, "Acme", &[], 1).unwrap();
    projections
        .replace(
            &DocumentProjection {
                workspace,
                document,
                source_revision: b"r1".to_vec(),
                nodes: vec![a_node("the quarterly plan")],
            },
            1,
            None,
        )
        .unwrap();
    let mut indexer = Indexer::new(
        SearchIndex::in_memory().unwrap(),
        projections,
        Queue::default(),
    );
    settle(&mut indexer, 0);
    assert_eq!(matches(&indexer, "quarterly"), 0);
    assert_eq!(indexer.queue().stats().unlisted, 1);

    indexer
        .projections
        .set_listing(workspace, "Acme", &[listed(document, "Roadmap")], 2)
        .unwrap();
    settle(&mut indexer, RECONCILE_MILLIS * 2);

    assert_eq!(matches(&indexer, "quarterly"), 1);
    assert_eq!(indexer.queue().stats().unlisted, 0);
}

/// **Equal is current; everything else is work.** A recovered document's
/// revision can go *backwards*, and a rule of "only newer replaces older" would
/// leave it stale for good with nothing to notice.
#[test]
fn a_revision_that_went_backwards_is_still_re_indexed() {
    let fixture = Fixture::new("after the recovery");
    let (workspace, document) = (fixture.workspace, fixture.document);
    let mut indexer = fixture.indexer();
    settle(&mut indexer, 0);

    // The store fell back to a checkpoint: an older revision, older content.
    indexer
        .projections
        .replace(
            &DocumentProjection {
                workspace,
                document,
                source_revision: b"r0".to_vec(),
                nodes: vec![a_node("before the recovery")],
            },
            2,
            None,
        )
        .unwrap();
    settle(&mut indexer, RECONCILE_MILLIS * 2);

    assert_eq!(matches(&indexer, "after"), 0);
    assert_eq!(matches(&indexer, "before"), 1);
}

/// An idle store must not be comparing two tables several times a second.
#[test]
fn an_idle_index_reconciles_on_an_interval_rather_than_every_tick() {
    let fixture = Fixture::new("the quarterly plan");
    let mut indexer = fixture.indexer();
    settle(&mut indexer, 0);

    indexer
        .projections
        .replace(
            &DocumentProjection {
                workspace: indexer
                    .projections
                    .read(
                        *indexer
                            .projections
                            .revisions()
                            .unwrap()
                            .keys()
                            .next()
                            .unwrap(),
                    )
                    .unwrap()
                    .unwrap()
                    .workspace,
                document: *indexer
                    .projections
                    .revisions()
                    .unwrap()
                    .keys()
                    .next()
                    .unwrap(),
                source_revision: b"r2".to_vec(),
                nodes: vec![a_node("changed underneath")],
            },
            2,
            None,
        )
        .unwrap();

    // Well inside the interval: nothing is noticed yet.
    assert!(!indexer.tick(RECONCILE_MILLIS / 2));
    assert_eq!(matches(&indexer, "changed"), 0);

    settle(&mut indexer, RECONCILE_MILLIS * 2);
    assert_eq!(matches(&indexer, "changed"), 1);
}

// ------------------------------------------------------------------- tags

const TAG_A: &str = "0191d0f2-0000-7000-8000-00000000000a";
const TAG_B: &str = "0191d0f2-0000-7000-8000-00000000000b";

/// A tag table this test controls, and a count of how often the indexer asked
/// for it — which is how "cached, not read per node" is a fact rather than a
/// hope.
#[derive(Clone, Default)]
struct FakeTags(Arc<Mutex<(Vec<(String, String)>, usize)>>);

impl FakeTags {
    fn new(pairs: &[(&str, &str)]) -> Self {
        let this = Self::default();
        this.set(pairs);
        this
    }

    fn set(&self, pairs: &[(&str, &str)]) {
        self.0.lock().unwrap().0 = pairs
            .iter()
            .map(|(id, name)| ((*id).to_owned(), (*name).to_owned()))
            .collect();
    }

    fn reads(&self) -> usize {
        self.0.lock().unwrap().1
    }
}

impl TagNames for FakeTags {
    fn tag_names(&self, _workspace: WorkspaceId) -> Option<Vec<(String, String)>> {
        let mut inner = self.0.lock().unwrap();
        inner.1 += 1;
        Some(inner.0.clone())
    }
}

fn tagged_node(text: &str, tags: &[&str]) -> ProjectedNode {
    let mut node = a_node(text);
    node.attrs = tags
        .iter()
        .enumerate()
        .map(|(i, tag)| koru_projection::model::ProjectedAttr {
            id: format!("a{i}"),
            role: crate::tags::TAG_ROLE.to_owned(),
            kind: "text".to_owned(),
            value: (*tag).to_owned(),
            zone: None,
            label: None,
        })
        .collect();
    node
}

fn system_entry(document: DocumentId) -> ListedDocument {
    ListedDocument {
        kind: crate::tags::KIND_SYSTEM.to_owned(),
        ..listed(document, "System")
    }
}

/// A workspace with a system document, one tagged document and one that is not.
struct Tagged {
    workspace: WorkspaceId,
    system: DocumentId,
    plain: DocumentId,
    tags: FakeTags,
    indexer: Indexer,
}

impl Tagged {
    fn new(pairs: &[(&str, &str)], on: &[&str]) -> Self {
        let workspace = WorkspaceId::new();
        let (system, tagged, plain) = (DocumentId::new(), DocumentId::new(), DocumentId::new());
        let mut projections = Projections::in_memory().unwrap();
        projections
            .set_listing(
                workspace,
                "Acme",
                &[
                    system_entry(system),
                    listed(tagged, "Roadmap"),
                    listed(plain, "Notes"),
                ],
                1,
            )
            .unwrap();
        for (document, nodes) in [
            (system, Vec::new()),
            (tagged, vec![tagged_node("Hello", on)]),
            (plain, vec![a_node("nothing to declare")]),
        ] {
            projections
                .replace(
                    &DocumentProjection {
                        workspace,
                        document,
                        source_revision: b"r1".to_vec(),
                        nodes,
                    },
                    1,
                    None,
                )
                .unwrap();
        }
        let tags = FakeTags::new(pairs);
        let indexer = Indexer::new(
            SearchIndex::in_memory().unwrap(),
            projections,
            Queue::default(),
        )
        .with_tag_names(Box::new(tags.clone()));
        Self {
            workspace,
            system,
            plain,
            tags,
            indexer,
        }
    }

    /// The projector's announcement that the system document changed.
    fn system_changed(&mut self, revision: &[u8]) {
        let projection = DocumentProjection {
            workspace: self.workspace,
            document: self.system,
            source_revision: revision.to_vec(),
            nodes: Vec::new(),
        };
        self.indexer
            .projections
            .replace(&projection, 2, None)
            .unwrap();
        Feed::new(self.indexer.queue()).projected(&projection);
    }
}

/// **The decision.** Searching the word finds the block carrying the tag.
#[test]
fn a_tagged_node_is_found_by_the_tags_name() {
    let mut fixture = Tagged::new(&[(TAG_A, "amazing")], &[TAG_A]);
    settle(&mut fixture.indexer, 0);

    assert_eq!(matches(&fixture.indexer, "amazing"), 1);
    assert_eq!(
        matches(&fixture.indexer, TAG_A),
        0,
        "the id is not a word anybody types"
    );
}

/// A rename is one record in the system document. It reaches the index as that
/// document reprojecting, and the documents carrying the id are rebuilt.
#[test]
fn renaming_a_tag_re_indexes_what_carries_it() {
    let mut fixture = Tagged::new(&[(TAG_A, "amazing")], &[TAG_A]);
    settle(&mut fixture.indexer, 0);
    let indexed = fixture.indexer.queue().stats().indexed;

    fixture.tags.set(&[(TAG_A, "astonishing")]);
    fixture.system_changed(b"r2");
    settle(&mut fixture.indexer, 100);

    assert_eq!(matches(&fixture.indexer, "astonishing"), 1);
    assert_eq!(
        matches(&fixture.indexer, "amazing"),
        0,
        "the old name is still matchable"
    );
    // The system document itself, plus the one document carrying the tag.
    assert_eq!(fixture.indexer.queue().stats().indexed, indexed + 2);
}

/// **Not the whole workspace.** A system document reprojects for a colour, a
/// new attribute definition, a merge onto the same spelling — none of which
/// changes a byte of a fragment. Only a name that moved is work, and only the
/// documents carrying *that* id are.
#[test]
fn a_system_change_that_renames_nothing_re_indexes_nothing() {
    let mut fixture = Tagged::new(&[(TAG_A, "amazing"), (TAG_B, "mvp")], &[TAG_A]);
    settle(&mut fixture.indexer, 0);
    let indexed = fixture.indexer.queue().stats().indexed;

    fixture.system_changed(b"r2");
    settle(&mut fixture.indexer, 100);

    assert_eq!(
        fixture.indexer.queue().stats().indexed,
        indexed + 1,
        "only the system document itself was re-indexed"
    );

    // And a rename of a tag nothing carries reaches no document either.
    fixture.tags.set(&[(TAG_A, "amazing"), (TAG_B, "later")]);
    fixture.system_changed(b"r3");
    settle(&mut fixture.indexer, 200);
    assert_eq!(fixture.indexer.queue().stats().indexed, indexed + 2);
    assert_eq!(matches(&fixture.indexer, "amazing"), 1);
}

/// A merged-away, forgotten id has no name. No fragment, no panic, no blank —
/// and the node's own text is untouched.
#[test]
fn a_forgotten_tag_id_degrades_quietly() {
    let mut fixture = Tagged::new(&[(TAG_A, "amazing")], &[TAG_A, TAG_B]);
    settle(&mut fixture.indexer, 0);

    assert_eq!(matches(&fixture.indexer, "amazing"), 1);
    assert_eq!(matches(&fixture.indexer, "Hello"), 1);
    assert_eq!(matches(&fixture.indexer, TAG_B), 0);

    // And when the last name goes, the block is still findable by its text.
    fixture.tags.set(&[]);
    fixture.system_changed(b"r2");
    settle(&mut fixture.indexer, 100);
    assert_eq!(matches(&fixture.indexer, "amazing"), 0);
    assert_eq!(matches(&fixture.indexer, "Hello"), 1);
}

/// **Per workspace, not per node.** The table is read when a workspace is first
/// needed and when its system document reprojects — never once per document and
/// never once per attribute.
#[test]
fn the_table_is_read_per_workspace_rather_than_per_node() {
    let mut fixture = Tagged::new(&[(TAG_A, "amazing")], &[TAG_A]);
    settle(&mut fixture.indexer, 0);

    // Three documents indexed; at most one lazy read plus one on the system
    // document's own pass.
    assert!(
        fixture.tags.reads() <= 2,
        "read {} times for three documents",
        fixture.tags.reads()
    );
    let after_first = fixture.tags.reads();

    // A plain document changing does not re-read it.
    let projection = DocumentProjection {
        workspace: fixture.workspace,
        document: fixture.plain,
        source_revision: b"r2".to_vec(),
        nodes: vec![a_node("still nothing to declare")],
    };
    fixture
        .indexer
        .projections
        .replace(&projection, 2, None)
        .unwrap();
    Feed::new(fixture.indexer.queue()).projected(&projection);
    settle(&mut fixture.indexer, 100);

    assert_eq!(fixture.tags.reads(), after_first);
}

/// A source that cannot answer yet, and then can.
#[derive(Clone, Default)]
struct LateTags(Arc<Mutex<Option<Vec<(String, String)>>>>);

impl LateTags {
    fn arrive(&self, pairs: &[(&str, &str)]) {
        *self.0.lock().unwrap() = Some(
            pairs
                .iter()
                .map(|(id, name)| ((*id).to_owned(), (*name).to_owned()))
                .collect(),
        );
    }
}

impl TagNames for LateTags {
    fn tag_names(&self, _workspace: WorkspaceId) -> Option<Vec<(String, String)>> {
        self.0.lock().unwrap().clone()
    }
}

/// **The cold-rebuild order.** A document indexed while the workspace's tag
/// table could not be read carries the id, and must not carry it forever: the
/// first *successful* read is a change like any other, and everything the table
/// names is due.
///
/// The trigger here is deliberately an ordinary document rather than the system
/// one. A system document reprojecting was always covered; every other order
/// was not, because the lazy read used to install the table quietly — so a
/// rebuild that indexed content before the table was readable answered `#tag`
/// queries with nothing, permanently, and nothing said why.
#[test]
fn a_tag_table_that_arrives_late_re_indexes_what_was_indexed_without_it() {
    let workspace = WorkspaceId::new();
    let (system, tagged, plain) = (DocumentId::new(), DocumentId::new(), DocumentId::new());
    let mut projections = Projections::in_memory().unwrap();
    projections
        .set_listing(
            workspace,
            "Acme",
            &[
                system_entry(system),
                listed(tagged, "Roadmap"),
                listed(plain, "Notes"),
            ],
            1,
        )
        .unwrap();
    for (document, nodes) in [
        (system, Vec::new()),
        (tagged, vec![tagged_node("Hello", &[TAG_A])]),
        (plain, vec![a_node("nothing to declare")]),
    ] {
        projections
            .replace(
                &DocumentProjection {
                    workspace,
                    document,
                    source_revision: b"r1".to_vec(),
                    nodes,
                },
                1,
                None,
            )
            .unwrap();
    }
    let tags = LateTags::default();
    let mut indexer = Indexer::new(
        SearchIndex::in_memory().unwrap(),
        projections,
        Queue::default(),
    )
    .with_tag_names(Box::new(tags.clone()));

    // Nothing can answer yet: the id is indexed, rather than the tag vanishing.
    settle(&mut indexer, 0);
    assert_eq!(matches(&indexer, TAG_A), 1);
    assert_eq!(matches(&indexer, "amazing"), 0);

    // The table becomes readable, and an ordinary document is what asks next.
    tags.arrive(&[(TAG_A, "amazing")]);
    let projection = DocumentProjection {
        workspace,
        document: plain,
        source_revision: b"r2".to_vec(),
        nodes: vec![a_node("still nothing to declare")],
    };
    indexer.projections.replace(&projection, 2, None).unwrap();
    Feed::new(indexer.queue()).projected(&projection);
    settle(&mut indexer, 100);

    assert_eq!(matches(&indexer, "amazing"), 1, "the tag kept its id");
    assert_eq!(matches(&indexer, TAG_A), 0);
}

/// **No source is not an empty table.** A host that wired none keeps indexing
/// the id, exactly as the index did before names were resolved at all — rather
/// than dropping every tag while it waits for something that never comes.
#[test]
fn with_no_tag_source_the_id_is_still_indexed() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let mut projections = Projections::in_memory().unwrap();
    projections
        .set_listing(workspace, "Acme", &[listed(document, "Roadmap")], 1)
        .unwrap();
    projections
        .replace(
            &DocumentProjection {
                workspace,
                document,
                source_revision: b"r1".to_vec(),
                nodes: vec![tagged_node("Hello", &[TAG_A])],
            },
            1,
            None,
        )
        .unwrap();
    let mut indexer = Indexer::new(
        SearchIndex::in_memory().unwrap(),
        projections,
        Queue::default(),
    );

    settle(&mut indexer, 0);

    assert_eq!(matches(&indexer, TAG_A), 1);
}

/// **A document no listing names is skipped, counted, and not asked about
/// again.** This is the one that span.
///
/// The read model can hold a document the workspace listing does not name — on
/// the machine this was found on, 319 of them across four workspaces, because
/// their content had reached the store and their manifest entry had not.
/// `index_one` requires a listing row, so it abandoned each one without
/// recording anything; the next reconcile found it again and queued it again;
/// and because `reconcile` answered "found work" every time, the driver's
/// `if !busy { sleep }` never fired. The result was a permanent spin rewriting
/// the workspace listing at several hundred megabytes a second, logging
/// nothing, because giving up there was not a failure.
///
/// The assertion is on `tick` answering `false` *after the reconcile interval
/// has passed*, which is the property that actually broke — settling once was
/// never the problem.
#[test]
fn a_document_no_listing_names_is_skipped_rather_than_queued_forever() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let mut projections = Projections::in_memory().unwrap();
    projections
        .replace(
            &DocumentProjection {
                workspace,
                document,
                source_revision: b"r1".to_vec(),
                nodes: vec![a_node("the quarterly plan")],
            },
            1,
            None,
        )
        .unwrap();
    // A listing for the workspace that does not name this document — which is
    // the shape of the real thing, rather than no listing at all.
    projections.set_listing(workspace, "Acme", &[], 1).unwrap();
    let mut indexer = Indexer::new(
        SearchIndex::in_memory().unwrap(),
        projections,
        Queue::default(),
    );

    settle(&mut indexer, 0);
    assert_eq!(matches(&indexer, "quarterly"), 0, "it cannot be indexed");
    assert_eq!(
        indexer.queue.lock().stats.unlisted,
        1,
        "and the count says so, rather than the whole thing being silent"
    );

    // The reconcile interval passes: there must still be nothing to do.
    for round in 1..4 {
        let busy = indexer.tick(RECONCILE_MILLIS * 2 * round);
        assert!(
            !busy,
            "round {round}: the indexer queued work it can never complete"
        );
    }
}

/// **...and it is not marked done either.** The tempting fix is to record a
/// cursor so the reconcile stops finding it. That is worse: a cursor is keyed
/// on `source_revision`, so a document marked done at revision `r1` would stay
/// unindexed when its listing arrived later without its content changing —
/// trading a loud loop for a document that is quietly unsearchable for good.
#[test]
fn a_listing_that_arrives_late_still_indexes_the_document() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let mut projections = Projections::in_memory().unwrap();
    projections
        .replace(
            &DocumentProjection {
                workspace,
                document,
                source_revision: b"r1".to_vec(),
                nodes: vec![a_node("the quarterly plan")],
            },
            1,
            None,
        )
        .unwrap();
    projections.set_listing(workspace, "Acme", &[], 1).unwrap();
    let mut indexer = Indexer::new(
        SearchIndex::in_memory().unwrap(),
        projections,
        Queue::default(),
    );
    settle(&mut indexer, 0);
    assert_eq!(matches(&indexer, "quarterly"), 0);

    // The manifest entry turns up. The content has not changed, so the
    // revision has not either — which is exactly the case a recorded cursor
    // would have swallowed.
    indexer
        .projections
        .set_listing(workspace, "Acme", &[listed(document, "Roadmap")], 2)
        .unwrap();

    settle(&mut indexer, RECONCILE_MILLIS * 2);

    assert_eq!(matches(&indexer, "quarterly"), 1, "it is searchable now");
    assert_eq!(indexer.queue.lock().stats.unlisted, 0);
}

// --------------------------------------------------------------- readings

const MANUAL: &str = "b3:0001";
const PHOTO: &str = "b3:0002";

/// A readings source this test controls: `None` is *cannot answer now*, and a
/// hash missing from `Some` has no reading. It counts how often it was asked.
#[derive(Clone, Default)]
struct FakeReadings(Arc<Mutex<(Option<Vec<ReadingText>>, usize)>>);

impl FakeReadings {
    fn answering(readings: Vec<ReadingText>) -> Self {
        let this = Self::default();
        this.set(Some(readings));
        this
    }

    fn set(&self, readings: Option<Vec<ReadingText>>) {
        self.0.lock().unwrap().0 = readings;
    }

    fn asked(&self) -> usize {
        self.0.lock().unwrap().1
    }
}

impl Readings for FakeReadings {
    fn readings(&self, _workspace: WorkspaceId, hashes: &[String]) -> Option<Vec<ReadingText>> {
        let mut inner = self.0.lock().unwrap();
        inner.1 += 1;
        inner.0.as_ref().map(|all| {
            all.iter()
                .filter(|r| hashes.contains(&r.hash))
                .cloned()
                .collect()
        })
    }
}

fn reading(hash: &str, page_count: u32, pages: &[(u32, &str)]) -> ReadingText {
    ReadingText {
        hash: hash.to_owned(),
        page_count,
        pages: pages.iter().map(|(p, t)| (*p, (*t).to_owned())).collect(),
    }
}

/// The forty-page manual of the plan's verification, its words spread so that
/// two of them meet on one page and two never do.
fn the_manual() -> ReadingText {
    reading(
        MANUAL,
        40,
        &[
            (1, "contents and safety notices"),
            (2, "unpack the gearbox"),
            (3, "torque the flange bolts evenly"),
            (7, "replace the gasket"),
        ],
    )
}

/// A node holding a blob: the root of a file document, or a picture in a page.
fn blob_node(id: &str, kind: &str, name: &str, hash: &str) -> ProjectedNode {
    ProjectedNode {
        id: id.to_owned(),
        kind: kind.to_owned(),
        text: String::new(),
        name: Some(name.to_owned()),
        blob: Some(hash.to_owned()),
        mime: Some("application/octet-stream".to_owned()),
        ..a_node("")
    }
}

/// A workspace holding a file (the manual), a page with a picture in it, and a
/// page with neither.
struct Library {
    workspace: WorkspaceId,
    file: DocumentId,
    page: DocumentId,
    plain: DocumentId,
    readings: FakeReadings,
    indexer: Indexer,
}

impl Library {
    fn new(readings: FakeReadings) -> Self {
        Self::with_listing(readings, |entries| entries)
    }

    fn with_listing(
        readings: FakeReadings,
        listing: impl FnOnce(Vec<ListedDocument>) -> Vec<ListedDocument>,
    ) -> Self {
        let workspace = WorkspaceId::new();
        let (file, page, plain) = (DocumentId::new(), DocumentId::new(), DocumentId::new());
        let mut projections = Projections::in_memory().unwrap();
        projections
            .set_listing(
                workspace,
                "Acme",
                &listing(vec![
                    ListedDocument {
                        kind: "file".to_owned(),
                        mime: Some("application/pdf".to_owned()),
                        ..listed(file, "manual.pdf")
                    },
                    listed(page, "Site visit"),
                    listed(plain, "Notes"),
                ]),
                1,
            )
            .unwrap();
        for (document, nodes) in [
            (file, vec![blob_node("root", "file", "manual.pdf", MANUAL)]),
            (
                page,
                vec![
                    a_node("we photographed the board"),
                    blob_node("img", "image", "board.jpg", PHOTO),
                ],
            ),
            (plain, vec![a_node("nothing read here")]),
        ] {
            projections
                .replace(
                    &DocumentProjection {
                        workspace,
                        document,
                        source_revision: b"r1".to_vec(),
                        nodes,
                    },
                    1,
                    None,
                )
                .unwrap();
        }
        let indexer = Indexer::new(
            SearchIndex::in_memory().unwrap(),
            projections,
            Queue::default(),
        )
        .with_readings(Box::new(readings.clone()));
        Self {
            workspace,
            file,
            page,
            plain,
            readings,
            indexer,
        }
    }

    fn hits(&self, text: &str) -> Vec<crate::model::SearchHit> {
        self.hits_in(text, crate::model::LifecycleFilter::Live)
    }

    fn hits_in(
        &self,
        text: &str,
        lifecycle: crate::model::LifecycleFilter,
    ) -> Vec<crate::model::SearchHit> {
        self.indexer
            .index()
            .query(
                &crate::model::SearchQuery {
                    text: text.into(),
                    lifecycle,
                    ..Default::default()
                },
                &[],
            )
            .unwrap()
            .hits
    }

    /// Whether the file is still found by what it was found by before
    /// readings: its listed title and its node's name.
    fn file_found_by_name(&self) -> bool {
        let fields: Vec<String> = self
            .hits("manual")
            .into_iter()
            .filter(|hit| hit.document.as_deref() == Some(self.file.to_storage().as_str()))
            .map(|hit| hit.field)
            .collect();
        fields.iter().any(|f| f == crate::model::field::TITLE)
            && fields.iter().any(|f| f == crate::model::field::BODY)
            && !fields.iter().any(|f| f == crate::model::field::READ)
    }

    /// Hits on words read out of something, as `(document, page)`.
    fn read_hits(&self, text: &str) -> Vec<(String, Option<u32>)> {
        self.hits(text)
            .into_iter()
            .filter(|hit| hit.field == crate::model::field::READ)
            .map(|hit| (hit.document.unwrap(), hit.page))
            .collect()
    }
}

/// **The plan's first verification.** A word on page 3 finds the file, and the
/// hit says page 3.
#[test]
fn a_word_on_page_three_finds_the_file_with_page_three() {
    let mut library = Library::new(FakeReadings::answering(vec![the_manual()]));
    settle(&mut library.indexer, 0);

    let hits = library.hits("torque");
    assert_eq!(hits.len(), 1, "{hits:?}");
    let hit = &hits[0];
    assert_eq!(
        hit.document.as_deref(),
        Some(library.file.to_storage().as_str())
    );
    assert_eq!(hit.field, crate::model::field::READ);
    assert_eq!(hit.page, Some(3));
    assert_eq!(hit.node.as_deref(), Some("root"));
    assert_eq!(hit.title, "manual.pdf", "the row names the file");
    assert!(
        hit.snippet.contains("torque"),
        "the snippet is that page's words: {:?}",
        hit.snippet
    );
    assert!(!hit.spans.is_empty(), "and the matched word is highlighted");
}

/// **Every term co-occurs in one fragment, and a fragment is a page.** Two
/// words on one page find it; two words on different pages find nothing —
/// with no query logic of its own.
#[test]
fn two_words_match_on_one_page_and_never_across_two() {
    let mut library = Library::new(FakeReadings::answering(vec![the_manual()]));
    settle(&mut library.indexer, 0);

    assert_eq!(
        library.read_hits("flange bolts"),
        [(library.file.to_storage(), Some(3))]
    );
    assert!(
        library.read_hits("torque gasket").is_empty(),
        "page 3 and page 7 were assembled into one answer"
    );
    assert!(library.hits("torque gasket").is_empty());
}

/// A picture, a one-page PDF and a plain-text file have no page a person would
/// be told about.
#[test]
fn a_one_page_reading_carries_no_page() {
    let mut library = Library::new(FakeReadings::answering(vec![
        reading(MANUAL, 1, &[(1, "a single page of warranty terms")]),
        reading(PHOTO, 1, &[(1, "whiteboard sketch of the pump")]),
    ]));
    settle(&mut library.indexer, 0);

    assert_eq!(
        library.read_hits("warranty"),
        [(library.file.to_storage(), None)]
    );
    assert_eq!(
        library.read_hits("whiteboard"),
        [(library.page.to_storage(), None)],
        "a picture in a page is found in that page"
    );
}

/// A file in the workspace's Trash keeps its words, and they answer the Trash
/// filter only — the bin is a place somebody searches and restores from.
#[test]
fn a_trashed_files_words_answer_only_the_trash_filter() {
    let mut library = Library::with_listing(
        FakeReadings::answering(vec![the_manual()]),
        |mut entries| {
            entries[0].trashed_at = Some(42);
            entries
        },
    );
    settle(&mut library.indexer, 0);

    assert!(
        library.hits("torque").is_empty(),
        "a binned file in a live answer"
    );
    let binned = library.hits_in("torque", crate::model::LifecycleFilter::Trash);
    assert_eq!(binned.len(), 1);
    assert_eq!(binned[0].page, Some(3));
    assert_eq!(binned[0].lifecycle, crate::model::Lifecycle::Trashed);
}

/// **`None` is *not now*.** Names and titles are indexed as they always were,
/// and no words are — not an empty reading, and not an error.
#[test]
fn a_source_that_cannot_answer_indexes_names_and_titles_and_no_words() {
    let readings = FakeReadings::default();
    let mut library = Library::new(readings.clone());
    settle(&mut library.indexer, 0);

    assert!(library.read_hits("torque").is_empty());
    assert!(library.file_found_by_name(), "the file's title and name");
    assert_eq!(
        library.hits("board").len(),
        2,
        "the picture's name, and the line beside it"
    );
    assert!(
        readings.asked() >= 2,
        "it was asked, for the file and the page"
    );
    assert_eq!(
        library.indexer.queue().stats().failed,
        0,
        "not being able to answer is not a failure"
    );
}

/// **And it is asked again.** A document indexed while the source could not
/// answer is not left without words for good: the reconcile asks once per
/// workspace whether the source can answer yet, and re-indexes what waited.
#[test]
fn words_that_could_not_be_had_are_indexed_once_the_source_answers() {
    let readings = FakeReadings::default();
    let mut library = Library::new(readings.clone());
    settle(&mut library.indexer, 0);
    assert!(library.read_hits("torque").is_empty());

    readings.set(Some(vec![the_manual()]));
    settle(&mut library.indexer, RECONCILE_MILLIS * 2);

    assert_eq!(
        library.read_hits("torque"),
        [(library.file.to_storage(), Some(3))]
    );
    // And once it has answered, an idle index stops asking.
    let asked = readings.asked();
    settle(&mut library.indexer, RECONCILE_MILLIS * 4);
    assert_eq!(
        readings.asked(),
        asked,
        "it went on probing a source that answers"
    );
}

/// **A reading that disappears takes its words with it** on the next index of
/// the document — a hash missing from a `Some` answer has no reading.
#[test]
fn a_reading_that_disappears_from_the_source_leaves_the_index() {
    let readings = FakeReadings::answering(vec![the_manual()]);
    let mut library = Library::new(readings.clone());
    settle(&mut library.indexer, 0);
    assert_eq!(library.read_hits("torque").len(), 1);

    readings.set(Some(Vec::new()));
    library
        .indexer
        .queue()
        .enqueue_document(library.workspace, library.file);
    settle(&mut library.indexer, 100);

    assert!(library.read_hits("torque").is_empty());
    assert!(
        library.file_found_by_name(),
        "the file is still found by its name"
    );
}

/// **The daemon's two doors.** A reading lands for the manual: its parent is
/// named, and the hash reaches every other document carrying it — here a page
/// the same PDF was dropped into — and nothing else.
#[test]
fn a_changed_reading_re_indexes_exactly_the_documents_carrying_it() {
    let readings = FakeReadings::answering(Vec::new());
    let mut library = Library::new(readings.clone());
    settle(&mut library.indexer, 0);

    // The same PDF also sits in the page, beside the picture.
    let projection = DocumentProjection {
        workspace: library.workspace,
        document: library.page,
        source_revision: b"r2".to_vec(),
        nodes: vec![
            a_node("we photographed the board"),
            blob_node("img", "image", "board.jpg", PHOTO),
            blob_node("pdf", "file", "manual.pdf", MANUAL),
        ],
    };
    library
        .indexer
        .projections
        .replace(&projection, 2, None)
        .unwrap();
    settle(&mut library.indexer, RECONCILE_MILLIS * 2);
    let indexed = library.indexer.queue().stats().indexed;
    assert!(
        library.read_hits("torque").is_empty(),
        "nothing has been read yet"
    );

    // The reading lands. Nothing about either document changed.
    readings.set(Some(vec![the_manual()]));
    let queue = library.indexer.queue();
    queue.enqueue_document(library.workspace, library.file);
    queue.enqueue_hashes(library.workspace, [MANUAL]);
    assert_eq!(
        queue.pending_workspaces(),
        vec![library.workspace],
        "a workspace with readings to apply is updating"
    );
    settle(&mut library.indexer, RECONCILE_MILLIS * 2 + 100);

    let mut found = library.read_hits("torque");
    found.sort();
    let mut expected = vec![
        (library.file.to_storage(), Some(3)),
        (library.page.to_storage(), Some(3)),
    ];
    expected.sort();
    assert_eq!(found, expected);
    assert_eq!(
        library.indexer.queue().stats().indexed,
        indexed + 2,
        "the file and the page, once each, and not the page with no picture"
    );
    let _ = library.plain;
}

/// **The edit path asks too.** A picture pasted into a page, whose bytes were
/// already read elsewhere, is findable by those words the moment the page is
/// indexed — no reading changed, so nothing else would say so.
#[test]
fn a_picture_pasted_into_a_page_is_findable_by_words_read_elsewhere() {
    let readings = FakeReadings::answering(vec![reading(
        PHOTO,
        1,
        &[(1, "whiteboard sketch of the pump")],
    )]);
    let mut library = Library::new(readings);
    settle(&mut library.indexer, 0);
    assert_eq!(
        library.read_hits("pump").len(),
        1,
        "the page that already has it"
    );

    let projection = DocumentProjection {
        workspace: library.workspace,
        document: library.plain,
        source_revision: b"r2".to_vec(),
        nodes: vec![
            a_node("nothing read here"),
            blob_node("pasted", "image", "copy.jpg", PHOTO),
        ],
    };
    library
        .indexer
        .projections
        .replace(&projection, 2, None)
        .unwrap();
    Feed::new(library.indexer.queue()).projected(&projection);
    settle(&mut library.indexer, 100);

    let mut found: Vec<String> = library
        .read_hits("pump")
        .into_iter()
        .map(|(document, _)| document)
        .collect();
    found.sort();
    let mut expected = vec![library.page.to_storage(), library.plain.to_storage()];
    expected.sort();
    assert_eq!(found, expected);
}

/// **Read text ranks with tables and properties**, below every kind of typed
/// text: the line somebody wrote comes before the words read out of a picture.
#[test]
fn what_somebody_wrote_outranks_what_was_read() {
    let mut library = Library::new(FakeReadings::answering(vec![reading(
        PHOTO,
        1,
        &[(1, "the board shows the schedule")],
    )]));
    settle(&mut library.indexer, 0);

    let hits = library.hits("board");
    let fields: Vec<&str> = hits.iter().map(|hit| hit.field.as_str()).collect();
    let body = fields.iter().position(|f| *f == crate::model::field::BODY);
    let read = fields.iter().position(|f| *f == crate::model::field::READ);
    assert!(
        body.is_some() && read.is_some() && body < read,
        "the typed line did not rank above the read one: {fields:?}"
    );
    let score = |field: &str| hits.iter().find(|h| h.field == field).unwrap().score;
    assert!(score(crate::model::field::BODY) > score(crate::model::field::READ));
}

/// With no source wired a picture and a file are found by their names, exactly
/// as before readings existed, and nobody is asked anything.
#[test]
fn with_no_readings_source_nothing_is_read_and_nothing_waits() {
    let mut library = Library::new(FakeReadings::default());
    library.indexer.readings = None;
    settle(&mut library.indexer, 0);

    assert!(library.read_hits("torque").is_empty());
    assert!(library.file_found_by_name());
    assert!(library.indexer.unread.is_empty());
    assert_eq!(library.readings.asked(), 0);
}
