// SPDX-License-Identifier: MPL-2.0

//! What the read model promises about what comes back out of it.

use super::*;
use crate::model::ProjectedNode;
use koru_store_core::{TargetState, TemporalValue};

fn a_node(id: &str, text: &str) -> ProjectedNode {
    ProjectedNode {
        id: id.to_owned(),
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
        in_trash: false,
        runs: Vec::new(),
        attrs: Vec::new(),
    }
}

/// The same node, in the document's own bin.
fn a_binned_node(id: &str, text: &str) -> ProjectedNode {
    ProjectedNode {
        in_trash: true,
        ..a_node(id, text)
    }
}

fn a_projection(
    workspace: WorkspaceId,
    document: DocumentId,
    revision: &[u8],
) -> DocumentProjection {
    DocumentProjection {
        workspace,
        document,
        source_revision: revision.to_vec(),
        nodes: vec![a_node("n1", "first"), a_node("n2", "second")],
    }
}

#[test]
fn what_was_written_is_what_comes_back() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());

    let mut projection = a_projection(ws, doc, b"rev-1");
    projection.nodes[0].heading = Some(2);
    projection.nodes[0].done = true;
    projection.nodes[0].attrs.push(ProjectedAttr {
        id: "a1".into(),
        role: "due".into(),
        kind: "date".into(),
        value: "2026-03-01".into(),
        zone: None,
        label: None,
    });
    projection.nodes[1].runs.push(ProjectedRun {
        start: 0,
        end: 3,
        bold: true,
        ..Default::default()
    });

    p.replace(&projection, 1, None).unwrap();
    let read = p.read(doc).unwrap().expect("a projection");

    assert_eq!(read.workspace, ws);
    assert_eq!(read.source_revision, b"rev-1");
    assert_eq!(read.nodes.len(), 2);
    assert_eq!(read.nodes[0].heading, Some(2));
    assert!(read.nodes[0].done);
    assert_eq!(read.nodes[0].attrs[0].kind, "date");
    assert_eq!(read.nodes[0].attrs[0].value, "2026-03-01");
    assert_eq!(read.nodes[1].runs.len(), 1);
    assert!(read.nodes[1].runs[0].bold);
}

/// **Whole-document replacement means whole.** A node that is gone must not
/// linger — a stale row is a search hit on text nobody can find.
#[test]
fn replacing_a_document_removes_what_is_no_longer_in_it() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    p.replace(&a_projection(ws, doc, b"rev-1"), 1, None)
        .unwrap();

    let mut smaller = a_projection(ws, doc, b"rev-2");
    smaller.nodes.truncate(1);
    p.replace(&smaller, 2, None).unwrap();

    let read = p.read(doc).unwrap().unwrap();
    assert_eq!(read.nodes.len(), 1);
    assert_eq!(read.nodes[0].text, "first");
    assert_eq!(read.source_revision, b"rev-2");
}

/// One document's replacement must not touch another's — the unit of work is
/// one document, and this is the assertion that says so about the storage.
#[test]
fn replacing_one_document_leaves_another_alone() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let (one, two) = (DocumentId::new(), DocumentId::new());
    p.replace(&a_projection(ws, one, b"a"), 1, None).unwrap();
    p.replace(&a_projection(ws, two, b"b"), 1, None).unwrap();

    let mut changed = a_projection(ws, one, b"a2");
    changed.nodes.clear();
    p.replace(&changed, 2, None).unwrap();

    assert_eq!(p.read(one).unwrap().unwrap().nodes.len(), 0);
    assert_eq!(p.read(two).unwrap().unwrap().nodes.len(), 2);
    assert_eq!(p.documents().unwrap(), 2);
}

/// **Equal is current; everything else is a re-projection.**
///
/// Not an ordering. Two revisions can be incomparable, and a *recovered*
/// document's can go backwards — the store fell back to a checkpoint. A rule of
/// "only a newer revision may replace an older one" would leave that document
/// stale for good, with nothing to notice.
#[test]
fn staleness_is_equality_not_an_ordering() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());

    assert_eq!(p.staleness(doc, b"anything").unwrap(), Staleness::Missing);

    p.replace(&a_projection(ws, doc, b"rev-5"), 1, None)
        .unwrap();
    assert_eq!(p.staleness(doc, b"rev-5").unwrap(), Staleness::Current);
    assert_eq!(p.staleness(doc, b"rev-6").unwrap(), Staleness::Stale);
    // The one a naive comparison gets wrong: the store went *backwards*.
    assert_eq!(p.staleness(doc, b"rev-2").unwrap(), Staleness::Stale);
}

#[test]
fn a_too_new_source_is_settled_without_replacing_compatible_rows() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    p.replace(&a_projection(ws, doc, b"compatible"), 1, None)
        .unwrap();
    p.mark_schema_too_new(
        ws,
        doc,
        b"future",
        koru_core::schema::SCHEMA_VERSION + 1,
        koru_core::schema::SCHEMA_VERSION,
        2,
    )
    .unwrap();

    assert_eq!(
        p.read(doc).unwrap().unwrap().source_revision,
        b"compatible",
        "the last projection this build understood was overwritten"
    );
    assert!(p.revisions().unwrap().get(&doc).is_none());
    assert_eq!(p.staleness(doc, b"future").unwrap(), Staleness::Current);
    assert_eq!(p.staleness(doc, b"another").unwrap(), Staleness::Stale);
    assert!(matches!(
        p.source_status(doc).unwrap(),
        Some(ProjectionSourceStatus::SchemaTooNew { source_revision, .. })
            if source_revision == b"future"
    ));
}

#[test]
fn a_successful_projection_clears_the_incompatible_status() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    p.mark_schema_too_new(ws, doc, b"future", 9, 3, 1).unwrap();

    p.replace(&a_projection(ws, doc, b"supported"), 2, None)
        .unwrap();

    assert_eq!(p.source_status(doc).unwrap(), None);
    assert_eq!(p.revisions().unwrap()[&doc], b"supported");
}

/// A consumer reconciles by comparing this against its own record. Without it
/// there is no way to catch up except by being told, which is the thing this
/// layer refuses to rely on.
#[test]
fn every_projected_documents_revision_is_enumerable() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let (one, two) = (DocumentId::new(), DocumentId::new());
    p.replace(&a_projection(ws, one, b"a"), 1, None).unwrap();
    p.replace(&a_projection(ws, two, b"b"), 1, None).unwrap();

    let revisions = p.revisions().unwrap();
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[&one], b"a");
    assert_eq!(revisions[&two], b"b");
}

/// Dropping is not binning. This is the removal for a document the store no
/// longer holds at all.
#[test]
fn removing_a_document_takes_all_of_its_rows() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    p.replace(&a_projection(ws, doc, b"a"), 1, None).unwrap();

    p.remove(doc).unwrap();

    assert!(p.read(doc).unwrap().is_none());
    assert_eq!(p.documents().unwrap(), 0);
    assert!(p.revisions().unwrap().is_empty());
}

#[test]
fn an_unprojected_document_reads_as_nothing() {
    let p = Projections::in_memory().unwrap();
    assert!(p.read(DocumentId::new()).unwrap().is_none());
}

/// **A schema bump is a rebuild.** The projection is derived, so throwing it
/// away is both simpler than a migration and strictly safer — and it is why
/// nothing here has a migration engine to get wrong.
#[test]
fn a_projection_from_another_schema_is_discarded_rather_than_migrated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("projection.db");
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    {
        let mut p = Projections::at(&path).unwrap();
        p.replace(&a_projection(ws, doc, b"a"), 1, None).unwrap();
        assert_eq!(p.documents().unwrap(), 1);
    }

    // A build from another version of this crate.
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.pragma_update(None, "user_version", PROJECTION_SCHEMA_VERSION + 1)
            .unwrap();
    }

    let p = Projections::at(&path).unwrap();
    assert_eq!(
        p.documents().unwrap(),
        0,
        "the old projection was migrated instead of discarded"
    );
    assert!(p.read(doc).unwrap().is_none());
}

/// Reopening at the same schema keeps everything — the discard must be a
/// response to a *change*, not something a restart does.
#[test]
fn reopening_at_the_same_schema_keeps_what_was_there() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("projection.db");
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    {
        let mut p = Projections::at(&path).unwrap();
        p.replace(&a_projection(ws, doc, b"a"), 1, None).unwrap();
    }
    let p = Projections::at(&path).unwrap();
    assert_eq!(p.documents().unwrap(), 1);
    assert_eq!(p.read(doc).unwrap().unwrap().source_revision, b"a");
}

/// Projection schema 3 was used by two different table shapes. In the older
/// one `listing` had no MIME column, so trusting only `user_version` made every
/// listing update fail forever.
#[test]
fn a_projection_stamped_with_the_colliding_v3_shape_is_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("projection.db");
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE listing (
                 document_id TEXT PRIMARY KEY,
                 workspace_id TEXT NOT NULL,
                 parent_id TEXT,
                 ordinal INTEGER NOT NULL,
                 title TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 trashed_at INTEGER,
                 trashed_by TEXT
             );
             PRAGMA user_version = 3;",
        )
        .unwrap();
    }

    let p = Projections::at(&path).unwrap();
    let version: i64 =
        p.db.pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
    let has_mime: bool =
        p.db.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM pragma_table_info('listing') WHERE name = 'mime'
             )",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(version, PROJECTION_SCHEMA_VERSION);
    assert!(has_mime);
}

#[test]
fn state_survives_a_reopen_and_can_be_cleared() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("projection.db");
    {
        let mut p = Projections::at(&path).unwrap();
        p.set_state("cursor", "abc").unwrap();
        p.set_state("cursor", "def").unwrap();
    }
    let mut p = Projections::at(&path).unwrap();
    assert_eq!(p.state("cursor").unwrap().as_deref(), Some("def"));
    p.clear_state("cursor").unwrap();
    assert!(p.state("cursor").unwrap().is_none());
}

// -------------------------------------------------------------------- listing

fn a_listed(document: DocumentId, title: &str) -> ListedDocument {
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

#[test]
fn a_listing_comes_back_as_it_went_in() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let (parent, child) = (DocumentId::new(), DocumentId::new());
    let mut nested = a_listed(child, "Notes");
    nested.parent = Some(parent);
    nested.ordinal = 1;
    nested.trashed_at = Some(42);
    nested.trashed_by = Some("Steven".into());

    assert!(
        p.set_listing(ws, "Acme", &[a_listed(parent, "Roadmap"), nested], 7)
            .unwrap()
    );

    let listing = p.listing(ws).unwrap().expect("a listing");
    assert_eq!(listing.name, "Acme");
    assert_eq!(listing.documents.len(), 2);
    let child = listing
        .documents
        .iter()
        .find(|d| d.document == child)
        .unwrap();
    assert_eq!(child.parent, Some(parent));
    assert_eq!(child.trashed_at, Some(42));
    assert_eq!(child.trashed_by.as_deref(), Some("Steven"));
}

#[test]
fn live_file_blob_hashes_follow_the_live_listing_tree_and_are_deduplicated() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let (live, child, trashed, inherited) = (
        DocumentId::new(),
        DocumentId::new(),
        DocumentId::new(),
        DocumentId::new(),
    );

    for (document, hash) in [
        (live, "blake3:embedded-in-page"),
        (child, "blake3:file"),
        (trashed, "blake3:trashed"),
        (inherited, "blake3:inherited"),
    ] {
        let mut projection = a_projection(ws, document, b"revision");
        projection.nodes[0].blob = Some(hash.to_owned());
        p.replace(&projection, 1, None).unwrap();
    }

    let mut child_entry = a_listed(child, "Child");
    child_entry.parent = Some(live);
    child_entry.kind = "file".into();
    let mut trashed_entry = a_listed(trashed, "Trash");
    trashed_entry.kind = "file".into();
    trashed_entry.trashed_at = Some(42);
    let mut inherited_entry = a_listed(inherited, "Inherited trash");
    inherited_entry.parent = Some(trashed);
    inherited_entry.kind = "file".into();
    p.set_listing(
        ws,
        "Acme",
        &[
            a_listed(live, "Live"),
            child_entry,
            trashed_entry,
            inherited_entry,
        ],
        1,
    )
    .unwrap();

    assert_eq!(p.live_file_blob_hashes(ws).unwrap(), ["blake3:file"]);

    p.set_listing(ws, "Acme", &[], 2).unwrap();
    assert!(p.live_file_blob_hashes(ws).unwrap().is_empty());
}

/// **The transfer's question is not the sweep's**, and this is the test that
/// keeps the two from being answered by one query.
///
/// `live_file_blob_hashes` drops a binned document and an image pasted into an
/// ordinary page; the approved usage corpus counts both (cases 12 and 13), and
/// `12-blob-referenced-only-from-trash.json` says accounting must not reuse
/// that query. The same four documents go in, and the two answers differ by
/// exactly the three references the sweep is right to drop and a bill and a
/// transfer are wrong to.
#[test]
fn blob_references_keep_what_the_sweep_query_is_right_to_drop() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let (page, file, trashed, inherited) = (
        DocumentId::new(),
        DocumentId::new(),
        DocumentId::new(),
        DocumentId::new(),
    );

    for (document, hash) in [
        (page, "b3:aa"),
        (file, "b3:bb"),
        (trashed, "b3:cc"),
        (inherited, "b3:dd"),
    ] {
        let mut projection = a_projection(ws, document, b"revision");
        projection.nodes[0].blob = Some(hash.to_owned());
        projection.nodes[0].mime = Some("image/png".into());
        p.replace(&projection, 1, None).unwrap();
    }

    let mut file_entry = a_listed(file, "Child");
    file_entry.parent = Some(page);
    file_entry.kind = "file".into();
    let mut trashed_entry = a_listed(trashed, "Trash");
    trashed_entry.kind = "file".into();
    trashed_entry.trashed_at = Some(42);
    let mut inherited_entry = a_listed(inherited, "Inherited trash");
    inherited_entry.parent = Some(trashed);
    inherited_entry.kind = "file".into();
    p.set_listing(
        ws,
        "Acme",
        &[
            a_listed(page, "Live"),
            file_entry,
            trashed_entry,
            inherited_entry,
        ],
        1,
    )
    .unwrap();

    // The sweep's answer: one hash, from the one live first-class file entry.
    assert_eq!(p.live_file_blob_hashes(ws).unwrap(), ["b3:bb"]);

    // The transfer's and the bill's: all four, because a page holds files too
    // and a stamp on a manifest entry is not a deletion.
    let mut rows = p.blob_references(ws).unwrap();
    rows.sort_by(|left, right| left.blob.cmp(&right.blob));
    assert_eq!(
        rows.iter().map(|row| row.blob.as_str()).collect::<Vec<_>>(),
        ["b3:aa", "b3:bb", "b3:cc", "b3:dd"]
    );
    assert!(
        rows.iter()
            .all(|row| row.mime.as_deref() == Some("image/png"))
    );
    // The Trash stamp is carried rather than filtered, so a status surface can
    // explain a number instead of only printing one.
    assert_eq!(
        rows.iter().filter(|row| row.trashed).count(),
        1,
        "only the document with its own stamp is marked; inheritance is the walk's"
    );

    // A document the manifest no longer names has no listing row, so it
    // contributes nothing without a predicate anybody could forget to write.
    p.set_listing(ws, "Acme", &[], 2).unwrap();
    assert!(p.blob_references(ws).unwrap().is_empty());
}

/// **The sample is one read, and it names the state each document's references
/// came from.**
///
/// Cloud coverage is a claim about a Workspace at a moment. A reference set
/// taken from one read and a per-document version taken from another is two
/// moments wearing one answer, so the two arrive together — and a document the
/// listing names but nothing has projected comes back with **no** state rather
/// than with an empty one, because *unknown references* and *no references* are
/// not the same fact.
#[test]
fn the_blob_reference_sample_carries_each_document_state_beside_its_references() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let (with_picture, plain, unread) = (DocumentId::new(), DocumentId::new(), DocumentId::new());

    let mut projection = a_projection(ws, with_picture, b"revision-one");
    projection.nodes[0].blob = Some("b3:aa".to_owned());
    projection.nodes[0].mime = Some("image/png".into());
    p.replace(&projection, 1, None).unwrap();
    p.replace(&a_projection(ws, plain, b"revision-two"), 1, None)
        .unwrap();

    let mut binned = a_listed(with_picture, "Has a picture");
    binned.trashed_at = Some(42);
    p.set_listing(
        ws,
        "Acme",
        &[
            binned,
            a_listed(plain, "No picture"),
            a_listed(unread, "Never projected"),
        ],
        1,
    )
    .unwrap();

    let sample = p.blob_reference_sample(ws).unwrap();
    assert_eq!(
        sample.len(),
        3,
        "a listed document was left out of the sample"
    );

    let picture = sample
        .iter()
        .find(|row| row.document == with_picture.to_storage())
        .expect("the document with a picture");
    assert_eq!(picture.state.as_deref(), Some(&b"revision-one"[..]));
    assert_eq!(
        picture.references,
        vec![("b3:aa".to_owned(), Some("image/png".to_owned()))]
    );
    // A binned document still holds its picture: the stamp is carried, never
    // used here to drop a reference.
    assert!(picture.trashed);

    let plain_row = sample
        .iter()
        .find(|row| row.document == plain.to_storage())
        .expect("the document with no picture");
    assert_eq!(plain_row.state.as_deref(), Some(&b"revision-two"[..]));
    assert!(plain_row.references.is_empty());
    assert!(!plain_row.trashed);

    // **`None`, not empty.** Its references are unknown, and a caller that read
    // this as "no pictures" would report a Workspace complete while one of its
    // documents had never been looked at.
    let unread_row = sample
        .iter()
        .find(|row| row.document == unread.to_storage())
        .expect("the document nothing has projected");
    assert_eq!(unread_row.state, None);
    assert!(unread_row.references.is_empty());

    // The walk's own rule still holds over the sample: a document the manifest
    // no longer names contributes nothing at all.
    p.set_listing(ws, "Acme", &[], 2).unwrap();
    assert!(p.blob_reference_sample(ws).unwrap().is_empty());
}

/// **The digest is what a consumer reconciles against**, so it has to move for
/// anything a consumer can see and stay put for everything else. Writing the
/// same listing twice must not look like a change.
#[test]
fn the_digest_moves_for_a_rename_and_not_for_a_rewrite() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let doc = DocumentId::new();

    assert!(
        p.set_listing(ws, "Acme", &[a_listed(doc, "Roadmap")], 1)
            .unwrap()
    );
    assert!(
        !p.set_listing(ws, "Acme", &[a_listed(doc, "Roadmap")], 2)
            .unwrap(),
        "an identical listing was published as a change"
    );
    assert!(
        p.set_listing(ws, "Acme", &[a_listed(doc, "Plans")], 3)
            .unwrap()
    );
    assert!(
        p.set_listing(ws, "Acme Ltd", &[a_listed(doc, "Plans")], 4)
            .unwrap(),
        "the workspace's own name is part of what a consumer joins against"
    );
}

/// A rename is one row. That is the whole reason the listing may live here
/// while the same title on every node row may not.
#[test]
fn a_rename_leaves_every_node_row_alone() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    p.replace(&a_projection(ws, doc, b"a"), 1, None).unwrap();
    p.set_listing(ws, "Acme", &[a_listed(doc, "Roadmap")], 1)
        .unwrap();

    p.set_listing(ws, "Acme", &[a_listed(doc, "Plans")], 2)
        .unwrap();

    let projection = p.read(doc).unwrap().unwrap();
    assert_eq!(projection.source_revision, b"a");
    assert_eq!(projection.nodes.len(), 2);
}

/// A dropped document loses its listing row with the rest of it — a consumer
/// joining against a listing that still named it would draw a result nobody can
/// open.
#[test]
fn removing_a_document_takes_its_listing_row_too() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    p.replace(&a_projection(ws, doc, b"a"), 1, None).unwrap();
    p.set_listing(ws, "Acme", &[a_listed(doc, "Roadmap")], 1)
        .unwrap();

    p.remove(doc).unwrap();

    assert!(p.listing(ws).unwrap().unwrap().documents.is_empty());
}

// ------------------------------------------------------------------- headings

/// A heading node's `kind` is `paragraph` — the level is a block property, not
/// a kind — so this builds one the way the projector really writes it.
fn a_heading(id: &str, text: &str, level: u8) -> ProjectedNode {
    let mut node = a_node(id, text);
    node.heading = Some(level);
    node
}

#[test]
fn headings_are_returned_in_reading_order() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());

    let mut projection = a_projection(ws, doc, b"rev-1");
    projection.nodes = vec![
        a_heading("n1", "Overview", 1),
        a_node("n2", "body text"),
        a_heading("n3", "Rollout", 2),
        a_heading("n4", "Rollback", 2),
    ];
    p.replace(&projection, 1, None).unwrap();

    let headings = p.headings(doc).unwrap();
    assert_eq!(
        headings
            .iter()
            .map(|h| (h.node.as_str(), h.level))
            .collect::<Vec<_>>(),
        vec![("n1", 1), ("n3", 2), ("n4", 2)],
    );
    assert!(headings.windows(2).all(|w| w[0].seq < w[1].seq));
}

/// The load-bearing one. A heading is `NodeMeta::heading`, and the node's kind
/// still reads `paragraph`, so a query that filtered on kind would answer
/// nothing at all while looking perfectly reasonable.
#[test]
fn headings_exclude_body_paragraphs() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());

    let mut projection = a_projection(ws, doc, b"rev-1");
    projection.nodes = vec![a_node("n1", "body text"), a_heading("n2", "Setup", 3)];
    p.replace(&projection, 1, None).unwrap();

    let headings = p.headings(doc).unwrap();
    assert_eq!(headings.len(), 1);
    assert_eq!(headings[0].text, "Setup");
    assert_eq!(headings[0].level, 3);
}

/// The debounce window has to be answerable. A document nobody has projected
/// yet is an empty answer, never an error — the caller distinguishes "not
/// indexed" from "no headings" by other means.
#[test]
fn headings_of_an_unprojected_document_are_empty_not_an_error() {
    let p = Projections::in_memory().unwrap();
    assert!(p.headings(DocumentId::new()).unwrap().is_empty());
}

#[test]
fn headings_are_scoped_to_their_document() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let (one, two) = (DocumentId::new(), DocumentId::new());

    let mut first = a_projection(ws, one, b"rev-1");
    first.nodes = vec![a_heading("n1", "Mine", 1)];
    p.replace(&first, 1, None).unwrap();
    let mut second = a_projection(ws, two, b"rev-1");
    second.nodes = vec![a_heading("n1", "Theirs", 1)];
    p.replace(&second, 1, None).unwrap();

    assert_eq!(p.headings(one).unwrap()[0].text, "Mine");
    assert_eq!(p.headings(two).unwrap()[0].text, "Theirs");
}

/// A level this build cannot draw is still a deliberate heading. Clamping in
/// Rust rather than in SQL is what keeps a corrupt row from panicking a cast.
#[test]
fn an_out_of_range_heading_level_is_clamped_rather_than_dropped() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());

    let mut projection = a_projection(ws, doc, b"rev-1");
    projection.nodes = vec![a_heading("n1", "Deep", 9)];
    p.replace(&projection, 1, None).unwrap();

    assert_eq!(p.headings(doc).unwrap()[0].level, 6);
}

// ------------------------------------------------------- attribute queries

use koru_core::attributes::Priority;

fn an_attr(id: &str, role: &str, kind: &str, value: &str) -> crate::model::ProjectedAttr {
    crate::model::ProjectedAttr {
        id: id.to_owned(),
        role: role.to_owned(),
        kind: kind.to_owned(),
        value: value.to_owned(),
        zone: None,
        label: None,
    }
}

/// A zoned due — the instant, and the zone beside it. The zone is not
/// optional on this kind; a row without one is not a value ([ADR-183]).
///
/// [ADR-183]: ../../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
fn a_zoned_attr(id: &str, role: &str, instant_ms: i64, zone: &str) -> crate::model::ProjectedAttr {
    crate::model::ProjectedAttr {
        zone: Some(zone.to_owned()),
        ..an_attr(
            id,
            role,
            koru_core::time::TemporalKind::Zoned.token(),
            &instant_ms.to_string(),
        )
    }
}

fn attributed(id: &str, text: &str, attrs: Vec<crate::model::ProjectedAttr>) -> ProjectedNode {
    let mut node = a_node(id, text);
    node.attrs = attrs;
    node
}

/// One workspace, one listed document, whichever nodes the test wants.
fn seeded(p: &mut Projections, ws: WorkspaceId, doc: DocumentId, nodes: Vec<ProjectedNode>) {
    let mut projection = a_projection(ws, doc, b"rev-1");
    projection.nodes = nodes;
    p.replace(&projection, 1, None).unwrap();
    p.set_listing(ws, "Acme", &[a_listed(doc, "Tasks")], 1)
        .unwrap();
}

fn reminder_target(ws: WorkspaceId, doc: DocumentId, node: &str) -> EntityRef {
    EntityRef::new("node", ws, doc, uuid::Uuid::parse_str(node).unwrap()).unwrap()
}

#[test]
fn reminder_target_join_is_batched_beyond_sqlites_variable_limit() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let node = "11111111-1111-4111-8111-111111111111";
    seeded(&mut p, ws, doc, vec![a_node(node, "Target")]);
    let target = reminder_target(ws, doc, node);
    let targets = vec![target.clone(); 501];

    let rows = p.reminder_targets(&targets).unwrap();
    assert_eq!(rows.len(), 501);
    assert!(rows.iter().all(|row| row.target == target && row.projected));
}

#[test]
fn reminder_target_due_uses_deterministic_attribute_identity_order() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let node = "22222222-2222-4222-8222-222222222222";
    seeded(
        &mut p,
        ws,
        doc,
        vec![attributed(
            node,
            "Target",
            vec![
                an_attr("z-due", "koru.due", "date", "2026-09-09"),
                an_attr("a-due", "koru.due", "date", "2026-09-01"),
            ],
        )],
    );

    let row = p
        .reminder_targets(&[reminder_target(ws, doc, node)])
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        row.due,
        Some(TemporalValue::Date {
            date: koru_store_core::CalendarDate::parse("2026-09-01").unwrap()
        })
    );
}

#[test]
fn reminder_target_requires_projection_and_current_listing_before_claiming_current() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let node = "33333333-3333-4333-8333-333333333333";
    let mut projection = a_projection(ws, doc, b"rev-1");
    projection.nodes = vec![a_node(node, "Target")];
    p.replace(&projection, 1, None).unwrap();

    let row = p
        .reminder_targets(&[reminder_target(ws, doc, node)])
        .unwrap()
        .pop()
        .unwrap();
    assert!(!row.projected);
    assert_eq!(row.state, TargetState::Missing);

    p.set_listing(ws, "Acme", &[a_listed(doc, "Tasks")], 2)
        .unwrap();
    let current = p
        .reminder_targets(&[reminder_target(ws, doc, node)])
        .unwrap()
        .pop()
        .unwrap();
    assert!(current.projected);
    assert_eq!(current.state, TargetState::Active);
    assert_eq!(current.source_location, "Acme / Tasks");
}

#[test]
fn reminder_source_location_is_root_first_for_nested_and_document_targets() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let (parent, child) = (DocumentId::new(), DocumentId::new());
    let nested_id = "44444444-4444-4444-8444-444444444444";
    let mut projection = a_projection(ws, child, b"rev-1");
    projection.nodes = vec![
        a_node(&child.to_storage(), "Notes"),
        a_node(nested_id, "Call supplier"),
    ];
    p.replace(&projection, 1, None).unwrap();
    let mut listed_child = a_listed(child, "Notes");
    listed_child.parent = Some(parent);
    p.set_listing(ws, "Acme", &[a_listed(parent, "Projects"), listed_child], 1)
        .unwrap();

    let nested = reminder_target(ws, child, nested_id);
    let document = EntityRef::new("document", ws, child, child.as_uuid()).unwrap();
    let rows = p.reminder_targets(&[nested, document]).unwrap();
    assert_eq!(rows[0].source_location, "Acme / Projects / Notes");
    assert_eq!(rows[1].source_location, "Acme / Projects");
}

#[test]
fn bin_and_restore_change_effective_target_state_without_removing_due() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let node = "44444444-4444-4444-8444-444444444444";
    seeded(
        &mut p,
        ws,
        doc,
        vec![attributed(
            node,
            "Target",
            vec![an_attr("due", "koru.due", "date", "2026-09-01")],
        )],
    );
    let target = reminder_target(ws, doc, node);
    let mut binned = a_listed(doc, "Tasks");
    binned.trashed_at = Some(3);
    p.set_listing(ws, "Acme", &[binned], 3).unwrap();
    let in_bin = p.reminder_targets(std::slice::from_ref(&target)).unwrap();
    assert_eq!(in_bin[0].state, TargetState::InBin);
    assert!(in_bin[0].due.is_some());

    p.set_listing(ws, "Acme", &[a_listed(doc, "Tasks")], 4)
        .unwrap();
    let restored = p.reminder_targets(&[target]).unwrap();
    assert_eq!(restored[0].state, TargetState::Active);
    assert!(restored[0].due.is_some());
}

fn query() -> AttributeRowQuery {
    AttributeRowQuery {
        limit: 100,
        ..AttributeRowQuery::default()
    }
}

#[test]
fn a_row_carries_workspace_document_and_node_together_with_its_attrs() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            attributed(
                "n1",
                "ship it",
                vec![an_attr("a1", "koru.due", "date", "2026-09-01")],
            ),
            a_node("n2", "no attributes here"),
        ],
    );

    let rows = p
        .attribute_rows(&AttributeRowQuery {
            has: Some("koru.due".into()),
            ..query()
        })
        .unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!((row.workspace, row.document), (ws, doc));
    assert_eq!(row.node, "n1");
    assert_eq!(row.node_kind, "paragraph");
    assert_eq!(row.excerpt, "ship it");
    assert_eq!(row.title, "Tasks");
    assert!(!row.trashed);
    assert_eq!(row.attrs.len(), 1);
    assert_eq!(row.attrs[0].kind, "date");
    assert_eq!(row.attrs[0].value, "2026-09-01");
}

#[test]
fn a_node_with_no_attributes_is_never_a_row_even_under_missing() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            attributed(
                "tagged",
                "has a tag, no due",
                vec![an_attr("a1", "koru.tag", "text", "some-tag-id")],
            ),
            a_node("bare", "a plain paragraph"),
        ],
    );

    let rows = p
        .attribute_rows(&AttributeRowQuery {
            missing: Some("koru.due".into()),
            ..query()
        })
        .unwrap();
    assert_eq!(rows.len(), 1, "the bare paragraph is out of scope");
    assert_eq!(rows[0].node, "tagged");
}

#[test]
fn tag_filters_match_any_of_the_named_ids() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            attributed(
                "n1",
                "one",
                vec![an_attr("a1", "koru.tag", "text", "tag-a")],
            ),
            attributed(
                "n2",
                "two",
                vec![an_attr("a2", "koru.tag", "text", "tag-b")],
            ),
            attributed(
                "n3",
                "three",
                vec![an_attr("a3", "koru.tag", "text", "tag-c")],
            ),
        ],
    );

    let rows = p
        .attribute_rows(&AttributeRowQuery {
            tags: vec!["tag-a".into(), "tag-c".into()],
            ..query()
        })
        .unwrap();
    let nodes: Vec<&str> = rows.iter().map(|r| r.node.as_str()).collect();
    assert_eq!(nodes, ["n1", "n3"]);
}

#[test]
fn minimum_priority_is_the_semantic_order_not_the_alphabetic_one() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            attributed(
                "lo",
                "low",
                vec![an_attr("a1", "koru.priority", "text", "low")],
            ),
            attributed(
                "md",
                "medium",
                vec![an_attr("a2", "koru.priority", "text", "medium")],
            ),
            attributed(
                "hi",
                "high",
                vec![an_attr("a3", "koru.priority", "text", "high")],
            ),
            attributed(
                "ur",
                "urgent",
                vec![an_attr("a4", "koru.priority", "text", "urgent")],
            ),
        ],
    );

    let rows = p
        .attribute_rows(&AttributeRowQuery {
            min_priority: Some(Priority::High),
            ..query()
        })
        .unwrap();
    let mut nodes: Vec<&str> = rows.iter().map(|r| r.node.as_str()).collect();
    nodes.sort_unstable();
    // Alphabetically "medium" sorts above "high"; semantically it is below
    // the floor and stays out.
    assert_eq!(nodes, ["hi", "ur"]);

    let exact = p
        .attribute_rows(&AttributeRowQuery {
            priority: Some(Priority::Medium),
            ..query()
        })
        .unwrap();
    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0].node, "md");
}

/// **The due window states what it means, per temporal type** ([ADR-183],
/// rule 5). A calendar window answers for calendar days; an instant window
/// answers for zoned deadlines; neither is folded onto the other, and a row of
/// a type the filter did not name simply does not match.
///
/// The shape this replaced folded a date bound onto instants at the UTC day
/// boundary, which is the bug: `afternoon` below is 15:00Z on Sep 2, which is
/// already **Sep 3** in Tokyo — so "everything due on Sep 2" answered by UTC
/// arithmetic is a different question from the one a reader in Tokyo asked,
/// and the only honest fix is to make the caller say which.
///
/// [ADR-183]: ../../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
#[test]
fn a_due_window_answers_each_temporal_type_in_its_own_terms() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    // 2026-09-02T15:00:00Z.
    let afternoon = (super::days_from_civil(2026, 9, 2) * 86_400_000) + 15 * 3_600_000;
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            attributed(
                "d1",
                "sep 1",
                vec![an_attr("a1", "koru.due", "date", "2026-09-01")],
            ),
            attributed(
                "d2",
                "sep 2",
                vec![an_attr("a2", "koru.due", "date", "2026-09-02")],
            ),
            attributed(
                "t2",
                "sep 2 afternoon in Tokyo",
                vec![a_zoned_attr("a3", "koru.due", afternoon, "Asia/Tokyo")],
            ),
            attributed(
                "f2",
                "sep 2 wall clock",
                vec![an_attr(
                    "a5",
                    "koru.due",
                    "datetime_floating",
                    "2026-09-02T09:00:00",
                )],
            ),
            attributed(
                "d9",
                "sep 9",
                vec![an_attr("a4", "koru.due", "date", "2026-09-09")],
            ),
        ],
    );

    // A calendar window answers for the two types that have a day without
    // being told where they are read: the date, and the floating wall clock.
    // The zoned instant is **not** silently folded in.
    let rows = p
        .attribute_rows(&AttributeRowQuery {
            due: Some(DueFilter {
                dates: Some(DateWindow {
                    after: Some("2026-09-02".into()),
                    before: Some("2026-09-03".into()),
                }),
                instants: None,
            }),
            ..query()
        })
        .unwrap();
    let mut nodes: Vec<&str> = rows.iter().map(|r| r.node.as_str()).collect();
    nodes.sort_unstable();
    assert_eq!(nodes, ["d2", "f2"]);

    // An instant window answers for the zoned deadline only, and exactly —
    // 15:00 is not before 15:00.
    let rows = p
        .attribute_rows(&AttributeRowQuery {
            due: Some(DueFilter {
                dates: None,
                instants: Some(InstantWindow {
                    after_ms: None,
                    before_ms: Some(afternoon + 1),
                }),
            }),
            ..query()
        })
        .unwrap();
    assert_eq!(
        rows.iter().map(|r| r.node.as_str()).collect::<Vec<_>>(),
        ["t2"],
        "no calendar row is folded onto an instant window"
    );
    let rows = p
        .attribute_rows(&AttributeRowQuery {
            due: Some(DueFilter {
                dates: None,
                instants: Some(InstantWindow {
                    after_ms: None,
                    before_ms: Some(afternoon),
                }),
            }),
            ..query()
        })
        .unwrap();
    assert!(rows.is_empty(), "the upper bound is exclusive");

    // "This day, however it was set" is two questions, and a caller asks both.
    let rows = p
        .attribute_rows(&AttributeRowQuery {
            due: Some(DueFilter {
                dates: Some(DateWindow {
                    after: Some("2026-09-02".into()),
                    before: Some("2026-09-03".into()),
                }),
                instants: Some(InstantWindow {
                    after_ms: Some(afternoon),
                    before_ms: Some(afternoon + 1),
                }),
            }),
            ..query()
        })
        .unwrap();
    let mut nodes: Vec<&str> = rows.iter().map(|r| r.node.as_str()).collect();
    nodes.sort_unstable();
    assert_eq!(nodes, ["d2", "f2", "t2"]);
}

/// A filter naming no window at all is a question nobody asked. Answering
/// "everything" would be a guess, so it is refused.
#[test]
fn a_due_filter_with_no_window_is_refused() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    seeded(
        &mut p,
        ws,
        doc,
        vec![attributed(
            "d1",
            "sep 1",
            vec![an_attr("a1", "koru.due", "date", "2026-09-01")],
        )],
    );
    assert!(
        p.attribute_rows(&AttributeRowQuery {
            due: Some(DueFilter::default()),
            ..query()
        })
        .is_err()
    );
}

/// The zone rides with the instant out of the read model. A consumer handed an
/// epoch-ms number and nothing else would be holding exactly the ambiguous
/// shape the three types abolish.
#[test]
fn a_zoned_due_keeps_its_zone_through_the_projection() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    seeded(
        &mut p,
        ws,
        doc,
        vec![attributed(
            "t1",
            "deadline",
            vec![a_zoned_attr(
                "a1",
                "koru.due",
                1_790_000_000_000,
                "Europe/Zurich",
            )],
        )],
    );
    let rows = p.attribute_rows(&query()).unwrap();
    assert_eq!(rows.len(), 1);
    let due = &rows[0].attrs[0];
    assert_eq!(due.kind, "datetime_zoned");
    assert_eq!(due.value, "1790000000000");
    assert_eq!(due.zone.as_deref(), Some("Europe/Zurich"));
}

/// **Sorting folds; filtering does not.** A sorted page has to put every row
/// somewhere, so the three temporal types collapse onto one number here — and
/// that is legitimate precisely because an ordering is not an answer to a
/// question ([ADR-183]; the filter beside it compares each type in its own
/// terms).
///
/// [ADR-183]: ../../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
#[test]
fn due_sort_folds_every_temporal_type_and_puts_the_undated_last() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let sep2_noon = (super::days_from_civil(2026, 9, 2) * 86_400_000) + 12 * 3_600_000;
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            attributed(
                "later",
                "sep 9",
                vec![an_attr("a1", "koru.due", "date", "2026-09-09")],
            ),
            attributed(
                "none",
                "priority only",
                vec![an_attr("a2", "koru.priority", "text", "high")],
            ),
            attributed(
                "soon",
                "sep 2 noon, zoned",
                vec![a_zoned_attr("a3", "koru.due", sep2_noon, "Europe/Zurich")],
            ),
            attributed(
                "middle",
                "sep 3 wall clock",
                vec![an_attr(
                    "a4",
                    "koru.due",
                    "datetime_floating",
                    "2026-09-03T08:00:00",
                )],
            ),
        ],
    );

    let rows = p
        .attribute_rows(&AttributeRowQuery {
            sort: AttrSort::Due,
            ..query()
        })
        .unwrap();
    let nodes: Vec<&str> = rows.iter().map(|r| r.node.as_str()).collect();
    assert_eq!(nodes, ["soon", "middle", "later", "none"]);
}

#[test]
fn priority_sort_is_most_important_first_with_unset_last() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            attributed(
                "md",
                "medium",
                vec![an_attr("a1", "koru.priority", "text", "medium")],
            ),
            attributed("un", "unset", vec![an_attr("a2", "koru.tag", "text", "t")]),
            attributed(
                "ur",
                "urgent",
                vec![an_attr("a3", "koru.priority", "text", "urgent")],
            ),
        ],
    );

    let rows = p
        .attribute_rows(&AttributeRowQuery {
            sort: AttrSort::Priority,
            ..query()
        })
        .unwrap();
    let nodes: Vec<&str> = rows.iter().map(|r| r.node.as_str()).collect();
    assert_eq!(nodes, ["ur", "md", "un"]);
}

#[test]
fn paging_is_stable_and_bounded() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    seeded(
        &mut p,
        ws,
        doc,
        (0..5)
            .map(|i| {
                attributed(
                    &format!("n{i}"),
                    "row",
                    vec![an_attr(&format!("a{i}"), "koru.tag", "text", "t")],
                )
            })
            .collect(),
    );

    let one = |offset| {
        p.attribute_rows(&AttributeRowQuery {
            limit: 2,
            offset,
            ..query()
        })
        .unwrap()
    };
    let (first, second, third) = (one(0), one(2), one(4));
    assert_eq!(first.len(), 2);
    assert_eq!(second.len(), 2);
    assert_eq!(third.len(), 1);
    let mut all: Vec<String> = first
        .iter()
        .chain(&second)
        .chain(&third)
        .map(|r| r.node.clone())
        .collect();
    all.sort_unstable();
    all.dedup();
    assert_eq!(all.len(), 5, "no row is repeated or skipped across pages");
}

/// Trash is inherited down the listing tree, and trashed rows are **flagged,
/// never hidden** — restore has to be able to find them.
#[test]
fn a_child_of_a_binned_parent_is_flagged_without_a_stamp_of_its_own() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let (parent, child, live) = (DocumentId::new(), DocumentId::new(), DocumentId::new());

    for (doc, node) in [(parent, "p"), (child, "c"), (live, "l")] {
        let mut projection = a_projection(ws, doc, b"rev-1");
        projection.nodes = vec![attributed(
            node,
            "row",
            vec![an_attr("a1", "koru.tag", "text", "t")],
        )];
        p.replace(&projection, 1, None).unwrap();
    }
    let mut binned = a_listed(parent, "Binned");
    binned.trashed_at = Some(9);
    let mut nested = a_listed(child, "Nested");
    nested.parent = Some(parent);
    p.set_listing(ws, "Acme", &[binned, nested, a_listed(live, "Live")], 1)
        .unwrap();

    let rows = p.attribute_rows(&query()).unwrap();
    assert_eq!(rows.len(), 3, "trashed rows are flagged, not hidden");
    let flag = |node: &str| rows.iter().find(|r| r.node == node).unwrap().trashed;
    assert!(flag("p"));
    assert!(
        flag("c"),
        "the child carries no stamp and inherits the flag"
    );
    assert!(!flag("l"));
}

#[test]
fn the_workspace_scope_separates_two_workspaces() {
    let mut p = Projections::in_memory().unwrap();
    let (ws_a, ws_b) = (WorkspaceId::new(), WorkspaceId::new());
    let (doc_a, doc_b) = (DocumentId::new(), DocumentId::new());
    seeded(
        &mut p,
        ws_a,
        doc_a,
        vec![attributed(
            "a",
            "ours",
            vec![an_attr("a1", "koru.tag", "text", "t")],
        )],
    );
    seeded(
        &mut p,
        ws_b,
        doc_b,
        vec![attributed(
            "b",
            "theirs",
            vec![an_attr("a2", "koru.tag", "text", "t")],
        )],
    );

    let rows = p
        .attribute_rows(&AttributeRowQuery {
            workspaces: vec![ws_a],
            ..query()
        })
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].workspace, ws_a);

    assert_eq!(
        p.attribute_rows(&query()).unwrap().len(),
        2,
        "empty scope is every workspace"
    );
}

#[test]
fn a_malformed_date_bound_is_refused_not_guessed() {
    let p = Projections::in_memory().unwrap();
    for bad in ["2026-9-1", "not a date", "2026-13-01", "20260901"] {
        assert!(
            p.attribute_rows(&AttributeRowQuery {
                due: Some(DueFilter {
                    dates: Some(DateWindow {
                        after: Some(bad.into()),
                        before: None,
                    }),
                    instants: None,
                }),
                ..query()
            })
            .is_err(),
            "{bad} should be refused"
        );
    }
}

#[test]
fn civil_day_arithmetic_matches_known_days() {
    // 1970-01-01 is day zero, and the leap-year boundaries land where they
    // should. The date bound is the one place this arithmetic is load-bearing:
    // it validates the string a caller sent before it reaches SQL.
    assert_eq!(super::days_from_civil(1970, 1, 1), 0);
    assert_eq!(
        super::days_from_civil(2024, 2, 29) + 1,
        super::days_from_civil(2024, 3, 1)
    );
    assert_eq!(
        super::days_from_civil(1999, 12, 31) + 1,
        super::days_from_civil(2000, 1, 1)
    );
}

// ------------------------------------------------------------- modified-at

/// **A re-projection is not a modification.** The projector runs over
/// documents nobody edited — a sweep after a restart, a recovery, a schema
/// rebuild, a `Staleness::Stale` comparison — and `projected_at` moved for
/// every one of them. That is what made the old proxy reorder a recency list
/// nobody had touched.
#[test]
fn re_projecting_the_same_content_does_not_move_modified_at() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());

    p.replace(&a_projection(ws, doc, b"rev-1"), 1_000, None)
        .unwrap();
    assert_eq!(p.modified_at(doc).unwrap(), Some(1_000));

    // Same nodes, a later revision, a later projector run: the source moved
    // (another peer's edit that projected identically, a recovery) but what a
    // consumer can see did not.
    p.replace(&a_projection(ws, doc, b"rev-2"), 2_000, None)
        .unwrap();
    assert_eq!(
        p.modified_at(doc).unwrap(),
        Some(1_000),
        "a projection that reproduced the same rows restamped the document"
    );

    let mut edited = a_projection(ws, doc, b"rev-3");
    edited.nodes[0].text = "changed".to_owned();
    p.replace(&edited, 3_000, None).unwrap();
    assert_eq!(p.modified_at(doc).unwrap(), Some(3_000));
}

/// Every field a consumer can see is in the digest, not only the text: an
/// attribute set on a node whose text did not move is exactly the edit a
/// Tracked view sorts on.
#[test]
fn an_attribute_edit_alone_moves_modified_at() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    p.replace(&a_projection(ws, doc, b"rev-1"), 1_000, None)
        .unwrap();

    let mut with_due = a_projection(ws, doc, b"rev-2");
    with_due.nodes[0].attrs = vec![an_attr("a1", "koru.due", "date", "2026-09-01")];
    p.replace(&with_due, 2_000, None).unwrap();

    assert_eq!(p.modified_at(doc).unwrap(), Some(2_000));
}

/// **The store's durable stamp is the value written, and the projector's clock
/// is only the fallback.** The two are far apart here on purpose: the stamp a
/// consumer sorts on is the moment the *store* wrote the document, not the
/// moment this database happened to be brought level with it.
#[test]
fn the_stores_durable_stamp_wins_over_the_projectors_clock() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());

    // Projected long after the edit — a rebuild, or a workspace swept for the
    // first time since a restart.
    p.replace(&a_projection(ws, doc, b"rev-1"), 900_000, Some(1_000))
        .unwrap();
    assert_eq!(p.modified_at(doc).unwrap(), Some(1_000));

    // The gate is still the digest: same content, a stamp the store moved for a
    // checkpoint rather than an edit, and the row does not move.
    p.replace(&a_projection(ws, doc, b"rev-2"), 950_000, Some(8_000))
        .unwrap();
    assert_eq!(
        p.modified_at(doc).unwrap(),
        Some(1_000),
        "a durable stamp moved by maintenance restamped the document"
    );

    let mut edited = a_projection(ws, doc, b"rev-3");
    edited.nodes[0].text = "changed".to_owned();
    p.replace(&edited, 990_000, Some(9_000)).unwrap();
    assert_eq!(p.modified_at(doc).unwrap(), Some(9_000));
}

/// The sort reads `modified_at`, so a document re-projected without changing
/// does not jump to the top of a list somebody is reading.
#[test]
fn the_modified_sort_follows_the_content_not_the_projector() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let (old, new) = (DocumentId::new(), DocumentId::new());
    for (doc, node, at) in [(old, "o", 1_000), (new, "n", 2_000)] {
        let mut projection = a_projection(ws, doc, b"rev-1");
        projection.nodes = vec![attributed(
            node,
            "row",
            vec![an_attr("a1", "koru.tag", "text", "t")],
        )];
        p.replace(&projection, at, None).unwrap();
    }
    p.set_listing(ws, "Acme", &[a_listed(old, "Old"), a_listed(new, "New")], 1)
        .unwrap();

    let order = |p: &Projections| {
        p.attribute_rows(&AttributeRowQuery {
            sort: AttrSort::Modified,
            ..query()
        })
        .unwrap()
        .into_iter()
        .map(|r| r.node)
        .collect::<Vec<_>>()
    };
    assert_eq!(order(&p), ["n", "o"]);

    // The sweep re-projects the older document from an identical source.
    let mut again = a_projection(ws, old, b"rev-2");
    again.nodes = vec![attributed(
        "o",
        "row",
        vec![an_attr("a1", "koru.tag", "text", "t")],
    )];
    p.replace(&again, 9_000, None).unwrap();

    assert_eq!(
        order(&p),
        ["n", "o"],
        "a sweep reordered a list nobody had edited"
    );
}

// ------------------------------------------------------------- group by tag

/// Several documents in one workspace, each with its own nodes, listed
/// together — the group query counts a corpus, so a fixture of one document
/// cannot show what it is for.
fn seeded_many(
    p: &mut Projections,
    ws: WorkspaceId,
    documents: &[(DocumentId, &str, Vec<ProjectedNode>)],
) {
    let mut listed = Vec::new();
    for (doc, title, nodes) in documents {
        let mut projection = a_projection(ws, *doc, b"rev-1");
        projection.nodes = nodes.clone();
        p.replace(&projection, 1, None).unwrap();
        listed.push(a_listed(*doc, title));
    }
    p.set_listing(ws, "Acme", &listed, 1).unwrap();
}

fn tag_rows(id: &str, text: &str, tags: &[&str]) -> ProjectedNode {
    attributed(
        id,
        text,
        tags.iter()
            .enumerate()
            .map(|(i, tag)| an_attr(&format!("{id}-a{i}"), "koru.tag", "text", tag))
            .collect(),
    )
}

fn counts(groups: &[AttributeGroup]) -> Vec<(String, usize)> {
    groups.iter().map(|g| (g.tag.clone(), g.count)).collect()
}

/// **The counts are the corpus, not the page.** A group header saying 42 over
/// a list holding the twenty rows it fetched is worse than no header, so the
/// count comes from its own statement with the same filter and no `LIMIT`.
#[test]
fn a_group_counts_the_whole_corpus_rather_than_one_page() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let (a, b) = (DocumentId::new(), DocumentId::new());
    seeded_many(
        &mut p,
        ws,
        &[
            (
                a,
                "Tasks",
                vec![
                    tag_rows("n1", "one", &["mvp"]),
                    tag_rows("n2", "two", &["mvp"]),
                    tag_rows("n3", "three", &["later"]),
                ],
            ),
            (b, "More", vec![tag_rows("n4", "four", &["mvp"])]),
        ],
    );

    let paged = AttributeRowQuery {
        limit: 1,
        ..query()
    };
    assert_eq!(p.attribute_rows(&paged).unwrap().len(), 1);
    assert_eq!(
        counts(&p.attribute_groups(&paged, &[]).unwrap()),
        [("mvp".to_owned(), 3), ("later".to_owned(), 1)]
    );
}

/// A node carrying two tags is one row of each group and never two of either —
/// which is why the counts add up to more than the row count, and why the
/// statement counts distinct `(document, node)` rather than rows.
#[test]
fn a_node_with_two_tags_counts_once_in_each_group() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let doc = DocumentId::new();
    seeded_many(
        &mut p,
        ws,
        &[(
            doc,
            "Tasks",
            vec![tag_rows("n1", "both", &["mvp", "later"])],
        )],
    );

    let groups = p.attribute_groups(&query(), &[]).unwrap();
    assert_eq!(
        counts(&groups),
        [("later".to_owned(), 1), ("mvp".to_owned(), 1)]
    );
    assert_eq!(p.attribute_rows(&query()).unwrap().len(), 1);
}

/// Binned rows are **counted and flagged**, the same rule the row query
/// follows. Hiding them would make a heading disagree with the list under it;
/// counting them silently would hide that the work is in the bin.
#[test]
fn a_group_counts_binned_rows_and_says_how_many() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let (live, binned, child) = (DocumentId::new(), DocumentId::new(), DocumentId::new());
    for (doc, node) in [(live, "l"), (binned, "b"), (child, "c")] {
        let mut projection = a_projection(ws, doc, b"rev-1");
        projection.nodes = vec![tag_rows(node, "row", &["mvp"])];
        p.replace(&projection, 1, None).unwrap();
    }
    let mut bin = a_listed(binned, "Binned");
    bin.trashed_at = Some(9);
    let mut nested = a_listed(child, "Nested");
    nested.parent = Some(binned);
    p.set_listing(ws, "Acme", &[a_listed(live, "Live"), bin, nested], 1)
        .unwrap();

    let groups = p.attribute_groups(&query(), &[]).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].count, 3);
    assert_eq!(
        groups[0].trashed, 2,
        "the child of a binned parent carries no stamp and inherits the flag"
    );
}

/// A `TagId` belongs to one workspace's table, so a group does too — folding
/// two workspaces' counts into one heading would name a tag neither table has.
#[test]
fn groups_are_per_workspace_like_the_tag_tables_they_are_named_from() {
    let mut p = Projections::in_memory().unwrap();
    let (ws_a, ws_b) = (WorkspaceId::new(), WorkspaceId::new());
    seeded_many(
        &mut p,
        ws_a,
        &[(DocumentId::new(), "A", vec![tag_rows("a", "ours", &["t"])])],
    );
    seeded_many(
        &mut p,
        ws_b,
        &[(
            DocumentId::new(),
            "B",
            vec![tag_rows("b", "theirs", &["t"])],
        )],
    );

    let all = p.attribute_groups(&query(), &[]).unwrap();
    assert_eq!(
        all.len(),
        2,
        "one group per workspace, not one merged group"
    );
    assert!(all.iter().all(|g| g.count == 1));

    let scoped = p
        .attribute_groups(
            &AttributeRowQuery {
                workspaces: vec![ws_a],
                ..query()
            },
            &[],
        )
        .unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].workspace, ws_a);
}

/// The filters are the row query's, so a group counts the set the list under
/// it is showing — never the whole workspace.
#[test]
fn a_group_obeys_the_same_filters_as_the_rows() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let doc = DocumentId::new();
    let mut with_due = tag_rows("due", "has a due", &["mvp"]);
    with_due
        .attrs
        .push(an_attr("d1", "koru.due", "date", "2026-09-01"));
    seeded_many(
        &mut p,
        ws,
        &[(
            doc,
            "Tasks",
            vec![with_due, tag_rows("bare", "no due", &["mvp"])],
        )],
    );

    let groups = p
        .attribute_groups(
            &AttributeRowQuery {
                has: Some("koru.due".into()),
                ..query()
            },
            &[],
        )
        .unwrap();
    assert_eq!(counts(&groups), [("mvp".to_owned(), 1)]);
}

/// **`merge_tags` re-points no node**, so nodes go on carrying an id the live
/// table cannot name. Grouping on the raw value therefore splits one group
/// into a named half and a nameless one — a merge, visible as a bug. The alias
/// fold is what closes it, and it happens in SQL because a node carrying
/// *both* ids is one node of one group: adding the two counts afterwards would
/// say two.
#[test]
fn a_merged_tag_folds_into_the_tag_it_was_merged_into() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let doc = DocumentId::new();
    seeded_many(
        &mut p,
        ws,
        &[(
            doc,
            "Tasks",
            vec![
                tag_rows("n1", "old id", &["gone"]),
                tag_rows("n2", "new id", &["kept"]),
                tag_rows("n3", "both, after a merge", &["gone", "kept"]),
            ],
        )],
    );

    let unfolded = p.attribute_groups(&query(), &[]).unwrap();
    assert_eq!(
        counts(&unfolded),
        [("gone".to_owned(), 2), ("kept".to_owned(), 2)],
        "without the fold, the merged-away id is its own nameless group"
    );

    let folded = p
        .attribute_groups(&query(), &[("gone".to_owned(), "kept".to_owned())])
        .unwrap();
    assert_eq!(
        counts(&folded),
        [("kept".to_owned(), 3)],
        "three distinct nodes, and the one carrying both is not counted twice"
    );
}

/// **A document no listing names is reported, with enough of it to judge.**
///
/// The title comes from the document's own first row rather than from the
/// listing, which is the whole reason a review is possible: the manifest entry
/// is the thing that went missing, and the title survives it.
#[test]
fn a_document_no_listing_names_is_reported_with_its_content() {
    let workspace = WorkspaceId::new();
    let unfiled = DocumentId::new();
    let mut projections = Projections::in_memory().unwrap();
    projections
        .replace(
            &DocumentProjection {
                workspace,
                document: unfiled,
                source_revision: b"r1".to_vec(),
                nodes: vec![
                    a_node("n1", "Roblox Elements"),
                    a_node("n2", "Visible progress everywhere"),
                    a_node("n3", "and a second line"),
                ],
            },
            10,
            None,
        )
        .unwrap();
    // A listing for the workspace that does not name it.
    projections
        .set_listing(workspace, "Execution", &[], 1)
        .unwrap();

    let found = projections.unfiled_documents().unwrap();
    assert_eq!(found.len(), 1);
    let row = &found[0];
    assert_eq!(row.document, unfiled);
    assert_eq!(row.workspace, workspace);
    assert_eq!(row.workspace_name, "Execution");
    assert_eq!(row.title, "Roblox Elements");
    assert_eq!(
        row.preview,
        "Visible progress everywhere / and a second line"
    );
    assert_eq!(row.nodes, 3);
    assert_eq!(row.projected_at, 10);
    assert!(!row.same_title_as_listed);
}

/// A document the listing names is not unfiled — including a trashed one, which
/// is a different state and already has a screen.
#[test]
fn a_listed_document_is_not_reported() {
    let workspace = WorkspaceId::new();
    let live = DocumentId::new();
    let binned = DocumentId::new();
    let mut projections = Projections::in_memory().unwrap();
    for document in [live, binned] {
        projections
            .replace(&a_projection(workspace, document, b"r1"), 1, None)
            .unwrap();
    }
    let mut trashed = a_listed(binned, "Old notes");
    trashed.trashed_at = Some(5);
    projections
        .set_listing(
            workspace,
            "Execution",
            &[a_listed(live, "Plans"), trashed],
            1,
        )
        .unwrap();

    assert!(projections.unfiled_documents().unwrap().is_empty());
}

/// **The duplicate flag, which is what a purge decision would rest on.**
///
/// Same title as a document the listing *does* name means "possibly a stray
/// second copy"; no match means "possibly the only copy". Deliberately weak —
/// it prompts a look rather than deciding anything.
#[test]
fn sharing_a_title_with_a_listed_document_is_flagged() {
    let workspace = WorkspaceId::new();
    let live = DocumentId::new();
    let twin = DocumentId::new();
    let alone = DocumentId::new();
    let mut projections = Projections::in_memory().unwrap();
    projections
        .replace(&a_projection(workspace, live, b"r1"), 1, None)
        .unwrap();
    for (document, title) in [(twin, "Plans"), (alone, "Something else")] {
        projections
            .replace(
                &DocumentProjection {
                    workspace,
                    document,
                    source_revision: b"r1".to_vec(),
                    nodes: vec![a_node("n1", title)],
                },
                1,
                None,
            )
            .unwrap();
    }
    projections
        .set_listing(workspace, "Execution", &[a_listed(live, "Plans")], 1)
        .unwrap();

    let found = projections.unfiled_documents().unwrap();
    assert_eq!(found.len(), 2);
    let flagged: Vec<_> = found
        .iter()
        .filter(|d| d.same_title_as_listed)
        .map(|d| d.document)
        .collect();
    assert_eq!(flagged, vec![twin]);
}

/// An untitled document must not be flagged as a duplicate of every other
/// untitled one — an empty title is an absence, not a match.
#[test]
fn an_empty_title_is_never_a_duplicate() {
    let workspace = WorkspaceId::new();
    let listed = DocumentId::new();
    let unfiled = DocumentId::new();
    let mut projections = Projections::in_memory().unwrap();
    for document in [listed, unfiled] {
        projections
            .replace(
                &DocumentProjection {
                    workspace,
                    document,
                    source_revision: b"r1".to_vec(),
                    nodes: vec![a_node("n1", "  ")],
                },
                1,
                None,
            )
            .unwrap();
    }
    projections
        .set_listing(workspace, "Execution", &[a_listed(listed, "")], 1)
        .unwrap();

    let found = projections.unfiled_documents().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].title, "");
    assert!(!found[0].same_title_as_listed);
}

// ---------------------------------------------- the bin, and who is shown it
//
// Two notions of trash meet in this file and they are not the same question.
// `listing.trashed_at` is a binned **document**, inherited down the document
// tree, and those rows are *flagged, never hidden* — a consumer can see the
// flag and decide. `nodes.in_trash` is a node somebody binned **inside** a
// document, and those rows are *stored, and left out unless asked for* —
// nothing on the wire could carry a flag for one, so handing them over would
// put binned content into live answers with nothing to mark it.
//
// Each test below pins one query's side of that.

/// The write path keeps the bin, and the default read hands back the live
/// document. Both halves matter: dropping the rows would make searching the
/// bin impossible, and returning them by default would put content somebody
/// threw away into every ordinary answer.
#[test]
fn the_bin_is_stored_and_the_default_read_leaves_it_out() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let mut projection = a_projection(ws, doc, b"rev-1");
    projection.nodes = vec![a_node("n1", "on the page"), a_binned_node("n2", "binned")];
    p.replace(&projection, 1, None).unwrap();

    let live = p.read(doc).unwrap().unwrap();
    assert_eq!(
        live.nodes
            .iter()
            .map(|n| n.text.as_str())
            .collect::<Vec<_>>(),
        ["on the page"]
    );
    assert!(live.nodes.iter().all(|n| !n.in_trash));

    let all = p.read_with_trash(doc).unwrap().unwrap();
    assert_eq!(all.nodes.len(), 2);
    assert_eq!(all.nodes[1].text, "binned");
    assert!(all.nodes[1].in_trash);
    assert_eq!(all, projection, "the exact inverse of what was written");
}

/// **The consumer audit, end to end, through the real walk.** The search
/// indexer's only door into this database is
/// [`read`](Projections::read) — it throws the projection it is handed away and
/// re-reads, so this is literally what it tokenizes. Bin a paragraph in a real
/// document and its text must not be in there; restore it and it must be.
#[test]
fn what_the_search_indexer_reads_is_the_live_document() {
    use koru_core::node::Node;
    use koru_core::schema::{Parent, WorkspaceDoc};

    let mut doc_ws = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "W").unwrap();
    let root = doc_ws
        .insert(Parent::Root, Node::document("Roadmap").unwrap())
        .unwrap();
    let binned = doc_ws
        .insert(Parent::Node(root), Node::paragraph("thrown away").unwrap())
        .unwrap();
    doc_ws
        .insert(Parent::Node(root), Node::paragraph("still here").unwrap())
        .unwrap();
    doc_ws.trash(binned, 1_000, Some("Ana")).unwrap();

    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let project =
        |doc_ws: &WorkspaceDoc| crate::walk::project(doc_ws, ws, doc, b"rev".to_vec()).unwrap();
    p.replace(&project(&doc_ws), 1, None).unwrap();

    let indexed: Vec<String> = p
        .read(doc)
        .unwrap()
        .unwrap()
        .nodes
        .iter()
        .map(|n| n.text.clone())
        .collect();
    assert_eq!(indexed, ["Roadmap", "still here"]);

    // ...and the bin is in the database all the same, which is what makes
    // searching it possible at all.
    assert!(
        p.read_with_trash(doc)
            .unwrap()
            .unwrap()
            .nodes
            .iter()
            .any(|n| n.text == "thrown away" && n.in_trash)
    );

    doc_ws.restore(binned, Some("Ana")).unwrap();
    p.replace(&project(&doc_ws), 2, None).unwrap();
    assert!(
        p.read(doc)
            .unwrap()
            .unwrap()
            .nodes
            .iter()
            .any(|n| n.text == "thrown away")
    );
}

/// Restoring is the same move backwards, so the next projection puts the row
/// back into the live answer — without any consumer having to know that
/// node-level trash exists.
#[test]
fn restoring_a_node_returns_it_to_the_default_read() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let mut binned = a_projection(ws, doc, b"rev-1");
    binned.nodes = vec![a_binned_node("n1", "binned")];
    p.replace(&binned, 1, None).unwrap();
    assert!(p.read(doc).unwrap().unwrap().nodes.is_empty());

    let mut restored = a_projection(ws, doc, b"rev-2");
    restored.nodes = vec![a_node("n1", "binned")];
    p.replace(&restored, 2, None).unwrap();

    let live = p.read(doc).unwrap().unwrap();
    assert_eq!(live.nodes.len(), 1);
    assert!(!live.nodes[0].in_trash);
}

/// A binned heading is not a jump target: it is not on the page, and offering
/// it would send somebody to a row they cannot see.
#[test]
fn headings_leave_the_bin_out() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let mut binned = a_heading("h2", "Thrown away", 1);
    binned.in_trash = true;
    let mut projection = a_projection(ws, doc, b"rev-1");
    projection.nodes = vec![a_heading("h1", "On the page", 1), binned];
    p.replace(&projection, 1, None).unwrap();

    assert_eq!(
        p.headings(doc)
            .unwrap()
            .iter()
            .map(|h| h.text.clone())
            .collect::<Vec<_>>(),
        ["On the page"]
    );
}

/// **The regression this plan had to not cause.** An attribute page is the live
/// document's attributes; a due date on a node somebody binned is not one of
/// them, and `AttributeRow` has no field that could have said so.
#[test]
fn an_attribute_page_leaves_binned_nodes_out() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let mut binned = attributed(
        "n2",
        "thrown away",
        vec![an_attr("a2", "koru.due", "date", "2026-09-02")],
    );
    binned.in_trash = true;
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            attributed(
                "n1",
                "ship it",
                vec![an_attr("a1", "koru.due", "date", "2026-09-01")],
            ),
            binned,
        ],
    );

    let rows = p.attribute_rows(&query()).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.node.as_str()).collect::<Vec<_>>(),
        ["n1"]
    );
    assert!(!rows[0].trashed, "the *document* is not in the trash");
}

/// The count above the list has to be the list. Both queries share
/// `attribute_conditions`, so this is what says the sharing still holds now
/// that the corpus has a bin in it.
#[test]
fn a_tag_group_counts_what_the_page_returns() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let tag = "tag-1";
    let mut binned = attributed(
        "n2",
        "thrown away",
        vec![an_attr("a2", "koru.tag", "ref", tag)],
    );
    binned.in_trash = true;
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            attributed("n1", "ship it", vec![an_attr("a1", "koru.tag", "ref", tag)]),
            binned,
        ],
    );

    let groups = p.attribute_groups(&query(), &[]).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].tag, tag);
    assert_eq!(groups[0].count, 1, "one live node carries it");
    assert_eq!(groups[0].trashed, 0, "no *document* is binned here");
    assert_eq!(p.attribute_rows(&query()).unwrap().len(), groups[0].count);
}

/// The attr snapshot is what the event diff compares against, so it has to
/// answer the same question the page does — or binning a node would leave its
/// due date announced forever and restoring it would say nothing.
#[test]
fn the_attr_snapshot_leaves_binned_nodes_out() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let mut binned = attributed(
        "n2",
        "thrown away",
        vec![an_attr("a2", "koru.due", "date", "2026-09-02")],
    );
    binned.in_trash = true;
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            attributed(
                "n1",
                "ship it",
                vec![an_attr("a1", "koru.due", "date", "2026-09-01")],
            ),
            binned,
        ],
    );

    assert_eq!(
        p.document_attrs(doc)
            .unwrap()
            .iter()
            .map(|(node, _)| node.clone())
            .collect::<Vec<_>>(),
        ["n1"]
    );
    assert_eq!(p.attrs_in_workspace(ws).unwrap()[&doc].len(), 1);
}

/// **The one query where the bin is deliberately counted.** This list is what a
/// blob sweep keeps; dropping a hash because the node holding it is binned
/// would let the bytes go while the node is still restorable, and the restore
/// would hand back a broken picture.
#[test]
fn a_binned_nodes_blob_is_still_kept_alive() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let mut binned = a_binned_node("n2", "thrown away");
    binned.blob = Some("blake3:binned".to_owned());
    let mut projection = a_projection(ws, doc, b"rev-1");
    projection.nodes = vec![a_node("n1", "on the page"), binned];
    p.replace(&projection, 1, None).unwrap();
    let mut entry = a_listed(doc, "A picture");
    entry.kind = "file".into();
    p.set_listing(ws, "Acme", &[entry], 1).unwrap();

    assert_eq!(p.live_file_blob_hashes(ws).unwrap(), ["blake3:binned"]);
}

/// The unreachable-document report is read by somebody deciding what a document
/// *is*. Counting its bin into "how big is it", or previewing a line out of it,
/// would answer with content the author had already thrown away.
#[test]
fn the_unfiled_report_describes_the_live_document() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let mut p = Projections::in_memory().unwrap();
    p.replace(
        &DocumentProjection {
            workspace,
            document,
            source_revision: b"r1".to_vec(),
            nodes: vec![
                a_node("n1", "Roadmap"),
                a_node("n2", "on the page"),
                a_binned_node("n3", "thrown away"),
            ],
        },
        1,
        None,
    )
    .unwrap();
    p.set_listing(workspace, "Execution", &[], 1).unwrap();

    let found = p.unfiled_documents().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].nodes, 2);
    assert_eq!(found[0].preview, "on the page");
}

/// **A schema change is a rebuild, and this is the bump that proves it for
/// `in_trash`.** A projection written by the previous shape has no such column;
/// keeping those rows would mean answering "is this binned" from a default.
#[test]
fn a_projection_written_before_the_bin_was_visible_is_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("projection.db");
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    {
        let mut p = Projections::at(&path).unwrap();
        p.replace(&a_projection(ws, doc, b"rev-1"), 1, None)
            .unwrap();
        p.db.pragma_update(None, "user_version", PROJECTION_SCHEMA_VERSION - 1)
            .unwrap();
    }

    let p = Projections::at(&path).unwrap();
    assert!(p.read(doc).unwrap().is_none(), "rebuilt, not migrated");
    assert_eq!(
        p.db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        PROJECTION_SCHEMA_VERSION
    );
}

// ------------------------------------------------------------ tasks queries

/// A task by the marker alone — no due, no state, which is the shape
/// `ensure_task` writes.
fn a_marked_task(id: &str, text: &str) -> ProjectedNode {
    attributed(
        id,
        text,
        vec![an_attr(
            &format!("{id}-task"),
            "koru.task",
            "text",
            "manual",
        )],
    )
}

/// The same node with a lifecycle written on it.
fn completed(mut node: ProjectedNode) -> ProjectedNode {
    let id = node.id.clone();
    node.attrs.push(an_attr(
        &format!("{id}-state"),
        "koru.task_state",
        "text",
        "completed",
    ));
    node
}

/// The same node with a completion day, as `complete_task` writes the pair.
fn completed_on(node: ProjectedNode, day: &str) -> ProjectedNode {
    let mut node = completed(node);
    let id = node.id.clone();
    node.attrs.push(an_attr(
        &format!("{id}-day"),
        "koru.completed_at",
        "date",
        day,
    ));
    node
}

fn tasks() -> TaskRowQuery {
    TaskRowQuery {
        limit: 100,
        ..TaskRowQuery::default()
    }
}

fn task_nodes(page: &TaskPage) -> Vec<String> {
    page.rows.iter().map(|r| r.node.clone()).collect()
}

/// **The derivation, in SQL, is the domain's.** Marker or due — the legacy bare
/// `due` role included, so a document written before `koru.due` existed is a
/// task list without a migration — and nothing else is.
#[test]
fn a_task_is_a_marker_or_a_due_and_the_legacy_role_counts() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            a_marked_task("marked", "marked only"),
            attributed(
                "dated",
                "due only",
                vec![an_attr("d1", "koru.due", "date", "2026-09-01")],
            ),
            attributed(
                "legacy",
                "an older document",
                vec![an_attr("d2", "due", "date", "2026-09-02")],
            ),
            attributed(
                "tagged",
                "an attribute, not a task",
                vec![an_attr("t1", "koru.tag", "text", "some-tag")],
            ),
            a_node("bare", "a plain paragraph"),
        ],
    );

    let mut found = task_nodes(&p.task_rows(&tasks()).unwrap());
    found.sort();
    assert_eq!(found, ["dated", "legacy", "marked"]);
}

/// SPEC §4's closed set is rendered from the domain's list, so a due on a kind
/// that cannot carry task state is not a task row — the same refusal the
/// commands make, asked of the read model.
#[test]
fn a_due_on_a_kind_that_cannot_carry_a_task_is_not_a_row() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let mut quoted = attributed(
        "quoted",
        "a quote with a date in it",
        vec![an_attr("d1", "koru.due", "date", "2026-09-01")],
    );
    quoted.kind = "quote".into();
    let mut imaged = attributed(
        "imaged",
        "a picture with a deadline",
        vec![an_attr("d2", "koru.due", "date", "2026-09-01")],
    );
    imaged.kind = "image".into();
    seeded(&mut p, ws, doc, vec![quoted, imaged]);

    assert_eq!(task_nodes(&p.task_rows(&tasks()).unwrap()), ["imaged"]);
}

/// **Absence is open**, which is the read rule the whole derivation depends on:
/// a task that exists only because it carries a due has nobody to write it a
/// state, and it is still the answer to *what should I do next*.
#[test]
fn a_task_with_no_stored_state_is_open() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            a_marked_task("open", "still to do"),
            completed(a_marked_task("done", "finished")),
        ],
    );

    let open = p.task_rows(&tasks()).unwrap();
    assert_eq!(task_nodes(&open), ["open"]);
    assert_eq!(open.rows[0].lifecycle, "open");

    let done = p
        .task_rows(&TaskRowQuery {
            lifecycle: LifecycleFilter::Completed,
            ..tasks()
        })
        .unwrap();
    assert_eq!(task_nodes(&done), ["done"]);
    assert_eq!(done.rows[0].lifecycle, "completed");

    let mut all = task_nodes(
        &p.task_rows(&TaskRowQuery {
            lifecycle: LifecycleFilter::All,
            ..tasks()
        })
        .unwrap(),
    );
    all.sort();
    assert_eq!(all, ["done", "open"]);
}

/// **Two trash flags, two rules.** A binned *node* is left out unless asked
/// for; a task in a binned *document* is always returned and flagged, because
/// restore has to be able to find it.
#[test]
fn the_two_kinds_of_trash_are_answered_separately() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let (live, binned) = (DocumentId::new(), DocumentId::new());

    let mut here = a_projection(ws, live, b"rev-1");
    here.nodes = vec![a_marked_task("visible", "on the page"), {
        let mut node = a_marked_task("binned-node", "thrown away");
        node.in_trash = true;
        node
    }];
    p.replace(&here, 1, None).unwrap();

    let mut there = a_projection(ws, binned, b"rev-1");
    there.nodes = vec![a_marked_task("in-binned-doc", "inside a binned page")];
    p.replace(&there, 1, None).unwrap();

    let mut trashed = a_listed(binned, "Binned");
    trashed.trashed_at = Some(9);
    p.set_listing(ws, "Acme", &[a_listed(live, "Tasks"), trashed], 1)
        .unwrap();

    let default = p.task_rows(&tasks()).unwrap();
    let mut names = task_nodes(&default);
    names.sort();
    assert_eq!(
        names,
        ["in-binned-doc", "visible"],
        "a binned document is flagged, a binned node is hidden"
    );
    let flagged = default
        .rows
        .iter()
        .find(|r| r.node == "in-binned-doc")
        .unwrap();
    assert!(flagged.document_trashed);
    assert!(!flagged.in_trash);

    let with_bin = p
        .task_rows(&TaskRowQuery {
            include_trashed: true,
            ..tasks()
        })
        .unwrap();
    let node = with_bin
        .rows
        .iter()
        .find(|r| r.node == "binned-node")
        .expect("the binned node is restorable, so it is findable");
    assert!(node.in_trash);
    assert!(!node.document_trashed, "its document is live");
}

/// The row a surface draws: the triple carried together, the reference that is
/// also the cursor, the title chain, and a breadcrumb assembled from the
/// workspace, the document path and the ancestor rows.
#[test]
fn a_row_carries_its_reference_its_title_and_where_it_lives() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let (parent, child) = (DocumentId::new(), DocumentId::new());

    let mut heading = a_node("section", "Milestones");
    heading.heading = Some(2);
    let mut task = a_marked_task("t1", "Ship it");
    task.parent = Some("section".into());

    let mut projection = a_projection(ws, child, b"rev-1");
    projection.nodes = vec![heading, task];
    p.replace(&projection, 1, None).unwrap();

    let mut nested = a_listed(child, "Q3 plan");
    nested.parent = Some(parent);
    p.set_listing(ws, "Acme", &[a_listed(parent, "Plans"), nested], 1)
        .unwrap();

    let page = p.task_rows(&tasks()).unwrap();
    assert_eq!(page.rows.len(), 1);
    let row = &page.rows[0];
    assert_eq!((row.workspace, row.document), (ws, child));
    assert_eq!(row.node, "t1");
    assert_eq!(
        row.task,
        format!("task:{}:{}:t1", ws.to_storage(), child.to_storage())
    );
    assert_eq!(row.node_kind, "paragraph");
    assert_eq!(row.title, "Ship it");
    assert_eq!(row.breadcrumb, "Acme / Plans / Q3 plan / Milestones");
    assert_eq!(row.attrs.len(), 1, "every attribute, not only the filtered");
}

/// A task with no text falls through `koru-title`'s chain rather than
/// rendering as a blank row in a list of things you can click.
#[test]
fn an_untitled_task_falls_through_the_one_title_chain() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let mut named = a_marked_task("named", "   ");
    named.name = Some("photo.png".into());
    named.kind = "image".into();
    seeded(
        &mut p,
        ws,
        doc,
        vec![named, a_marked_task("nameless", "  \t ")],
    );

    let page = p.task_rows(&tasks()).unwrap();
    let title = |node: &str| {
        page.rows
            .iter()
            .find(|r| r.node == node)
            .unwrap()
            .title
            .clone()
    };
    assert_eq!(title("named"), "photo.png");
    assert_eq!(title("nameless"), "Untitled");
}

/// **Keyset paging, on the reference.** Every row is seen once: the tie-break
/// is unique and stable, so a page boundary cannot repeat or skip under a
/// concurrent insert the way an offset can.
#[test]
fn paging_on_the_task_reference_neither_repeats_nor_skips() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let nodes: Vec<ProjectedNode> = (0..5)
        .map(|i| {
            attributed(
                &format!("n{i}"),
                "a task",
                vec![
                    an_attr(&format!("m{i}"), "koru.task", "text", "manual"),
                    // They share one due date, so every page boundary in this
                    // walk falls on the tie-break rather than on the key.
                    an_attr(&format!("d{i}"), "koru.due", "date", "2026-09-01"),
                ],
            )
        })
        .collect();
    seeded(&mut p, ws, doc, nodes);

    let mut seen: Vec<String> = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..5 {
        let page = p
            .task_rows(&TaskRowQuery {
                limit: 2,
                after: after.clone(),
                ..tasks()
            })
            .unwrap();
        assert!(!page.restarted);
        if page.rows.is_empty() {
            break;
        }
        after = Some(page.rows.last().unwrap().task.clone());
        seen.extend(page.rows.iter().map(|r| r.node.clone()));
    }
    seen.sort();
    let mut unique = seen.clone();
    unique.dedup();
    assert_eq!(seen, unique, "no row came back twice");
    assert_eq!(seen.len(), 5, "and none was skipped");
}

/// An undated task sorts **last**, and the tail pages like the head — the
/// NULL-keyed half of a keyset cursor is the part that is easy to get wrong.
#[test]
fn undated_tasks_sort_last_and_still_paginate() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            a_marked_task("undated-a", "someday"),
            a_marked_task("undated-b", "someday too"),
            attributed(
                "dated",
                "soon",
                vec![an_attr("d1", "koru.due", "date", "2026-09-01")],
            ),
        ],
    );

    let first = p
        .task_rows(&TaskRowQuery {
            limit: 1,
            ..tasks()
        })
        .unwrap();
    assert_eq!(task_nodes(&first), ["dated"], "a due date comes first");

    let mut rest = Vec::new();
    let mut after = Some(first.rows[0].task.clone());
    while let Some(cursor) = after.take() {
        let page = p
            .task_rows(&TaskRowQuery {
                limit: 1,
                after: Some(cursor),
                ..tasks()
            })
            .unwrap();
        assert!(!page.restarted);
        let Some(row) = page.rows.first() else { break };
        rest.push(row.node.clone());
        after = Some(row.task.clone());
    }
    rest.sort();
    assert_eq!(rest, ["undated-a", "undated-b"]);
}

/// **The dragged order**: ranked tasks by rank (a negative rank, a task typed
/// on top, included), then the never-ranked by due, then the undated — and the
/// keyset cursor pages it one row at a time without skipping or repeating.
#[test]
fn the_manual_sort_is_rank_then_due_then_undated_and_pages_by_cursor() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let ranked = |id: &str, rank: &str, due: Option<&str>| {
        let mut node = a_marked_task(id, id);
        node.attrs
            .push(an_attr(&format!("{id}-rank"), "koru.rank", "number", rank));
        if let Some(due) = due {
            node.attrs
                .push(an_attr(&format!("{id}-due"), "koru.due", "date", due));
        }
        node
    };
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            a_marked_task("undated-a", "someday"),
            ranked("second", "2097152", Some("2026-08-01")),
            attributed(
                "late",
                "late",
                vec![an_attr("d1", "koru.due", "date", "2026-10-01")],
            ),
            ranked("first", "-1048576", None),
            attributed(
                "early",
                "early",
                vec![an_attr("d2", "koru.due", "date", "2026-09-01")],
            ),
            a_marked_task("undated-b", "someday too"),
        ],
    );
    let manual = || TaskRowQuery {
        sort: TaskSort::Manual,
        ..tasks()
    };
    let whole = task_nodes(&p.task_rows(&manual()).unwrap());
    assert_eq!(&whole[..4], ["first", "second", "early", "late"]);
    let mut tail = whole[4..].to_vec();
    tail.sort();
    assert_eq!(tail, ["undated-a", "undated-b"]);

    let mut paged = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = p
            .task_rows(&TaskRowQuery {
                limit: 1,
                after: after.clone(),
                ..manual()
            })
            .unwrap();
        assert!(!page.restarted);
        let Some(row) = page.rows.first() else { break };
        paged.push(row.node.clone());
        after = Some(row.task.clone());
    }
    assert_eq!(
        paged, whole,
        "one row a page, the same order, nothing twice"
    );
}

/// A cursor the filter no longer matches is **not an error**: the answer comes
/// from the top with `restarted` set, so a client can tell a fresh list from a
/// continuation instead of silently drawing page one twice.
#[test]
fn a_stale_cursor_restarts_the_list_rather_than_failing_it() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            a_marked_task("one", "first"),
            a_marked_task("two", "second"),
        ],
    );

    let first = p
        .task_rows(&TaskRowQuery {
            limit: 1,
            ..tasks()
        })
        .unwrap();
    let cursor = first.rows[0].task.clone();
    let ended_on = first.rows[0].node.clone();
    let other = if ended_on == "one" { "two" } else { "one" };

    // The row the cursor named is completed between pages, so the open filter
    // no longer matches it.
    let mut after_edit = a_projection(ws, doc, b"rev-2");
    after_edit.nodes = vec![
        completed(a_marked_task(&ended_on, "first")),
        a_marked_task(other, "second"),
    ];
    p.replace(&after_edit, 2, None).unwrap();

    let page = p
        .task_rows(&TaskRowQuery {
            limit: 1,
            after: Some(cursor.clone()),
            ..tasks()
        })
        .unwrap();
    assert!(page.restarted, "the cursor is gone, so the list restarts");
    assert_eq!(page.rows.len(), 1);
    assert_ne!(page.rows[0].task, cursor);

    // A cursor that never named anything at all is the same answer.
    let nonsense = p
        .task_rows(&TaskRowQuery {
            after: Some("task:not:a:reference".into()),
            ..tasks()
        })
        .unwrap();
    assert!(nonsense.restarted);
    assert_eq!(nonsense.rows.len(), 1);
}

/// The filters are the attribute query's, through the one builder — so a
/// priority floor means the same thing to a task page as to an attribute page.
#[test]
fn a_task_query_filters_through_the_shared_builder() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            attributed(
                "urgent",
                "now",
                vec![
                    an_attr("m1", "koru.task", "text", "manual"),
                    an_attr("p1", "koru.priority", "text", "urgent"),
                ],
            ),
            attributed(
                "low",
                "later",
                vec![
                    an_attr("m2", "koru.task", "text", "manual"),
                    an_attr("p2", "koru.priority", "text", "low"),
                ],
            ),
        ],
    );

    let page = p
        .task_rows(&TaskRowQuery {
            attributes: AttributeRowQuery {
                min_priority: Some(Priority::High),
                ..AttributeRowQuery::default()
            },
            ..tasks()
        })
        .unwrap();
    assert_eq!(task_nodes(&page), ["urgent"]);
}

// ------------------------------------------------------- tasks: lists

/// A task in a list, spelled the way `set_list` writes it.
fn listed_task(id: &str, text: &str, list: &str) -> ProjectedNode {
    attributed(
        id,
        text,
        vec![
            an_attr(&format!("{id}-task"), "koru.task", "text", "manual"),
            an_attr(&format!("{id}-list"), "koru.list", "text", list),
        ],
    )
}

/// The same, with a calendar due on it.
fn listed_task_due(id: &str, text: &str, list: &str, due: &str) -> ProjectedNode {
    let mut node = listed_task(id, text, list);
    node.attrs
        .push(an_attr(&format!("{id}-due"), "koru.due", "date", due));
    node
}

/// **A list is a filter, not a place** (SPEC §8): one more predicate over one
/// more attribute, composing with the rest by AND.
#[test]
fn a_list_filter_is_one_more_predicate_over_the_same_corpus() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let wedding = "list-wedding";
    let bike = "list-bike";
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            listed_task("cake", "order the cake", wedding),
            listed_task_due("venue", "book the venue", wedding, "2026-09-04"),
            listed_task("groupset", "order the groupset", bike),
            a_marked_task("loose", "in no list at all"),
        ],
    );

    let mut in_wedding = task_nodes(
        &p.task_rows(&TaskRowQuery {
            list: Some(wedding.into()),
            ..tasks()
        })
        .unwrap(),
    );
    in_wedding.sort();
    assert_eq!(in_wedding, ["cake", "venue"]);

    // …and it ANDs with the filters that were already there.
    let dated = p
        .task_rows(&TaskRowQuery {
            list: Some(wedding.into()),
            attributes: AttributeRowQuery {
                has: Some("koru.due".into()),
                ..AttributeRowQuery::default()
            },
            ..tasks()
        })
        .unwrap();
    assert_eq!(task_nodes(&dated), ["venue"]);

    let ghost = p
        .task_rows(&TaskRowQuery {
            list: Some("list-deleted-yesterday".into()),
            ..tasks()
        })
        .unwrap();
    assert!(
        ghost.rows.is_empty(),
        "an id no table names matches nothing rather than erroring"
    );
}

/// The rail's numbers: a count per list over the **whole** corpus, and the
/// overdue share that tints it — by the caller's windows, because the display
/// zone is the reader's (SPEC §7).
#[test]
fn list_counts_cover_the_corpus_and_split_out_the_overdue() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let wedding = "list-wedding";
    let bike = "list-bike";
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            listed_task_due("cake", "order the cake", wedding, "2026-08-30"),
            listed_task_due("venue", "book the venue", wedding, "2026-09-30"),
            listed_task("dress", "no date at all", wedding),
            listed_task_due("groupset", "order the groupset", bike, "2026-08-29"),
            completed(listed_task("thanks", "write the thanks", wedding)),
            a_marked_task("loose", "in no list at all"),
        ],
    );

    // "Overdue" as a Zurich reader saw it on 2026-09-01.
    let overdue = DueFilter {
        dates: Some(DateWindow {
            after: None,
            before: Some("2026-09-01".into()),
        }),
        instants: Some(InstantWindow {
            after_ms: None,
            before_ms: Some(1_772_000_000_000),
        }),
    };

    let counts = p.task_list_counts(&tasks(), Some(&overdue)).unwrap();
    let by_list = |list: &str| {
        counts
            .iter()
            .find(|c| c.list == list)
            .cloned()
            .unwrap_or_else(|| panic!("{list} counted"))
    };
    assert_eq!(counts.len(), 2, "a row per list, and none for no list");
    let w = by_list(wedding);
    assert_eq!(
        (w.count, w.overdue, w.trashed),
        (3, 1, 0),
        "open only — the completed one is not in the rail's number — and \
         a dateless task counts but is never overdue"
    );
    assert_eq!(by_list(bike).count, 1);
    assert_eq!(by_list(bike).overdue, 1);
    assert_eq!(by_list(wedding).workspace, ws);

    let quiet = p.task_list_counts(&tasks(), None).unwrap();
    assert!(
        quiet.iter().all(|c| c.overdue == 0),
        "no windows, no overdue claim"
    );
    assert_eq!(quiet.iter().map(|c| c.count).sum::<usize>(), 4);

    let completed_counts = p
        .task_list_counts(
            &TaskRowQuery {
                lifecycle: LifecycleFilter::Completed,
                ..tasks()
            },
            Some(&overdue),
        )
        .unwrap();
    assert_eq!(completed_counts.len(), 1);
    assert_eq!(
        completed_counts[0].count, 1,
        "the Completed bucket's number"
    );
}

/// The counts are the **corpus**, never the page — the whole reason they are a
/// second statement rather than a fold over the rows.
#[test]
fn list_counts_do_not_stop_at_the_page_boundary() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let list = "list-wedding";
    let nodes: Vec<ProjectedNode> = (0..7)
        .map(|i| listed_task(&format!("t{i}"), "a task", list))
        .collect();
    seeded(&mut p, ws, doc, nodes);

    let page = p
        .task_rows(&TaskRowQuery {
            limit: 2,
            ..tasks()
        })
        .unwrap();
    assert_eq!(page.rows.len(), 2);
    let counts = p
        .task_list_counts(
            &TaskRowQuery {
                limit: 2,
                ..tasks()
            },
            None,
        )
        .unwrap();
    assert_eq!(counts[0].count, 7, "the corpus, not the page");
}

/// A task in a binned document is counted and flagged, exactly as its row is —
/// a rail number that hid them would disagree with the list underneath.
#[test]
fn list_counts_flag_binned_documents_rather_than_dropping_them() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let (live, binned) = (DocumentId::new(), DocumentId::new());
    let list = "list-wedding";

    let mut here = a_projection(ws, live, b"rev-1");
    here.nodes = vec![listed_task("here", "on the page", list)];
    p.replace(&here, 1, None).unwrap();
    let mut there = a_projection(ws, binned, b"rev-1");
    there.nodes = vec![listed_task("there", "inside a binned page", list)];
    p.replace(&there, 1, None).unwrap();
    let mut trashed = a_listed(binned, "Binned");
    trashed.trashed_at = Some(9);
    p.set_listing(ws, "Acme", &[a_listed(live, "Tasks"), trashed], 1)
        .unwrap();

    let counts = p.task_list_counts(&tasks(), None).unwrap();
    assert_eq!(counts.len(), 1);
    assert_eq!((counts[0].count, counts[0].trashed), (2, 1));
}

// ------------------------- tasks: freshness, pagination and scale (R-1)
//
// The corpus these run on is `crate::fixtures` — deterministic, and the same
// one P-Base measures, so an assertion here and a number there are about one
// fixture. What is asserted is **structure**: what came back, how often, and
// what did not move. Never a wall clock; `apps/koru/src/perf.rs` established
// why, and a task list is exactly the surface a raised threshold would stop
// protecting.

use crate::fixtures::{Corpus, PlannedTask, TaskCorpus};

/// Every task the default query owes an answer for, from the **plan** rather
/// than from a second run of the generator's arithmetic.
fn expected_default_answer(corpus: &Corpus) -> Vec<String> {
    let mut refs: Vec<String> = corpus
        .tasks()
        .filter(|t: &&PlannedTask| t.in_default_answer())
        .map(|t| t.reference.clone())
        .collect();
    refs.sort();
    refs
}

/// Walk the whole list by keyset, one page at a time, answering every reference
/// seen in order. The loop is bounded: a cursor that stopped moving is a
/// failure rather than a hang.
fn walk_all(p: &Projections, query: &TaskRowQuery) -> Vec<String> {
    let mut seen = Vec::new();
    // The walk continues whatever cursor it was handed, so a caller can start
    // it in the middle of a list.
    let mut after: Option<String> = query.after.clone();
    for _ in 0..10_000 {
        let page = p
            .task_rows(&TaskRowQuery {
                after: after.clone(),
                ..query.clone()
            })
            .unwrap();
        assert!(!page.restarted, "a cursor this walk minted went stale");
        let Some(last) = page.rows.last() else { break };
        after = Some(last.task.clone());
        seen.extend(page.rows.iter().map(|r| r.task.clone()));
        if page.rows.len() < query.limit {
            break;
        }
    }
    seen
}

/// **A page boundary survives an edit landing on the other side of it.**
///
/// This is what keyset paging is for and what an offset cannot do: a task
/// created *above* the cursor while a client is reading page two must not push
/// an unread row past it, and must not hand back a row already drawn. The
/// existing walk asserts the same property over a corpus nobody is editing;
/// this one edits between the pages.
#[test]
fn an_insert_landing_between_pages_neither_repeats_nor_skips_a_row() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let dated = |id: &str, day: &str| {
        attributed(
            id,
            "a task",
            vec![
                an_attr(&format!("m-{id}"), "koru.task", "text", "explicit"),
                an_attr(&format!("d-{id}"), "koru.due", "date", day),
            ],
        )
    };
    let original: Vec<ProjectedNode> = (1..=6)
        .map(|i| dated(&format!("n{i}"), &format!("2026-09-{:02}", i * 2)))
        .collect();
    seeded(&mut p, ws, doc, original.clone());

    let first = p
        .task_rows(&TaskRowQuery {
            limit: 2,
            ..tasks()
        })
        .unwrap();
    assert_eq!(task_nodes(&first), ["n1", "n2"]);
    let cursor = first.rows.last().unwrap().task.clone();

    // Between the pages: one task created **before** the cursor, one after it,
    // and the cursor's own row rewritten — its text, not its due. The sort key
    // is deliberately left alone: the cursor is re-derived on every request, so
    // moving the key a page boundary was taken on moves the boundary with it,
    // which is keyset paging working rather than a case this can assert around.
    let mut edited = a_projection(ws, doc, b"rev-2");
    edited.nodes = original;
    edited.nodes.push(dated("early", "2026-09-01"));
    edited.nodes.push(dated("late", "2026-09-30"));
    edited.nodes[1] = attributed(
        "n2",
        "the same task, retitled",
        vec![
            an_attr("m-n2", "koru.task", "text", "explicit"),
            an_attr("d-n2", "koru.due", "date", "2026-09-04"),
        ],
    );
    p.replace(&edited, 2, None).unwrap();

    let mut seen = task_nodes(&first);
    // `walk_all` answers references; the first page answered node ids. Compared
    // as nodes, which is what a reader would see drawn twice.
    seen.extend(
        walk_all(
            &p,
            &TaskRowQuery {
                limit: 2,
                after: Some(cursor),
                ..tasks()
            },
        )
        .into_iter()
        .map(|reference| reference.rsplit(':').next().unwrap().to_owned()),
    );

    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(seen.len(), unique.len(), "a row was drawn twice: {seen:?}");
    assert!(
        !seen.contains(&"early".to_owned()),
        "a row inserted above the cursor does not appear below it, which is the \
         page boundary holding rather than a row lost"
    );
    for expected in ["n3", "n4", "n5", "n6", "late"] {
        assert!(
            seen.contains(&expected.to_owned()),
            "{expected} was skipped: {seen:?}"
        );
    }
}

/// **A cold workspace answers nothing and says nothing is wrong**, and
/// converges when its sweep lands rather than when somebody edits.
///
/// The digest is the fact a port turns into `updating_workspaces`: absent means
/// *this workspace has not reached the read model*, which is a different answer
/// from *this workspace has no tasks* and the whole reason the response carries
/// both.
#[test]
fn a_cold_workspace_converges_when_its_sweep_lands_and_not_before() {
    let mut p = Projections::in_memory().unwrap();
    let corpus = TaskCorpus::small().plan();
    corpus.write_into(&mut p, 1).unwrap();
    let cold = corpus.cold().next().expect("one cold workspace").id;

    let scoped = |workspace: WorkspaceId| TaskRowQuery {
        attributes: AttributeRowQuery {
            workspaces: vec![workspace],
            ..AttributeRowQuery::default()
        },
        limit: 500,
        ..TaskRowQuery::default()
    };

    assert!(
        p.listing_digest(cold).unwrap().is_none(),
        "nothing has swept it"
    );
    assert!(
        p.task_rows(&scoped(cold)).unwrap().rows.is_empty(),
        "and it answers no rows rather than failing"
    );
    let warm = corpus.warm().next().unwrap().id;
    let before = p.task_rows(&scoped(warm)).unwrap().rows.len();
    assert!(
        before > 0,
        "the warm half answers while the cold one cannot"
    );

    corpus.sweep_in(&mut p, cold, 2).unwrap();

    assert!(p.listing_digest(cold).unwrap().is_some());
    let expected = corpus
        .workspaces
        .iter()
        .find(|w| w.id == cold)
        .unwrap()
        .documents
        .iter()
        .flat_map(|d| d.tasks.iter())
        .filter(|t| !t.completed && !t.in_trash)
        .count();
    assert_eq!(
        p.task_rows(&scoped(cold)).unwrap().rows.len(),
        expected,
        "converged, with no edit anywhere"
    );
    assert_eq!(
        p.task_rows(&scoped(warm)).unwrap().rows.len(),
        before,
        "and the workspace that was already warm did not move"
    );
}

/// **The whole corpus pages through exactly once** — 5,000 tasks among 55,000
/// rows, walked by keyset, compared against the plan rather than against a
/// count this file worked out for itself.
///
/// A page is bounded by its limit however large the corpus is, which is the
/// structural half of *a task list costs its page*: a query answering more than
/// it was asked for would be building the whole list in memory first.
#[test]
fn the_whole_corpus_pages_through_exactly_once() {
    let mut p = Projections::in_memory().unwrap();
    let corpus = TaskCorpus::roadmap().plan();
    corpus.write_into(&mut p, 1).unwrap();

    let query = TaskRowQuery {
        limit: 250,
        ..TaskRowQuery::default()
    };
    let first = p.task_rows(&query).unwrap();
    assert_eq!(
        first.rows.len(),
        query.limit,
        "a page is its limit, not the corpus"
    );

    let mut walked = walk_all(&p, &query);
    walked.sort();
    let mut unique = walked.clone();
    unique.dedup();
    assert_eq!(walked, unique, "a row came back on two pages");
    assert_eq!(
        walked,
        expected_default_answer(&corpus),
        "the walk is exactly the open, live tasks the plan holds"
    );
}

/// **Asking again is not more work.** Twenty identical queries over the whole
/// corpus answer identically and leave the read model exactly as they found it:
/// no cursor kept, no cache grown, no row written.
///
/// Structural rather than timed, deliberately. A timing assertion here would be
/// a promise about the machine; what the product needs promised is that a Tasks
/// view re-querying at every local midnight and on every resume is asking a
/// question rather than accumulating one.
#[test]
fn repeated_queries_answer_identically_and_change_nothing() {
    let mut p = Projections::in_memory().unwrap();
    // The rule is not about size — the size claim is the walk above — so this
    // runs on the corpus that costs a second rather than the one that costs
    // ten.
    let corpus = TaskCorpus::medium().plan();
    corpus.write_into(&mut p, 1).unwrap();

    // What the read model holds, as one number per table. Read through the
    // connection itself: this is the crate's own suite, and the claim is about
    // rows rather than about anything the public surface answers.
    let rows = |p: &Projections| -> (i64, i64, i64) {
        let count = |table: &str| {
            p.db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        };
        (count("nodes"), count("attrs"), count("listing"))
    };

    let query = TaskRowQuery {
        limit: 100,
        ..TaskRowQuery::default()
    };
    let before = rows(&p);
    let first = task_nodes(&p.task_rows(&query).unwrap());
    for _ in 0..19 {
        assert_eq!(
            task_nodes(&p.task_rows(&query).unwrap()),
            first,
            "the same question got a different answer"
        );
    }
    assert_eq!(rows(&p), before, "a query wrote something");
}

/// **Every filter the designed view draws reaches this corpus**, and each one
/// narrows it — R-1's mapping table, asserted rather than only written down.
/// `min_priority` is the one worth pinning here: it arrives through the shared
/// attribute builder, so a task page and an attribute page cannot come to
/// disagree about what *at least high* means.
#[test]
fn every_filter_the_view_draws_narrows_the_corpus() {
    let mut p = Projections::in_memory().unwrap();
    let corpus = TaskCorpus::medium().plan();
    corpus.write_into(&mut p, 1).unwrap();

    let counted = |query: TaskRowQuery| p.task_rows(&query).unwrap().rows.len();
    let all = TaskRowQuery {
        limit: 10_000,
        ..TaskRowQuery::default()
    };
    let open = counted(all.clone());

    // The Priority >= High chip.
    let high = counted(TaskRowQuery {
        attributes: AttributeRowQuery {
            min_priority: Some(Priority::High),
            ..AttributeRowQuery::default()
        },
        ..all.clone()
    });
    assert_eq!(
        high,
        corpus
            .tasks()
            .filter(|t| t.in_default_answer()
                && matches!(t.priority, Some(Priority::High | Priority::Urgent)))
            .count(),
        "the priority floor is the semantic order, over the whole corpus"
    );
    assert!(high < open && high > 0);

    // Open / Completed / All.
    let completed = counted(TaskRowQuery {
        lifecycle: LifecycleFilter::Completed,
        ..all.clone()
    });
    let both = counted(TaskRowQuery {
        lifecycle: LifecycleFilter::All,
        ..all.clone()
    });
    assert_eq!(open + completed, both, "the three lifecycles partition");

    // The due-window chip, stated per temporal type.
    let window = counted(TaskRowQuery {
        attributes: AttributeRowQuery {
            due: Some(DueFilter {
                dates: Some(DateWindow {
                    after: None,
                    before: Some("2026-04-01".to_owned()),
                }),
                instants: None,
            }),
            ..AttributeRowQuery::default()
        },
        ..all.clone()
    });
    assert_eq!(
        window,
        corpus
            .tasks()
            .filter(|t| t.in_default_answer() && t.due.as_deref().is_some_and(|d| d < "2026-04-01"))
            .count(),
        "the due window answers dates in their own terms"
    );

    // Show removed-to-trash, the overflow menu's third switch.
    let with_bin = counted(TaskRowQuery {
        include_trashed: true,
        ..all.clone()
    });
    assert!(
        with_bin > open,
        "the node bin is off by default and can be asked for"
    );

    // The three sorts answer one set in three orders.
    let ordered = |sort: TaskSort| {
        task_nodes(
            &p.task_rows(&TaskRowQuery {
                sort,
                ..all.clone()
            })
            .unwrap(),
        )
    };
    let by_due = ordered(TaskSort::Due);
    for sort in [TaskSort::Priority, TaskSort::Modified] {
        let other = ordered(sort);
        assert_eq!(
            other.len(),
            by_due.len(),
            "{sort:?} answered a different set"
        );
        assert_ne!(other, by_due, "{sort:?} ordered exactly like due");
    }
}

// ------------------------------------------------ tasks: the agenda buckets
//
// **The rule lives in `koru_core::tasks::bucket` and this is a second
// implementation of it**, so what these assert is agreement rather than
// arithmetic: the SQL places a row where the domain function places it, over
// the whole fixture corpus and over a hand-built grid of edges the corpus does
// not reach. A test that re-derived the expected bucket from the same formula
// the SQL uses would be the query agreeing with itself.

use chrono::{NaiveDate, NaiveDateTime, TimeZone};
use koru_core::attributes::Lifecycle;
use koru_core::time::{
    DateOnly, FloatingDateTime, Temporal, TimeZoneName, ZoneRules, ZonedDateTime,
};

/// A fixed-offset stand-in for a real zone — the precedent is `koru-core`'s own
/// suites, which need one for exactly this reason: no crate down here carries a
/// timezone database, and a bucket has to be asked in *some* zone.
struct FixedZone {
    name: TimeZoneName,
    offset_seconds: i32,
}

impl FixedZone {
    fn new(name: &str, offset_seconds: i32) -> Self {
        Self {
            name: TimeZoneName::parse(name).expect("a test zone name is valid"),
            offset_seconds,
        }
    }

    /// The instant a day **ends** at in this zone — midnight opening the next
    /// day, which is the edge a zoned due is compared against.
    fn day_ends_ms(&self, at: NaiveDate) -> i64 {
        self.instant_of(
            at.succ_opt()
                .expect("a fixture day has a successor")
                .and_time(chrono::NaiveTime::MIN),
        )
    }
}

impl ZoneRules for FixedZone {
    fn zone(&self) -> &TimeZoneName {
        &self.name
    }

    fn instant_of(&self, wall: NaiveDateTime) -> i64 {
        chrono::FixedOffset::east_opt(self.offset_seconds)
            .unwrap()
            .from_local_datetime(&wall)
            .single()
            .map(|dt| dt.timestamp_millis())
            .unwrap_or_default()
    }

    fn wall_of(&self, instant_ms: i64) -> NaiveDateTime {
        let offset = chrono::FixedOffset::east_opt(self.offset_seconds).unwrap();
        chrono::DateTime::from_timestamp_millis(instant_ms)
            .map(|utc| utc.with_timezone(&offset).naive_local())
            .unwrap_or_default()
    }
}

fn a_day(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("a fixture day is a day")
}

/// The windows a reader in `zone` sends on `today`, under SPEC 7's shipped
/// horizons — seven days and thirty. **Computed here rather than in the store**,
/// which is the whole point of the shape.
fn windows_on(today: &str, zone: &FixedZone) -> BucketWindows {
    let today = a_day(today);
    let horizon = today + chrono::Duration::days(7);
    let upcoming = today + chrono::Duration::days(30);
    BucketWindows {
        today: today.format("%Y-%m-%d").to_string(),
        horizon: horizon.format("%Y-%m-%d").to_string(),
        upcoming_horizon: upcoming.format("%Y-%m-%d").to_string(),
        today_ends_ms: zone.day_ends_ms(today),
        horizon_ends_ms: zone.day_ends_ms(horizon),
        upcoming_ends_ms: zone.day_ends_ms(upcoming),
        // Mid-morning, so "before now" is a strictly smaller set than "before
        // the end of today" and the two edges cannot pass by coincidence.
        now_ms: zone.instant_of(today.and_hms_opt(10, 0, 0).unwrap()),
        min_next_priority: Priority::High,
    }
}

/// Every task reference the query places in one bucket, walked to the end.
fn bucket_refs(p: &Projections, windows: &BucketWindows, bucket: Bucket) -> Vec<String> {
    let query = TaskRowQuery {
        bucket: Some(bucket),
        windows: Some(windows.clone()),
        include_trashed: true,
        limit: 250,
        ..TaskRowQuery::default()
    };
    let mut refs = walk_all(p, &query);
    refs.sort();
    refs
}

/// **The deliverable's heart: one rule, two implementations, no disagreement.**
///
/// Over the whole roadmap corpus — 5,000 tasks, binned documents and the node
/// bin included — every task's SQL bucket is the bucket
/// `koru_core::tasks::bucket` answers for the same five inputs. Exhaustive,
/// never sampled: what this guards against is one arm being off by a day, and a
/// sample is exactly what misses that.
#[test]
fn bucket_placement_agrees_with_the_domain_function() {
    let mut p = Projections::in_memory().unwrap();
    let corpus = TaskCorpus::roadmap().plan();
    corpus.write_into(&mut p, 1).unwrap();

    // A day inside the fixture's calendar year, so all five buckets are
    // populated and both horizons land on real content.
    let zone = FixedZone::new("Europe/Zurich", 2 * 3_600);
    let windows = windows_on("2026-06-15", &zone);
    let today = DateOnly::parse(&windows.today).unwrap();

    // What the domain function says, per task reference.
    let mut expected: HashMap<String, Bucket> = HashMap::new();
    for task in corpus.tasks().filter(|t: &&PlannedTask| !t.cold) {
        let due = task
            .due
            .as_deref()
            .map(|d| Temporal::Date(DateOnly::parse(d).expect("a fixture due is a day")));
        expected.insert(
            task.reference.clone(),
            koru_core::tasks::bucket(
                due.as_ref(),
                if task.completed {
                    Lifecycle::Completed
                } else {
                    Lifecycle::Open
                },
                task.priority,
                today,
                &zone,
            ),
        );
    }
    assert!(expected.len() > 3_000, "the corpus is the roadmap's corpus");

    let mut placed: HashMap<String, Bucket> = HashMap::new();
    for bucket in Bucket::ALL {
        for reference in bucket_refs(&p, &windows, bucket) {
            assert!(
                placed.insert(reference.clone(), bucket).is_none(),
                "{reference} was placed in two buckets"
            );
        }
    }

    assert_eq!(
        placed.len(),
        expected.len(),
        "the five buckets partition the corpus: every task in exactly one"
    );
    let mut disagreed: Vec<String> = expected
        .iter()
        .filter(|(reference, bucket)| placed.get(*reference) != Some(bucket))
        .map(|(reference, bucket)| {
            format!(
                "{reference}: SQL {:?}, domain {bucket:?}",
                placed.get(reference)
            )
        })
        .collect();
    disagreed.sort();
    assert!(
        disagreed.is_empty(),
        "{} of {} tasks were placed differently:\n{}",
        disagreed.len(),
        expected.len(),
        disagreed
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    // Every bucket did work — a partition that agreed because four arms were
    // empty would prove nothing.
    for bucket in Bucket::ALL {
        assert!(
            placed.values().any(|b| *b == bucket),
            "{bucket:?} is empty, so its arm was never exercised"
        );
    }
}

/// The corpus stores every due as a **calendar date**, so it cannot exercise the
/// two arms that matter most: a floating wall clock, whose day is read without a
/// zone, and a zoned instant, whose day is `date_in(zone)` and therefore lands
/// on a different calendar day for a reader two hours east of UTC.
///
/// This is that grid — every edge day, every temporal type, every priority and
/// both lifecycles — compared against the domain function one row at a time.
#[test]
fn every_temporal_type_lands_on_the_same_edge_as_the_domain_function() {
    let zone = FixedZone::new("Europe/Zurich", 2 * 3_600);
    let windows = windows_on("2026-06-15", &zone);
    let today = DateOnly::parse(&windows.today).unwrap();

    // Today, its edges, both horizons and one past each — the days an
    // off-by-one in any arm would move.
    let edges = [
        "2026-06-01", // long past: still Today, because overdue is Today
        "2026-06-15", // today itself
        "2026-06-16", // tomorrow: the first day of Next
        "2026-06-22", // the horizon: the last day of Next
        "2026-06-23", // the first day of Upcoming
        "2026-07-15", // the upcoming horizon: the last day of Upcoming
        "2026-07-16", // the first day of Later
    ];
    let priorities = [
        None,
        Some(Priority::Low),
        Some(Priority::Medium),
        Some(Priority::High),
        Some(Priority::Urgent),
    ];

    let mut nodes: Vec<ProjectedNode> = Vec::new();
    let mut expected: HashMap<String, Bucket> = HashMap::new();
    let mut id = 0usize;
    for edge in edges.iter().map(|e| Some(a_day(e))).chain([None]) {
        for kind in 0..3 {
            if edge.is_none() && kind > 0 {
                continue; // "no due" has one spelling, not three
            }
            for priority in priorities {
                for is_done in [false, true] {
                    id += 1;
                    let node_id = format!("grid-{id:04}");
                    let mut node = a_marked_task(&node_id, "a task");
                    let due: Option<Temporal> = edge.map(|d| match kind {
                        0 => Temporal::Date(DateOnly::new(d)),
                        // A wall clock late in the day: its own day, no zone.
                        1 => Temporal::Floating(FloatingDateTime::new(
                            d.and_hms_opt(23, 30, 0).unwrap(),
                        )),
                        // **00:30 in Zurich is the previous day in UTC**, so a
                        // query that folded an instant onto a UTC day would
                        // place every one of these a day early — which is the
                        // bug this whole shape exists to prevent, and the
                        // reason the edges are instants rather than days.
                        _ => Temporal::Zoned(ZonedDateTime::new(
                            zone.instant_of(d.and_hms_opt(0, 30, 0).unwrap()),
                            TimeZoneName::parse("Europe/Zurich").unwrap(),
                        )),
                    });
                    match &due {
                        Some(Temporal::Date(d)) => node.attrs.push(an_attr(
                            &format!("{node_id}-due"),
                            koru_core::attributes::KEY_DUE,
                            koru_core::time::TemporalKind::Date.token(),
                            &d.to_storage(),
                        )),
                        Some(Temporal::Floating(f)) => node.attrs.push(an_attr(
                            &format!("{node_id}-due"),
                            koru_core::attributes::KEY_DUE,
                            koru_core::time::TemporalKind::Floating.token(),
                            &f.to_storage(),
                        )),
                        Some(Temporal::Zoned(z)) => node.attrs.push(a_zoned_attr(
                            &format!("{node_id}-due"),
                            koru_core::attributes::KEY_DUE,
                            z.instant_ms(),
                            "Europe/Zurich",
                        )),
                        None => {}
                    }
                    if let Some(priority) = priority {
                        node.attrs.push(an_attr(
                            &format!("{node_id}-priority"),
                            koru_core::attributes::KEY_PRIORITY,
                            "text",
                            priority.as_str(),
                        ));
                    }
                    if is_done {
                        node = completed(node);
                    }
                    expected.insert(
                        node_id.clone(),
                        koru_core::tasks::bucket(
                            due.as_ref(),
                            if is_done {
                                Lifecycle::Completed
                            } else {
                                Lifecycle::Open
                            },
                            priority,
                            today,
                            &zone,
                        ),
                    );
                    nodes.push(node);
                }
            }
        }
    }

    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    seeded(&mut p, ws, doc, nodes);

    let mut placed: HashMap<String, Bucket> = HashMap::new();
    for bucket in Bucket::ALL {
        let page = p
            .task_rows(&TaskRowQuery {
                bucket: Some(bucket),
                windows: Some(windows.clone()),
                limit: 10_000,
                ..TaskRowQuery::default()
            })
            .unwrap();
        for row in page.rows {
            assert!(
                placed.insert(row.node.clone(), bucket).is_none(),
                "{} was placed in two buckets",
                row.node
            );
        }
    }

    let mut wrong: Vec<String> = expected
        .iter()
        .filter(|(node, bucket)| placed.get(*node) != Some(bucket))
        .map(|(node, bucket)| format!("{node}: SQL {:?}, domain {bucket:?}", placed.get(node)))
        .collect();
    wrong.sort();
    assert!(
        wrong.is_empty(),
        "{} of {} grid rows disagree:\n{}",
        wrong.len(),
        expected.len(),
        wrong.join("\n")
    );
}

/// **The union that cannot be spelled as a filter.** `Next` is *due within the
/// week* **or** *undated and important enough*, and filters AND — which is the
/// gap R-1 found and this closes. Both halves have to be in the answer, and a
/// dateless task below the floor has to be in `Later` instead.
#[test]
fn next_holds_both_halves_of_its_union() {
    let zone = FixedZone::new("Europe/Zurich", 2 * 3_600);
    let windows = windows_on("2026-06-15", &zone);

    let with_priority = |node: ProjectedNode, value: Priority| {
        let id = node.id.clone();
        let mut node = node;
        node.attrs.push(an_attr(
            &format!("{id}-priority"),
            koru_core::attributes::KEY_PRIORITY,
            "text",
            value.as_str(),
        ));
        node
    };
    let dated = |id: &str, due: &str| {
        let mut node = a_marked_task(id, "a task");
        node.attrs.push(an_attr(
            &format!("{id}-due"),
            koru_core::attributes::KEY_DUE,
            koru_core::time::TemporalKind::Date.token(),
            due,
        ));
        node
    };

    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            dated("this-week", "2026-06-18"),
            with_priority(a_marked_task("urgent-someday", "no date"), Priority::Urgent),
            with_priority(a_marked_task("high-someday", "no date"), Priority::High),
            with_priority(a_marked_task("medium-someday", "no date"), Priority::Medium),
            a_marked_task("plain-someday", "no date at all"),
            // Dated *and* high: the date decides, or the union double-counts.
            with_priority(dated("high-and-dated", "2026-06-30"), Priority::High),
        ],
    );

    let refs = |bucket: Bucket| {
        let mut nodes = task_nodes(
            &p.task_rows(&TaskRowQuery {
                bucket: Some(bucket),
                windows: Some(windows.clone()),
                limit: 100,
                ..TaskRowQuery::default()
            })
            .unwrap(),
        );
        nodes.sort();
        nodes
    };

    assert_eq!(
        refs(Bucket::Next),
        vec!["high-someday", "this-week", "urgent-someday"],
        "both halves of the union, and nothing below the floor"
    );
    assert_eq!(
        refs(Bucket::Later),
        vec!["medium-someday", "plain-someday"],
        "undated and below the floor is Later, never Next"
    );
    assert_eq!(
        refs(Bucket::Upcoming),
        vec!["high-and-dated"],
        "a dated task is placed by its date, whatever its priority"
    );
}

/// A bucket **names a lifecycle**, so it replaces the lifecycle filter rather
/// than ANDing with it. Without this, `Bucket::Completed` under the default
/// `Open` answers an empty page — a silent nothing on the most obvious request
/// anybody makes of the field.
#[test]
fn a_bucket_decides_the_lifecycle_rather_than_being_filtered_by_it() {
    let zone = FixedZone::new("Europe/Zurich", 2 * 3_600);
    let windows = windows_on("2026-06-15", &zone);

    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            a_marked_task("open-one", "still to do"),
            completed(a_marked_task("done-one", "ticked")),
        ],
    );

    let ask = |bucket: Bucket, lifecycle: LifecycleFilter| {
        task_nodes(
            &p.task_rows(&TaskRowQuery {
                bucket: Some(bucket),
                windows: Some(windows.clone()),
                lifecycle,
                limit: 100,
                ..TaskRowQuery::default()
            })
            .unwrap(),
        )
    };
    for lifecycle in [
        LifecycleFilter::Open,
        LifecycleFilter::Completed,
        LifecycleFilter::All,
    ] {
        assert_eq!(
            ask(Bucket::Completed, lifecycle),
            vec!["done-one"],
            "the Completed bucket answers the same under every lifecycle"
        );
        assert_eq!(ask(Bucket::Later, lifecycle), vec!["open-one"]);
    }
}

/// A bucket with no windows is **refused**, never guessed at. This layer has no
/// timezone database and no clock worth trusting, and a store that invented
/// today's date would be wrong by morning and wrong in another country.
#[test]
fn a_bucket_without_windows_is_refused() {
    let p = Projections::in_memory().unwrap();
    assert!(
        p.task_rows(&TaskRowQuery {
            bucket: Some(Bucket::Today),
            ..tasks()
        })
        .is_err(),
        "a bucket with no windows is a question this layer cannot answer"
    );
}

/// Transposed horizons are refused up front rather than answered as an empty
/// `Upcoming` and a fat `Later` — a wrong answer wearing the shape of a right
/// one is the failure worth spending a check on.
#[test]
fn windows_out_of_order_are_refused_rather_than_answered() {
    let zone = FixedZone::new("Europe/Zurich", 2 * 3_600);
    let good = windows_on("2026-06-15", &zone);

    let mut swapped = good.clone();
    std::mem::swap(&mut swapped.horizon, &mut swapped.upcoming_horizon);
    assert!(swapped.check().is_err(), "days out of order");

    let mut instants = good.clone();
    std::mem::swap(
        &mut instants.horizon_ends_ms,
        &mut instants.upcoming_ends_ms,
    );
    assert!(instants.check().is_err(), "instants out of order");

    let mut malformed = good.clone();
    malformed.today = "2026-6-1".to_owned();
    assert!(malformed.check().is_err(), "a day must be YYYY-MM-DD");

    assert!(good.check().is_ok());
}

/// **The rail's numbers**: five counts per workspace over the whole corpus, the
/// overdue share that tints Today, and every count equal to the length of the
/// list it sits above. A count that came from a different statement than the
/// rows is exactly how a rail comes to say 14 over a list of 11.
#[test]
fn bucket_counts_equal_the_lists_they_sit_above() {
    let mut p = Projections::in_memory().unwrap();
    let corpus = TaskCorpus::medium().plan();
    corpus.write_into(&mut p, 1).unwrap();

    let zone = FixedZone::new("Europe/Zurich", 2 * 3_600);
    let windows = windows_on("2026-06-15", &zone);
    let query = TaskRowQuery {
        include_trashed: true,
        limit: 100,
        ..TaskRowQuery::default()
    };

    let counts = p.task_bucket_counts(&query, &windows).unwrap();
    assert!(
        counts.iter().all(|c| c.count > 0),
        "a bucket nothing matches has no row rather than a zero"
    );

    for bucket in Bucket::ALL {
        let listed = bucket_refs(&p, &windows, bucket);
        let counted: usize = counts
            .iter()
            .filter(|c| c.bucket == bucket)
            .map(|c| c.count)
            .sum();
        assert_eq!(
            counted,
            listed.len(),
            "{bucket:?}: the rail's number and the list under it"
        );
    }
    assert_eq!(
        counts.iter().map(|c| c.count).sum::<usize>(),
        corpus.tasks().filter(|t| !t.cold).count(),
        "the five partition the corpus, so they sum to it"
    );

    // The overdue share is a fact about Today and nowhere else: nothing overdue
    // is placed in another bucket, so every other column's tint is zero.
    for count in &counts {
        if count.bucket == Bucket::Today {
            assert!(count.overdue > 0, "the corpus holds overdue work");
            assert!(count.overdue <= count.count);
        } else {
            assert_eq!(
                count.overdue, 0,
                "{:?} claimed overdue work it cannot hold",
                count.bucket
            );
        }
    }

    // Per workspace, not folded — a warm corpus has two.
    let workspaces: HashSet<WorkspaceId> = counts.iter().map(|c| c.workspace).collect();
    assert_eq!(workspaces.len(), corpus.warm().count());
}

/// The counts are the **corpus**, never the page, and they honour every other
/// filter the rows honour — `task_list_counts`' rule over a second key.
#[test]
fn bucket_counts_cover_the_corpus_and_obey_the_other_filters() {
    let mut p = Projections::in_memory().unwrap();
    let corpus = TaskCorpus::medium().plan();
    corpus.write_into(&mut p, 1).unwrap();

    let zone = FixedZone::new("Europe/Zurich", 2 * 3_600);
    let windows = windows_on("2026-06-15", &zone);

    let paged = TaskRowQuery {
        include_trashed: true,
        limit: 5,
        ..TaskRowQuery::default()
    };
    let whole: usize = p
        .task_bucket_counts(&paged, &windows)
        .unwrap()
        .iter()
        .map(|c| c.count)
        .sum();
    assert!(whole > 100, "the corpus, not the five rows of the page");

    // The priority floor narrows the counts exactly as it narrows the rows.
    let filtered = TaskRowQuery {
        attributes: AttributeRowQuery {
            min_priority: Some(Priority::High),
            ..AttributeRowQuery::default()
        },
        ..paged.clone()
    };
    let narrowed: usize = p
        .task_bucket_counts(&filtered, &windows)
        .unwrap()
        .iter()
        .map(|c| c.count)
        .sum();
    assert_eq!(
        narrowed,
        corpus
            .tasks()
            .filter(|t| !t.cold && matches!(t.priority, Some(Priority::High | Priority::Urgent)))
            .count(),
        "a filter means the same thing to the rail as it does to the list"
    );
    assert!(narrowed < whole);

    // A binned document is counted and flagged, exactly as its rows are.
    let trashed: usize = p
        .task_bucket_counts(&paged, &windows)
        .unwrap()
        .iter()
        .map(|c| c.trashed)
        .sum();
    assert!(trashed > 0, "binned documents are flagged, never dropped");
}

/// Bucket paging is the ordinary keyset paging over a narrowed set: a bucket
/// walked page by page is the same list as the bucket asked for in one go, with
/// nothing repeated and nothing skipped.
#[test]
fn a_bucket_pages_like_any_other_list() {
    let mut p = Projections::in_memory().unwrap();
    let corpus = TaskCorpus::medium().plan();
    corpus.write_into(&mut p, 1).unwrap();

    let zone = FixedZone::new("Europe/Zurich", 2 * 3_600);
    let windows = windows_on("2026-06-15", &zone);

    for bucket in Bucket::ALL {
        let one_go = {
            let page = p
                .task_rows(&TaskRowQuery {
                    bucket: Some(bucket),
                    windows: Some(windows.clone()),
                    include_trashed: true,
                    limit: 10_000,
                    ..TaskRowQuery::default()
                })
                .unwrap();
            let mut refs: Vec<String> = page.rows.iter().map(|r| r.task.clone()).collect();
            refs.sort();
            refs
        };
        let walked = bucket_refs(&p, &windows, bucket);
        let mut unique = walked.clone();
        unique.dedup();
        assert_eq!(walked, unique, "{bucket:?}: a row came back twice");
        assert_eq!(walked, one_go, "{bucket:?}: paging changed the set");
    }
}

// ------------------------------------------------------------- coverage

/// A real encoded vector, because the synthetic `b"rev-1"` strings the rest of
/// this file uses are not evidence for a predicate that decodes.
fn vv(entries: &[(u64, i32)]) -> loro::VersionVector {
    entries.iter().copied().collect()
}

fn projected_at(p: &mut Projections, ws: WorkspaceId, doc: DocumentId, at: &loro::VersionVector) {
    p.replace(&a_projection(ws, doc, &at.encode()), 1, None)
        .unwrap();
}

/// A requirement naming one writer's progress in `at` — the scoped receipt the
/// surfaces now send. `peer` is the writer the test cares about.
fn required(doc: DocumentId, peer: u64, at: &loro::VersionVector) -> Requirement {
    Requirement::new(doc, peer, i64::from(at.get(&peer).copied().unwrap_or(0)))
}

#[test]
fn coverage_is_causal_inclusion_not_equality() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    projected_at(&mut p, ws, doc, &vv(&[(1, 6)]));

    assert!(
        p.covers(&required(doc, 1, &vv(&[(1, 6)]))).unwrap(),
        "equal"
    );
    // The coalescing case: the write was {1:5}, twenty more commits followed,
    // and the projector built the rows from {1:6}. Equality says *stale*
    // forever; inclusion says the rows have the write.
    assert!(
        p.covers(&required(doc, 1, &vv(&[(1, 5)]))).unwrap(),
        "strictly later"
    );
    assert!(
        !p.covers(&required(doc, 1, &vv(&[(1, 7)]))).unwrap(),
        "earlier"
    );
    assert!(
        !p.covers(&required(doc, 2, &vv(&[(2, 1)]))).unwrap(),
        "incomparable"
    );
}

#[test]
fn a_document_nothing_projected_is_uncovered_not_an_error() {
    let p = Projections::in_memory().unwrap();
    assert!(
        !p.covers(&required(DocumentId::new(), 1, &vv(&[(1, 1)])))
            .unwrap()
    );
}

/// The store went backwards — a recovery lost history — and the projection
/// followed it. A wait on the lost write is unsatisfied, which the bounded
/// policy on the other side handles; what this must not do is say *yes*.
#[test]
fn a_projection_that_went_backwards_does_not_cover_the_lost_write() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    projected_at(&mut p, ws, doc, &vv(&[(1, 5)]));
    projected_at(&mut p, ws, doc, &vv(&[(1, 3)]));
    assert!(!p.covers(&required(doc, 1, &vv(&[(1, 5)]))).unwrap());
}

/// A source this build cannot read is settled at a revision, and
/// `staleness` rightly calls that *current* — nothing to do. It is not a
/// projection of readable rows, so as coverage it is *no*.
#[test]
fn a_too_new_source_is_current_for_reconciliation_and_uncovered_for_a_reader() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let at = vv(&[(1, 5)]);
    projected_at(&mut p, ws, doc, &at);
    p.mark_schema_too_new(ws, doc, &at.encode(), 99, 1, 2)
        .unwrap();
    assert_eq!(p.staleness(doc, &at.encode()).unwrap(), Staleness::Current);
    assert!(!p.covers(&required(doc, 1, &at)).unwrap());
}

#[test]
fn a_stored_revision_that_does_not_decode_covers_nothing() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    p.replace(&a_projection(ws, doc, b"rev-1"), 1, None)
        .unwrap();
    assert!(!p.covers(&required(doc, 1, &vv(&[(1, 1)]))).unwrap());
}

/// **A document with many historical writers is still coverable.**
///
/// This is the regression the whole change exists for. A version vector gains
/// an entry per writer a document has ever had, and Loro mints a fresh writer
/// per session — so the old requirement grew without bound and the transport
/// refused it entirely once it passed a length limit. A receipt names one
/// writer's own progress, so 500 historical writers cost nothing.
#[test]
fn a_document_with_hundreds_of_historical_writers_is_still_coverable() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    // The applied state carries 500 writers; ours is one of them.
    let mut entries: Vec<(u64, i32)> = (1..=500u64).map(|peer| (peer, 3)).collect();
    entries.push((900, 12));
    let applied: loro::VersionVector = entries.into_iter().collect();
    projected_at(&mut p, ws, doc, &applied);

    assert!(
        p.covers(&Requirement::new(doc, 900, 12)).unwrap(),
        "our own operations are applied"
    );
    assert!(
        p.covers(&Requirement::new(doc, 900, 5)).unwrap(),
        "and an earlier one of ours, because coalescing means >= not =="
    );
    assert!(
        !p.covers(&Requirement::new(doc, 900, 13)).unwrap(),
        "but not one that has not arrived"
    );
    assert!(
        !p.covers(&Requirement::new(doc, 501, 1)).unwrap(),
        "a writer the applied state has never seen is not covered"
    );
}

/// **Reading a document does not make it uncoverable.**
///
/// Opening a document mints a writing session whether or not it writes, so a
/// document that has merely been *opened* many times accumulates peers in its
/// history. Under the old shape every such open lengthened the requirement;
/// under a receipt they are invisible to it.
#[test]
fn many_read_only_opens_do_not_affect_coverage() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let ours = 42u64;
    let mut with_readers: Vec<(u64, i32)> = vec![(ours, 4)];
    with_readers.extend((1000..1200u64).map(|peer| (peer, 1)));
    projected_at(&mut p, ws, doc, &with_readers.into_iter().collect());

    assert!(
        p.covers(&Requirement::new(doc, ours, 4)).unwrap(),
        "two hundred other sessions in the history change nothing about ours"
    );
}

/// **The first query is uncovered and the second is covered** — the whole
/// point of the wait, end to end through `covers`.
#[test]
fn the_first_query_is_uncovered_and_the_second_is_covered() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let ours = 7u64;
    // The projection is behind: it has three of our operations, we wrote five.
    projected_at(&mut p, ws, doc, &[(ours, 3)].into_iter().collect());
    let want = Requirement::new(doc, ours, 5);
    assert!(!p.covers(&want).unwrap(), "the first ask is behind");

    // The projector catches up.
    projected_at(&mut p, ws, doc, &[(ours, 5)].into_iter().collect());
    assert!(p.covers(&want).unwrap(), "the second ask has it");
}

/// **A receipt is two integers and cannot fail to parse.** What replaced the
/// decode test: the shape that could be malformed is gone, and with it the
/// length limit that refused every query once a document had enough writers.
#[test]
fn a_requirement_is_a_peer_and_a_counter() {
    let doc = DocumentId::new();
    let r = Requirement::new(doc, 7, 5);
    assert_eq!((r.peer, r.counter), (7, 5));
}

/// **The comparison is conservative in the safe direction.** The stored
/// revision is captured before the rows are read, so the rows can be ahead of
/// it — and the answer for a write past the stored revision is *no* even when
/// the rows might already hold it. A false *no* costs one more query; a false
/// *yes* would draw a number known to be wrong.
#[test]
fn coverage_never_claims_more_than_the_stored_revision() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    // Rows written from a fork at {1:6}, stamped with the revision read
    // before the fork, {1:5}.
    projected_at(&mut p, ws, doc, &vv(&[(1, 5)]));
    assert!(!p.covers(&required(doc, 1, &vv(&[(1, 6)]))).unwrap());
}

#[test]
fn one_answer_carries_the_page_the_counts_and_the_coverage() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let at = vv(&[(7, 2)]);
    let mut projection = a_projection(ws, doc, &at.encode());
    projection.nodes = vec![
        a_marked_task("one", "first"),
        attributed(
            "two",
            "second",
            vec![an_attr("d1", "koru.due", "date", "2026-09-01")],
        ),
    ];
    p.replace(&projection, 1, None).unwrap();
    p.set_listing(ws, "Acme", &[a_listed(doc, "Tasks")], 1)
        .unwrap();

    let answer = p
        .task_answer(&tasks(), TaskGrouping::Rows, &[required(doc, 1, &at)])
        .unwrap();
    assert_eq!(answer.page.rows.len(), 2);
    assert!(matches!(answer.groups, TaskGroups::None));
    assert!(answer.covered);

    let answer = p
        .task_answer(
            &tasks(),
            TaskGrouping::Lists { overdue: None },
            &[required(doc, 1, &at), required(DocumentId::new(), 1, &at)],
        )
        .unwrap();
    assert!(matches!(answer.groups, TaskGroups::Lists(_)));
    assert!(
        !answer.covered,
        "one uncovered requirement is an uncovered answer"
    );

    let answer = p.task_answer(&tasks(), TaskGrouping::Rows, &[]).unwrap();
    assert!(answer.covered, "nothing required is covered");
}

#[test]
fn bucket_counts_without_windows_are_refused_inside_the_one_read() {
    let p = Projections::in_memory().unwrap();
    assert!(p.task_answer(&tasks(), TaskGrouping::Buckets, &[]).is_err());
}

// ------------------------------------------- the Completed window (round nine)

/// **The Completed window hides old finished work and nothing else.**
///
/// It is a window rather than an archive: a task finished before the boundary
/// is not listed, is not flagged, and is not touched — it is still in its
/// document, still ticked, still findable by search. What the store must get
/// right is the three edges of that: recent work stays, open work is never
/// affected by it whatever its dates, and a completion the store cannot date
/// is kept rather than assumed old.
#[test]
fn the_completed_window_hides_old_work_and_leaves_open_work_alone() {
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            completed_on(a_marked_task("recent", "Shipped it"), "2026-09-01"),
            completed_on(a_marked_task("old", "Shipped it long ago"), "2026-06-01"),
            // Completed by a build that recorded no day.
            completed(a_marked_task("undated", "Finished, once")),
            a_marked_task("open", "Still to do"),
        ],
    );

    let all = TaskRowQuery {
        lifecycle: LifecycleFilter::All,
        ..tasks()
    };
    assert_eq!(
        task_nodes(&p.task_rows(&all).unwrap()).len(),
        4,
        "with no window, everything"
    );

    let windowed = TaskRowQuery {
        completed_on_or_after: Some("2026-08-07".to_owned()),
        ..all.clone()
    };
    let mut kept = task_nodes(&p.task_rows(&windowed).unwrap());
    kept.sort();
    assert_eq!(
        kept,
        vec!["open".to_owned(), "recent".to_owned(), "undated".to_owned()],
        "the old one falls out; the undated one is kept, and open work is untouched"
    );

    // The boundary is inclusive: finished *on* the day is still inside it.
    let on_the_edge = TaskRowQuery {
        completed_on_or_after: Some("2026-09-01".to_owned()),
        ..all.clone()
    };
    assert!(
        task_nodes(&p.task_rows(&on_the_edge).unwrap()).contains(&"recent".to_owned()),
        "on the boundary is inside it"
    );

    // ...and a window over the open queue changes nothing at all.
    let open_only = TaskRowQuery {
        lifecycle: LifecycleFilter::Open,
        completed_on_or_after: Some("2026-08-07".to_owned()),
        ..tasks()
    };
    assert_eq!(
        task_nodes(&p.task_rows(&open_only).unwrap()),
        vec!["open".to_owned()]
    );
}

/// **What you just ticked stays where you ticked it, and only there.**
///
/// A queue places a row by its due, and rule one of the bucket function is
/// *completed goes to Completed* — so a task vanishes from Today the instant
/// it is ticked. `recently_completed_since` loosens that one rule for rows
/// finished on or after the day it names, so the page keeps them, struck
/// through, while the Completed queue keeps them too. Two pages, two
/// questions, one row in both.
#[test]
fn a_recently_completed_task_stays_in_the_queue_its_due_puts_it_in() {
    let zone = FixedZone::new("UTC", 0);
    let windows = windows_on("2026-09-06", &zone);
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let due_today = |id: &str| {
        attributed(
            id,
            "Ship it",
            vec![
                an_attr(&format!("{id}-task"), "koru.task", "text", "manual"),
                an_attr(&format!("{id}-due"), "koru.due", "date", "2026-09-06"),
            ],
        )
    };
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            due_today("open"),
            completed_on(due_today("ticked"), "2026-09-06"),
            completed_on(due_today("last-week"), "2026-08-28"),
        ],
    );

    let today_page = |recently: Option<&str>| {
        let mut refs = walk_all(
            &p,
            &TaskRowQuery {
                bucket: Some(Bucket::Today),
                windows: Some(windows.clone()),
                recently_completed_since: recently.map(str::to_owned),
                limit: 250,
                ..TaskRowQuery::default()
            },
        );
        refs.sort();
        refs
    };
    assert_eq!(
        today_page(None).len(),
        1,
        "off, Today is the open work in it"
    );
    assert_eq!(
        today_page(Some("2026-09-05")).len(),
        2,
        "on, what was ticked today is still there — and what was ticked last week is not"
    );

    // Completed keeps both of them either way: the switch widens a queue, it
    // does not move a row out of the queue it belongs to.
    assert_eq!(bucket_refs(&p, &windows, Bucket::Completed).len(), 2);
}

/// **A pinned task is on Today *and* on the bucket its due names.**
///
/// The pin is an extra membership, not a move: `koru.today` puts the task on
/// the Today queue, and the due still says when it is owed, so Upcoming keeps
/// it. Getting this wrong in the other direction is the easy mistake — the
/// three other arms are defined as `NOT today`, so widening the shared `today`
/// predicate would have silently taken the task out of Upcoming and made a pin
/// an edit of somebody's deadline.
#[test]
fn a_pinned_task_joins_today_without_leaving_the_bucket_its_due_names() {
    let zone = FixedZone::new("UTC", 0);
    let windows = windows_on("2026-09-06", &zone);
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let dated = |id: &str, due: &str, extra: Vec<_>| {
        let mut attrs = vec![
            an_attr(&format!("{id}-task"), "koru.task", "text", "manual"),
            an_attr(&format!("{id}-due"), "koru.due", "date", due),
        ];
        attrs.extend(extra);
        attributed(id, "Ship it", attrs)
    };
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            // Far enough out to be Upcoming, and pinned.
            dated(
                "pinned",
                "2026-09-26",
                vec![an_attr("pinned-today", "koru.today", "boolean", "true")],
            ),
            // The same date, not pinned: the control.
            dated("plain", "2026-09-26", Vec::new()),
        ],
    );

    let today = bucket_refs(&p, &windows, Bucket::Today);
    let upcoming = bucket_refs(&p, &windows, Bucket::Upcoming);

    assert!(
        today.iter().any(|r| r.contains("pinned")),
        "the pin puts it on Today: {today:?}"
    );
    assert!(
        !today.iter().any(|r| r.contains("plain")),
        "and only the pinned one: {today:?}"
    );
    assert!(
        upcoming.iter().any(|r| r.contains("pinned")),
        "and it is still where its due puts it: {upcoming:?}"
    );
    assert!(
        upcoming.iter().any(|r| r.contains("plain")),
        "beside the control: {upcoming:?}"
    );
}

/// **The rail's Today number counts the pinned rows the page lists.**
///
/// The count query is a partition — one row, one bucket — while the Today
/// *queue* is a union, so the two said different things: Today read 0 over a
/// page showing two rows. Both directions are checked here, including the case
/// that produced the report: a workspace with no naturally-Today row at all,
/// where the partition emits no `today` group to add to.
#[test]
fn the_today_count_includes_pinned_rows_the_page_lists() {
    let zone = FixedZone::new("UTC", 0);
    let windows = windows_on("2026-09-06", &zone);
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    let dated = |id: &str, due: &str, pinned: bool| {
        let mut attrs = vec![
            an_attr(&format!("{id}-task"), "koru.task", "text", "manual"),
            an_attr(&format!("{id}-due"), "koru.due", "date", due),
        ];
        if pinned {
            attrs.push(an_attr(
                &format!("{id}-today"),
                "koru.today",
                "boolean",
                "true",
            ));
        }
        attributed(id, "Ship it", attrs)
    };
    seeded(
        &mut p,
        ws,
        doc,
        vec![
            // Naturally Next, both pinned — and nothing is naturally Today, so
            // the partition emits no `today` group.
            dated("a", "2026-09-10", true),
            dated("b", "2026-09-10", true),
            dated("c", "2026-09-10", false),
        ],
    );

    let query = TaskRowQuery {
        include_trashed: true,
        limit: 100,
        ..TaskRowQuery::default()
    };
    let counts = p.task_bucket_counts(&query, &windows).unwrap();
    let of = |bucket: Bucket| -> usize {
        counts
            .iter()
            .filter(|c| c.bucket == bucket)
            .map(|c| c.count)
            .sum()
    };
    let _ = ws;

    assert_eq!(
        of(Bucket::Today),
        2,
        "the two pinned rows the Today page lists are counted: {counts:?}"
    );
    assert_eq!(
        of(Bucket::Next),
        3,
        "and all three are still counted where their due puts them"
    );
    assert_eq!(
        bucket_refs(&p, &windows, Bucket::Today).len(),
        2,
        "the number matches the page"
    );
}

/// A stored `false` is not a pin. `unpin_from_today` removes the record rather
/// than writing `false`, so this can only arrive from somewhere else — and
/// where it does, the SQL agrees with `TaskFields::pinned_today`, which reads
/// the bool rather than the record's existence.
#[test]
fn a_today_attribute_set_to_false_does_not_pin() {
    let zone = FixedZone::new("UTC", 0);
    let windows = windows_on("2026-09-06", &zone);
    let mut p = Projections::in_memory().unwrap();
    let (ws, doc) = (WorkspaceId::new(), DocumentId::new());
    seeded(
        &mut p,
        ws,
        doc,
        vec![attributed(
            "later",
            "Ship it",
            vec![
                an_attr("later-task", "koru.task", "text", "manual"),
                an_attr("later-due", "koru.due", "date", "2026-09-26"),
                an_attr("later-today", "koru.today", "boolean", "false"),
            ],
        )],
    );

    assert!(
        bucket_refs(&p, &windows, Bucket::Today).is_empty(),
        "false is not a pin"
    );
    assert_eq!(bucket_refs(&p, &windows, Bucket::Upcoming).len(), 1);
}

#[test]
fn reminder_title_prefers_an_authored_node_name() {
    assert_eq!(
        reminder_title(Some("  Diagram  "), Some("ignored"), Some("Document")),
        "Diagram"
    );
}

#[test]
fn reminder_title_never_includes_a_second_line() {
    let line_feed = char::from(10);
    let text = format!("   {line_feed}  First readable line  {line_feed}private body");
    assert_eq!(
        reminder_title(None, Some(&text), Some("Document")),
        "First readable line"
    );
}

#[test]
fn reminder_title_falls_back_to_the_document() {
    assert_eq!(
        reminder_title(Some(" "), Some(""), Some(" Tasks ")),
        "Tasks"
    );
}

#[test]
fn reminder_title_hides_transient_remind_command_text() {
    assert_eq!(
        reminder_title(
            None,
            Some("Snooze me /remind tomorrow at nine"),
            Some("Tasks")
        ),
        "Snooze me"
    );
    assert_eq!(
        reminder_title(None, Some("/remind in two minutes"), Some("Tasks")),
        "Tasks"
    );
    assert_eq!(
        reminder_title(None, Some("Explain /reminders"), Some("Tasks")),
        "Explain /reminders"
    );
}

/// **A file document's address is one indexed row** (D259 §4).
///
/// The alternative a window otherwise faces is fetching and decoding the
/// file's whole snapshot to learn what it points at — per click, on the
/// thread that has to stay ready. A document with no blob at all answers
/// `None`, which is the honest shape: it is not a file.
#[test]
fn the_blob_one_file_document_holds_is_a_single_lookup() {
    let mut p = Projections::in_memory().unwrap();
    let ws = WorkspaceId::new();
    let (file, prose) = (DocumentId::new(), DocumentId::new());

    let mut projection = a_projection(ws, file, b"revision-one");
    projection.nodes[0].blob = Some("b3:cafe".to_owned());
    projection.nodes[0].mime = Some("application/octet-stream".into());
    p.replace(&projection, 1, None).unwrap();
    p.replace(&a_projection(ws, prose, b"revision-two"), 1, None)
        .unwrap();

    assert_eq!(
        p.file_blob(file).unwrap(),
        Some((
            "b3:cafe".to_owned(),
            Some("application/octet-stream".into())
        )),
        "the media type travels with the address, whatever type it is",
    );
    assert_eq!(
        p.file_blob(prose).unwrap(),
        None,
        "a document with no blob is not a file, and nothing is invented for it",
    );
    assert_eq!(p.file_blob(DocumentId::new()).unwrap(), None);
}
