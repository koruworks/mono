// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_read::{PageText, ReaderId, Source};

fn memory() -> (Mirror, koru_sqlite::MemoryName, Connection) {
    let name = koru_sqlite::MemoryName::fresh("readings.db");
    let keeper = koru_sqlite::open_memory(&name).unwrap();
    (Mirror::in_named_memory(&name).unwrap(), name, keeper)
}

fn reading(status: ReadingStatus, pages: &[(u32, &str)]) -> Reading {
    Reading {
        reader: Some(ReaderId::text()),
        generation: GENERATION,
        status: Some(status),
        source: Some(Source::TextLayer),
        pages: pages
            .iter()
            .map(|(n, text)| (*n, PageText::new(*text)))
            .collect(),
        ..Reading::default()
    }
}

fn applied(workspace: WorkspaceId, parent: DocumentId, entries: Vec<(String, Reading)>) -> Applied {
    Applied {
        workspace,
        parent,
        document: koru_store_core::ids::reading_document_id(parent),
        revision: vec![1],
        entries,
    }
}

/// **The route's winner is the index's winner**: of two entries holding the
/// same bytes, the read one beats the partial one, whichever was applied last;
/// another workspace's reading is not this one's; nothing read is `None`.
#[test]
fn the_winner_in_a_workspace_is_the_best_reading_there() {
    let (mut mirror, _name, _keeper) = memory();
    let (ws, other) = (WorkspaceId::new(), WorkspaceId::new());
    let (one, two) = (DocumentId::new(), DocumentId::new());
    let h = "b3:aa".to_owned();
    assert_eq!(mirror.winner_in(ws, &h).unwrap(), None);

    let full = reading(ReadingStatus::Read, &[(1, "first"), (2, "second")]);
    let partial = reading(ReadingStatus::Partial, &[(1, "first")]);
    mirror
        .apply(&applied(ws, two, vec![(h.clone(), full)]))
        .unwrap();
    mirror
        .apply(&applied(ws, one, vec![(h.clone(), partial)]))
        .unwrap();
    let winner = mirror.winner_in(ws, &h).unwrap().expect("a winner");
    assert_eq!(winner.parent, two.to_storage());
    assert_eq!(winner.status.as_deref(), Some("read"));
    assert_eq!(winner.page_count, 2);
    assert_eq!(winner.workspace, ws.to_storage());
    assert_eq!(mirror.winner_in(other, &h).unwrap(), None);
}

/// **The words are held once per workspace and hash**, from the best reading;
/// a parent's rows follow its document; an application that says nothing new
/// changes nothing.
#[test]
fn applying_holds_words_once_and_the_best_reading_wins() {
    let (mut mirror, _name, _keeper) = memory();
    let ws = WorkspaceId::new();
    let (one, two) = (DocumentId::new(), DocumentId::new());
    let h = "b3:aa".to_owned();

    let partial = reading(ReadingStatus::Partial, &[(1, "first only")]);
    let changed = mirror
        .apply(&applied(ws, one, vec![(h.clone(), partial)]))
        .unwrap();
    assert_eq!(changed.hashes, vec![h.clone()]);
    let full = reading(ReadingStatus::Read, &[(1, "first only"), (2, "and second")]);
    mirror
        .apply(&applied(ws, two, vec![(h.clone(), full.clone())]))
        .unwrap();
    let words = mirror.readings_for(ws, std::slice::from_ref(&h)).unwrap();
    assert_eq!(words.len(), 1);
    assert_eq!(
        words[0].pages.len(),
        2,
        "the read entry beat the partial one"
    );
    assert_eq!(words[0].page_count, 2);

    // The worse one again: it does not take the words back.
    let again = mirror
        .apply(&applied(
            ws,
            one,
            vec![(
                h.clone(),
                reading(ReadingStatus::Partial, &[(1, "first only")]),
            )],
        ))
        .unwrap();
    assert!(again.hashes.is_empty(), "nothing moved");
    assert_eq!(
        mirror.readings_for(ws, std::slice::from_ref(&h)).unwrap()[0]
            .pages
            .len(),
        2
    );

    let settled = mirror.settled_in(ws).unwrap();
    assert!(settled.contains(&(two, h.clone())));
    assert!(
        !settled.contains(&(one, h.clone())),
        "a partial is not settled"
    );
    assert_eq!(mirror.settled_anywhere(&h, one).unwrap(), Some((ws, two)));
    assert_eq!(mirror.settled_anywhere(&h, two).unwrap(), None);

    // Another workspace holding the same bytes holds its own words.
    let elsewhere = WorkspaceId::new();
    assert!(
        mirror
            .readings_for(elsewhere, std::slice::from_ref(&h))
            .unwrap()
            .is_empty()
    );

    // Removing the reader of the best entry keeps the words while one holds
    // the hash; removing the last takes them.
    let removed = mirror
        .remove_document(koru_store_core::ids::reading_document_id(two))
        .unwrap()
        .unwrap();
    assert_eq!(removed.parent, two);
    assert_eq!(removed.hashes, vec![h.clone()]);
    assert_eq!(
        mirror
            .readings_for(ws, std::slice::from_ref(&h))
            .unwrap()
            .len(),
        1
    );
    mirror
        .remove_document(koru_store_core::ids::reading_document_id(one))
        .unwrap();
    assert!(mirror.readings_for(ws, &[h]).unwrap().is_empty());
    assert!(mirror.remove_document(DocumentId::new()).unwrap().is_none());
}

/// A hash a document no longer holds leaves its rows, and its words when no
/// other parent holds it.
#[test]
fn a_hash_the_document_dropped_takes_its_rows() {
    let (mut mirror, _name, _keeper) = memory();
    let ws = WorkspaceId::new();
    let page = DocumentId::new();
    let (a, b) = ("b3:aa".to_owned(), "b3:bb".to_owned());
    mirror
        .apply(&applied(
            ws,
            page,
            vec![
                (a.clone(), reading(ReadingStatus::Read, &[(1, "alpha")])),
                (b.clone(), reading(ReadingStatus::Read, &[(1, "beta")])),
            ],
        ))
        .unwrap();
    let changed = mirror
        .apply(&applied(
            ws,
            page,
            vec![(a.clone(), reading(ReadingStatus::Read, &[(1, "alpha")]))],
        ))
        .unwrap();
    assert_eq!(changed.hashes, vec![b.clone()]);
    assert!(mirror.readings_for(ws, &[b]).unwrap().is_empty());
    assert_eq!(mirror.sources_in(ws).unwrap().len(), 1);
}

/// `page_count` is the larger of `pages_done` and the highest page.
#[test]
fn page_count_is_the_larger_of_pages_done_and_the_highest_page() {
    let (mut mirror, _name, _keeper) = memory();
    let ws = WorkspaceId::new();
    let mut capped = reading(ReadingStatus::Partial, &[(3, "third")]);
    capped.reason = Some(Reason::PageCap);
    capped.pages_done = Some(200);
    mirror
        .apply(&applied(
            ws,
            DocumentId::new(),
            vec![("b3:cc".into(), capped)],
        ))
        .unwrap();
    let words = mirror.readings_for(ws, &["b3:cc".to_owned()]).unwrap();
    assert_eq!(words[0].page_count, 200);
    assert_eq!(mirror.counts().unwrap().page_capped, 1);
}

/// Attempts count up while the code stays the same, and start again when it
/// changes; a finished reading clears them.
#[test]
fn attempts_count_up_and_clear() {
    let (mut mirror, _name, _keeper) = memory();
    let first = mirror
        .record_attempt("b3:dd", "text", AttemptCode::Crashed, 1)
        .unwrap();
    assert_eq!(first.attempts, 1);
    let second = mirror
        .record_attempt("b3:dd", "text", AttemptCode::Crashed, 2)
        .unwrap();
    assert_eq!(second.attempts, 2);
    let other = mirror
        .record_attempt("b3:dd", "text", AttemptCode::Unreadable, 3)
        .unwrap();
    assert_eq!(other.attempts, 1);
    assert_eq!(
        mirror.attempts("text").unwrap()["b3:dd"].code,
        AttemptCode::Unreadable
    );
    mirror.clear_attempts("b3:dd").unwrap();
    assert!(mirror.attempts("text").unwrap().is_empty());
}

/// **Every token the schema admits is `koru-read`'s, and nothing else is.**
#[test]
fn the_checked_vocabularies_are_the_reading_types_own() {
    let (mirror, _name, _keeper) = memory();
    let (statuses, reasons, sources) = checked_tokens();
    let insert = |status: Option<&str>, reason: Option<&str>, source: Option<&str>| {
        mirror.connection().execute(
            "INSERT INTO readings VALUES ('w', ?1, 'h', 1, ?2, ?3, ?4, 0)",
            params![DocumentId::new().to_storage(), status, reason, source],
        )
    };
    for status in &statuses {
        insert(Some(status), None, None).unwrap();
    }
    for reason in &reasons {
        insert(None, Some(reason), None).unwrap();
    }
    for source in &sources {
        insert(None, None, Some(source)).unwrap();
    }
    assert!(insert(Some("finished"), None, None).is_err());
    assert!(insert(None, Some("too-big"), None).is_err());
    assert!(insert(None, None, Some("ocr"),).is_err());
    for code in AttemptCode::ALL {
        mirror
            .connection()
            .execute(
                "INSERT INTO read_attempts VALUES (?1, 'text', 1, ?2, 1, 0)",
                params![code.as_str(), code.as_str()],
            )
            .unwrap();
        assert_eq!(AttemptCode::parse(code.as_str()), Some(code));
    }
    assert!(
        mirror
            .connection()
            .execute(
                "INSERT INTO read_attempts VALUES ('x', 'text', 1, 'busy', 1, 0)",
                [],
            )
            .is_err(),
        "busy is about the conversation, not the file"
    );
}

/// **A mirror from another schema version is rebuilt, not migrated.**
#[test]
fn another_schema_version_is_discarded() {
    let (mut mirror, name, _keeper) = memory();
    let ws = WorkspaceId::new();
    mirror
        .apply(&applied(
            ws,
            DocumentId::new(),
            vec![("b3:ee".into(), reading(ReadingStatus::Read, &[(1, "x")]))],
        ))
        .unwrap();
    mirror
        .connection()
        .pragma_update(None, "user_version", READINGS_SCHEMA_VERSION + 1)
        .unwrap();
    drop(mirror);
    let reopened = Mirror::in_named_memory(&name).unwrap();
    assert_eq!(reopened.counts().unwrap().documents, 0);
}

/// **The plans, pinned, on a mirror of realistic size**: 20,000 readings of
/// three pages each over 40 workspaces — what a large archive imported into
/// several workspaces holds. The index's reads, the queue's and the copy's
/// each start from an index; none scans `readings` or `reading_pages`.
#[test]
fn the_mirror_queries_use_their_indexes() {
    let (mirror, _name, _keeper) = memory();
    let workspaces: Vec<String> = (0..40).map(|_| WorkspaceId::new().to_storage()).collect();
    {
        let db = mirror.connection();
        db.execute_batch("BEGIN").unwrap();
        for i in 0..20_000_u32 {
            let ws = &workspaces[(i % 40) as usize];
            let h = format!("b3:{i:064x}");
            db.execute(
                "INSERT INTO readings VALUES (?1, ?2, ?3, 1, 'read', NULL, 'text-layer', 3)",
                params![ws, DocumentId::new().to_storage(), h],
            )
            .unwrap();
            for page in 1..=3 {
                db.execute(
                    "INSERT INTO reading_pages VALUES (?1, ?2, ?3, 'words words')",
                    params![ws, h, page],
                )
                .unwrap();
            }
        }
        db.execute_batch("COMMIT").unwrap();
    }
    let plan = |sql: &str, parameters: Vec<String>| -> String {
        let rows: Vec<String> = mirror
            .connection()
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map(params_from_iter(parameters.iter()), |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        rows.join(" | ")
    };
    let ws = workspaces[0].clone();
    let cases: Vec<(&str, String, Vec<String>)> = vec![
        (
            "readings_for: words",
            reading_pages_sql(2),
            vec![ws.clone(), "b3:aa".into(), "b3:bb".into()],
        ),
        (
            "readings_for: page counts",
            page_count_sql(2),
            vec![ws.clone(), "b3:aa".into(), "b3:bb".into()],
        ),
        (
            "settled_in",
            "SELECT parent_document_id, hash, generation, status, reason
               FROM readings WHERE workspace_id = ?1"
                .into(),
            vec![ws.clone()],
        ),
        (
            "settled_anywhere",
            "SELECT workspace_id, parent_document_id, generation, status, reason, page_count
               FROM readings WHERE hash = ?1 AND parent_document_id <> ?2"
                .into(),
            vec!["b3:aa".into(), "x".into()],
        ),
        (
            "best elsewhere (apply)",
            "SELECT generation, status, reason, page_count FROM readings
              WHERE workspace_id = ?1 AND hash = ?2 AND parent_document_id <> ?3"
                .into(),
            vec![ws.clone(), "b3:aa".into(), "x".into()],
        ),
        (
            "winner_in (GET /v1/readings)",
            WINNER_IN_SQL.into(),
            vec![ws.clone(), "b3:aa".into()],
        ),
        (
            "still held (purge)",
            "SELECT EXISTS (SELECT 1 FROM readings WHERE workspace_id = ?1 AND hash = ?2)".into(),
            vec![ws.clone(), "b3:aa".into()],
        ),
        (
            "sources_in",
            "SELECT document_id, parent_document_id, revision FROM reading_sources
              WHERE workspace_id = ?1"
                .into(),
            vec![ws.clone()],
        ),
    ];
    for (name, sql, parameters) in cases {
        let plan = plan(&sql, parameters);
        println!("{name}: {plan}");
        assert!(
            !plan.contains("SCAN readings") && !plan.contains("SCAN reading_pages"),
            "{name} scans a table: {plan}"
        );
        assert!(plan.contains("USING"), "{name} uses no index: {plan}");
    }
}
