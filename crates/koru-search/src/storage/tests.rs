// SPDX-License-Identifier: MPL-2.0

//! What the index promises about what it stores and what comes back.

use super::*;
use crate::model::{LifecycleFilter, SearchQuery, field};
use koru_projection::model::ListedDocument;
use koru_store_core::ids::DocumentId;

fn listed(document: &DocumentId, title: &str) -> ListedDocument {
    ListedDocument {
        document: *document,
        parent: None,
        ordinal: 0,
        title: title.to_owned(),
        kind: "page".to_owned(),
        mime: None,
        trashed_at: None,
        trashed_by: None,
    }
}

fn fragment(document: &str, node: &str, field: &str, text: &str) -> Fragment {
    Fragment {
        key: format!("node:{document}:{node}:primary"),
        workspace: "w".to_owned(),
        document: Some(document.to_owned()),
        node: Some(node.to_owned()),
        seq: 1,
        kind: "paragraph".to_owned(),
        field: field.to_owned(),
        text: text.to_owned(),
        page: None,
    }
}

/// One workspace, one document, one line of text.
fn an_index(text: &str) -> (SearchIndex, String) {
    let mut index = SearchIndex::in_memory().unwrap();
    let document = DocumentId::new();
    let id = document.to_storage();
    index
        .set_listing("w", "Acme", b"d1", &[listed(&document, "Roadmap")])
        .unwrap();
    index
        .replace_document(
            "w",
            &id,
            &[fragment(&id, "n1", field::BODY, text)],
            &[],
            b"r1",
            10,
        )
        .unwrap();
    index.warm_fuzzy_vocabulary().unwrap();
    (index, id)
}

fn find(index: &SearchIndex, text: &str) -> Vec<crate::model::SearchHit> {
    index
        .query(
            &SearchQuery {
                text: text.into(),
                ..SearchQuery::default()
            },
            &[],
        )
        .unwrap()
        .hits
}

#[test]
fn what_was_indexed_is_findable_with_its_path_assembled() {
    let (index, document) = an_index("the quarterly plan");
    let hits = find(&index, "quarterly");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].document.as_deref(), Some(document.as_str()));
    assert_eq!(hits[0].title, "Roadmap");
    assert_eq!(hits[0].breadcrumb, "Acme / Roadmap");
    assert_eq!(hits[0].workspace_name, "Acme");
}

/// A workspace's own name is matchable, and the fragment that carries it
/// belongs to no document.
#[test]
fn a_workspace_is_findable_by_name() {
    let (index, _) = an_index("nothing to do with it");
    let hits = find(&index, "Acme");
    assert!(hits.iter().any(|h| h.document.is_none()));
}

/// **A rename is one row.** Every affected breadcrumb is right the moment it
/// returns, with no document re-indexed — which is the whole reason a title is
/// not stored on a fragment.
#[test]
fn a_rename_fixes_every_path_without_reindexing_anything() {
    let (mut index, document) = an_index("the quarterly plan");
    let before = index.fragment_count().unwrap();
    let indexed_at: i64 = index
        .connection
        .query_row("SELECT indexed_at_ms FROM documents", [], |r| r.get(0))
        .unwrap();

    let id = DocumentId::parse(&document).unwrap();
    index
        .set_listing("w", "Acme", b"d2", &[listed(&id, "Plans")])
        .unwrap();

    let hits = find(&index, "quarterly");
    assert_eq!(hits[0].title, "Plans");
    assert_eq!(hits[0].breadcrumb, "Acme / Plans");
    assert_eq!(
        index.fragment_count().unwrap(),
        before,
        "a rename rewrote fragments"
    );
    assert_eq!(
        index
            .connection
            .query_row("SELECT indexed_at_ms FROM documents", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        indexed_at,
        "a rename re-indexed the document"
    );
}

/// A renamed *workspace* is the same rule one level up, and its own name
/// fragment has to follow it or the old name stays matchable forever.
#[test]
fn a_renamed_workspace_stops_matching_its_old_name() {
    let (mut index, document) = an_index("the quarterly plan");
    let id = DocumentId::parse(&document).unwrap();
    index
        .set_listing("w", "Beta Ltd", b"d2", &[listed(&id, "Roadmap")])
        .unwrap();

    assert!(find(&index, "Acme").is_empty());
    assert!(!find(&index, "Beta").is_empty());
    assert_eq!(
        find(&index, "quarterly")[0].breadcrumb,
        "Beta Ltd / Roadmap"
    );
}

/// **Binned is not deleted.** A document in the trash keeps its rows and is
/// found under the trash filter, because the bin is a place somebody searches
/// and restores from.
#[test]
fn a_trashed_document_leaves_the_default_answer_and_stays_in_the_bin() {
    let (mut index, document) = an_index("the quarterly plan");
    let id = DocumentId::parse(&document).unwrap();
    let mut binned = listed(&id, "Roadmap");
    binned.trashed_at = Some(1_700);
    binned.trashed_by = Some("Steven".into());
    index.set_listing("w", "Acme", b"d2", &[binned]).unwrap();

    assert!(find(&index, "quarterly").is_empty());

    let hits = index
        .query(
            &SearchQuery {
                text: "quarterly".into(),
                lifecycle: LifecycleFilter::Trash,
                ..SearchQuery::default()
            },
            &[],
        )
        .unwrap()
        .hits;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].trashed_at, Some(1_700));
    assert_eq!(hits[0].trashed_by.as_deref(), Some("Steven"));
    assert_eq!(hits[0].lifecycle, Lifecycle::Trashed);
}

/// A workspace is never in the trash, so a trash-only query must not turn its
/// name into a result.
#[test]
fn the_trash_filter_does_not_answer_with_a_workspace() {
    let (index, _) = an_index("anything");
    let hits = index
        .query(
            &SearchQuery {
                text: "Acme".into(),
                lifecycle: LifecycleFilter::Trash,
                ..SearchQuery::default()
            },
            &[],
        )
        .unwrap()
        .hits;
    assert!(hits.is_empty());
}

/// **Leaving the listing is leaving the index.** *Empty trash* removes the
/// manifest entry while the store keeps the bytes, so the projection keeps
/// the document — and without this its body fragments went on answering
/// *live* queries through the lifecycle join's NULL row.
#[test]
fn a_document_that_leaves_the_listing_leaves_the_index() {
    let (mut index, _) = an_index("the quarterly plan");

    index.set_listing("w", "Acme", b"d2", &[]).unwrap();

    assert!(
        find(&index, "quarterly").is_empty(),
        "body fragments of an unlisted document still answered a live query"
    );
    // Gone, not binned: the trash filter must not answer either.
    let trashed = index
        .query(
            &SearchQuery {
                text: "quarterly".into(),
                lifecycle: LifecycleFilter::Trash,
                ..SearchQuery::default()
            },
            &[],
        )
        .unwrap()
        .hits;
    assert!(trashed.is_empty());
    // The cursor goes too, or the reconcile would believe it is indexed.
    let cursors: i64 = index
        .connection
        .query_row("SELECT COUNT(*) FROM documents", [], |r| r.get(0))
        .unwrap();
    assert_eq!(cursors, 0);
}

/// **The unit of work is one document.** Replacing one must not read or rewrite
/// another.
#[test]
fn replacing_one_document_leaves_another_alone() {
    let mut index = SearchIndex::in_memory().unwrap();
    let (one, two) = (DocumentId::new(), DocumentId::new());
    index
        .set_listing(
            "w",
            "Acme",
            b"d1",
            &[listed(&one, "One"), listed(&two, "Two")],
        )
        .unwrap();
    for (document, text) in [(&one, "alpha"), (&two, "beta")] {
        index
            .replace_document(
                "w",
                &document.to_storage(),
                &[fragment(&document.to_storage(), "n1", field::BODY, text)],
                &[],
                b"r1",
                10,
            )
            .unwrap();
    }

    index
        .replace_document(
            "w",
            &one.to_storage(),
            &[fragment(&one.to_storage(), "n1", field::BODY, "gamma")],
            &[],
            b"r2",
            20,
        )
        .unwrap();

    assert!(find(&index, "alpha").is_empty());
    assert_eq!(find(&index, "gamma").len(), 1);
    assert_eq!(find(&index, "beta").len(), 1, "the other document moved");
}

/// The FTS table is external-content: three triggers are the only thing keeping
/// it in step, and a delete that bypassed them would leave text matchable that
/// nothing holds.
#[test]
fn removing_a_document_removes_it_from_the_full_text_table_too() {
    let (mut index, document) = an_index("the quarterly plan");
    index.remove_document(&document).unwrap();

    assert!(find(&index, "quarterly").is_empty());
    let orphans: i64 = index
        .connection
        .query_row(
            "SELECT count(*) FROM fragment_fts WHERE fragment_fts MATCH 'quarterly'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(orphans, 0, "the full-text index kept a row nothing holds");
    assert!(index.listed(&document).unwrap().is_none());
}

/// `observed_modified_at_ms` answers *when did this document's text last
/// change*, not *when did we last look*. A re-projection that changed nothing
/// must not send a document to the top of a recently-modified sort.
#[test]
fn re_indexing_identical_text_does_not_move_the_observed_time() {
    let (mut index, document) = an_index("the quarterly plan");
    let observed = |index: &SearchIndex| -> i64 {
        index
            .connection
            .query_row("SELECT observed_modified_at_ms FROM documents", [], |r| {
                r.get(0)
            })
            .unwrap()
    };
    let first = observed(&index);

    index
        .replace_document(
            "w",
            &document,
            &[fragment(&document, "n1", field::BODY, "the quarterly plan")],
            &[],
            b"r2",
            999,
        )
        .unwrap();
    let after_same = observed(&index);

    index
        .replace_document(
            "w",
            &document,
            &[fragment(&document, "n1", field::BODY, "something else")],
            &[],
            b"r3",
            2_000,
        )
        .unwrap();

    assert_eq!(after_same, first, "identical text moved the observed time");
    assert_eq!(observed(&index), 2_000);
}

/// **The sort the surface opens on**, and until now the one with no test:
/// the freshest document comes first whatever the words said, and relevance
/// still decides the order inside one document.
#[test]
fn a_recency_sort_puts_the_freshest_document_first() {
    // The older document *is* the word; the fresher one merely says it.
    let (mut index, old) = an_index("quarterly");
    let fresh = DocumentId::new();
    let fresh_id = fresh.to_storage();
    let old_id = DocumentId::parse(&old).unwrap();
    index
        .set_listing(
            "w",
            "Acme",
            b"d2",
            &[listed(&old_id, "Roadmap"), listed(&fresh, "Notes")],
        )
        .unwrap();
    index
        .replace_document(
            "w",
            &fresh_id,
            // A weaker match than the older document's: a substring rather
            // than the whole field.
            &[fragment(
                &fresh_id,
                "n1",
                field::BODY,
                "notes about quarterly things",
            )],
            &[],
            b"r2",
            5_000,
        )
        .unwrap();
    index.warm_fuzzy_vocabulary().unwrap();

    let by_relevance = find(&index, "quarterly");
    let by_recency = index
        .query(
            &SearchQuery {
                text: "quarterly".into(),
                sort: crate::model::SearchSort::ObservedModified,
                ..SearchQuery::default()
            },
            &[],
        )
        .unwrap()
        .hits;

    assert_eq!(by_relevance[0].document.as_deref(), Some(old.as_str()));
    assert_eq!(by_recency[0].document.as_deref(), Some(fresh_id.as_str()));
    assert_eq!(by_recency[0].observed_modified_at_ms, Some(5_000));
}

/// **A workspace's own name belongs to no document**, so a recency sort had
/// nothing to order it by and buried it under every document in the store —
/// which is a search for a workspace by name not finding it.
#[test]
fn a_workspace_hit_is_ranked_by_the_freshest_change_in_it() {
    let mut index = SearchIndex::in_memory().unwrap();
    let stale = DocumentId::new();
    let stale_id = stale.to_storage();
    index
        .set_listing("w", "Quarterly", b"d1", &[listed(&stale, "Roadmap")])
        .unwrap();
    index
        .replace_document(
            "w",
            &stale_id,
            &[fragment(&stale_id, "n1", field::BODY, "quarterly")],
            &[],
            b"r1",
            10,
        )
        .unwrap();

    // A second workspace, changed long after the first, whose text also
    // matches — so the workspace name has something to be buried under.
    let recent = DocumentId::new();
    let recent_id = recent.to_storage();
    index
        .set_listing("w2", "Other", b"d1", &[listed(&recent, "Plans")])
        .unwrap();
    let mut elsewhere = fragment(&recent_id, "n1", field::BODY, "quarterly");
    elsewhere.workspace = "w2".to_owned();
    index
        .replace_document("w2", &recent_id, &[elsewhere], &[], b"r1", 9_000)
        .unwrap();
    index.warm_fuzzy_vocabulary().unwrap();

    let hits = index
        .query(
            &SearchQuery {
                text: "quarterly".into(),
                sort: crate::model::SearchSort::ObservedModified,
                ..SearchQuery::default()
            },
            &[],
        )
        .unwrap()
        .hits;

    let workspace_row = hits
        .iter()
        .position(|h| h.document.is_none())
        .expect("the workspace name did not match");
    let buried_under_its_own_workspace = hits
        .iter()
        .position(|h| h.workspace == "w" && h.document.is_some())
        .expect("the older document did not match");
    assert!(
        workspace_row < buried_under_its_own_workspace,
        "the workspace sorted below the documents it holds"
    );
}

/// Its own cursor, in the same idiom as the projection's: equal is current.
#[test]
fn the_cursor_records_what_was_consumed() {
    let (index, document) = an_index("anything");
    assert_eq!(
        index.cursors().unwrap().get(&document).map(Vec::as_slice),
        Some(b"r1".as_slice())
    );
    assert_eq!(
        index.digests().unwrap().get("w").map(Vec::as_slice),
        Some(b"d1".as_slice())
    );
}

/// A diacritic-insensitive match still highlights the authored bytes.
#[test]
fn a_match_explains_itself_in_the_text_that_was_written() {
    let mut index = SearchIndex::in_memory().unwrap();
    let document = DocumentId::new();
    let id = document.to_storage();
    index
        .set_listing("w", "Acme", b"d1", &[listed(&document, "Menu")])
        .unwrap();
    index
        .replace_document(
            "w",
            &id,
            &[fragment(&id, "n1", field::BODY, "Crème brûlée for eight")],
            &[],
            b"r1",
            10,
        )
        .unwrap();

    let hits = find(&index, "creme");
    let span = &hits[0].spans[0];
    assert_eq!(&hits[0].snippet[span.start..span.end], "Crème");
}

/// An empty query is where this person has been, not nothing.
#[test]
fn an_empty_query_answers_with_recents() {
    let (index, document) = an_index("anything");
    index.record_open("w", Some(&document), None, 5).unwrap();

    let page = index.query(&SearchQuery::default(), &[]).unwrap();
    assert_eq!(page.hits.len(), 1);
    assert_eq!(page.hits[0].title, "Roadmap");
    assert_eq!(page.hits[0].breadcrumb, "Acme / Roadmap");
}

/// A preview reads document order and starts after the title rather than
/// repeating it.
#[test]
fn a_preview_reads_a_window_of_the_document() {
    let mut index = SearchIndex::in_memory().unwrap();
    let document = DocumentId::new();
    let id = document.to_storage();
    index
        .set_listing("w", "Acme", b"d1", &[listed(&document, "Roadmap")])
        .unwrap();
    let mut fragments = Vec::new();
    for i in 1..=3 {
        let mut f = fragment(&id, &format!("n{i}"), field::BODY, &format!("line {i}"));
        f.seq = i as i64;
        fragments.push(f);
    }
    index
        .replace_document("w", &id, &fragments, &[], b"r1", 10)
        .unwrap();

    let preview = index.preview(&id, Some("n2")).unwrap();
    assert_eq!(preview.title, "Roadmap");
    assert_eq!(
        preview
            .blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>(),
        ["line 1", "line 2", "line 3"]
    );
    assert!(preview.blocks.iter().any(|b| b.matched));
}

/// Version 2 was briefly shipped with the pre-MIME listing shape. Leaving the
/// version at 2 when `mime` was added made `CREATE TABLE IF NOT EXISTS` accept
/// the old table, then every listing read failed forever. Version 3 must treat
/// that database as the derived cache it is and rebuild it.
#[test]
fn version_two_without_mime_is_rebuilt_with_the_current_listing_shape() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("search.db");
    {
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE listing (
                    document_id  TEXT PRIMARY KEY,
                    workspace_id TEXT NOT NULL,
                    parent_id    TEXT,
                    ordinal      INTEGER NOT NULL,
                    title        TEXT NOT NULL,
                    kind         TEXT NOT NULL,
                    trashed_at   INTEGER,
                    trashed_by   TEXT
                 );
                 PRAGMA user_version = 2;",
            )
            .unwrap();
    }

    let index = SearchIndex::at(&path).unwrap();
    let version: i64 = index
        .connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    let mut statement = index
        .connection
        .prepare("SELECT name FROM pragma_table_info('listing') ORDER BY cid")
        .unwrap();
    let columns = statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert_eq!(version, SEARCH_SCHEMA_VERSION);
    assert!(columns.iter().any(|column| column == "mime"));
}

/// **Version 5 has no `page` column**, and `CREATE TABLE IF NOT EXISTS` would
/// keep its `fragments` table as it is — every insert would then fail on the
/// column it does not have. Version 6 rebuilds, so the column exists and the
/// old rows, with no reading among them, are gone for the reconcile to refill.
#[test]
fn version_five_without_a_page_column_is_rebuilt_with_one() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE fragments (
                id           INTEGER PRIMARY KEY,
                stable_key   TEXT NOT NULL UNIQUE,
                workspace_id TEXT NOT NULL,
                document_id  TEXT,
                node_id      TEXT,
                seq          INTEGER NOT NULL,
                kind         TEXT NOT NULL,
                field        TEXT NOT NULL,
                text         TEXT NOT NULL,
                normalized   TEXT NOT NULL
             );
             INSERT INTO fragments (stable_key, workspace_id, document_id, node_id, seq, kind,
                                    field, text, normalized)
             VALUES ('node:d:n:primary', 'w', 'd', 'n', 1, 'paragraph', 'body', 'old', 'old');
             PRAGMA user_version = 5;",
        )
        .unwrap();

    let mut index = SearchIndex::over(connection, crate::fuzzy::SharedVocabulary::ready()).unwrap();

    let version: i64 = index
        .connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SEARCH_SCHEMA_VERSION);
    assert_eq!(SEARCH_SCHEMA_VERSION, 6);
    let has_page: bool = index
        .connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('fragments') WHERE name = 'page')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(has_page);
    assert_eq!(index.fragment_count().unwrap(), 0, "the old rows survived");

    // And a page round-trips through the new column onto a hit.
    let document = DocumentId::new();
    let id = document.to_storage();
    index
        .set_listing("w", "Acme", b"d1", &[listed(&document, "manual.pdf")])
        .unwrap();
    let read = Fragment {
        key: format!("node:{id}:n1:read:12"),
        page: Some(12),
        ..fragment(&id, "n1", field::READ, "torque the flange bolts")
    };
    index
        .replace_document("w", &id, &[read], &[], b"r1", 1)
        .unwrap();
    index.warm_fuzzy_vocabulary().unwrap();
    let hits = find(&index, "flange");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].page, Some(12));
    assert_eq!(hits[0].field, field::READ);
}

/// **A schema bump is a rebuild, not a migration.** What comes back is an empty
/// index with no cursors, which is exactly what makes the reconcile refill it.
#[test]
fn an_index_from_another_schema_is_discarded_rather_than_migrated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("search.db");
    {
        let mut index = SearchIndex::at(&path).unwrap();
        let document = DocumentId::new();
        index
            .set_listing("w", "Acme", b"d1", &[listed(&document, "Roadmap")])
            .unwrap();
        index
            .replace_document(
                "w",
                &document.to_storage(),
                &[fragment(&document.to_storage(), "n1", field::BODY, "text")],
                &[],
                b"r1",
                1,
            )
            .unwrap();
        index
            .connection
            .pragma_update(None, "user_version", SEARCH_SCHEMA_VERSION + 1)
            .unwrap();
    }

    let index = SearchIndex::at(&path).unwrap();
    assert_eq!(index.indexed_documents().unwrap(), 0);
    assert_eq!(index.fragment_count().unwrap(), 0);
    assert!(index.cursors().unwrap().is_empty());
}
