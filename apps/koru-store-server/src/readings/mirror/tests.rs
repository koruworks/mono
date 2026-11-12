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
            "outcomes_in (the queue, OCR Plan 3.1: with page_count; 3.2: source)",
            "SELECT parent_document_id, hash, generation, status, reason, page_count, source
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

/// **The siblings' query, pinned on a mirror of realistic size** (OCR Plan
/// 3.1): 20,000 readings over 40 workspaces, a tenth of them the same bytes
/// in a second workspace. Finding the hashes held twice has to visit every
/// hash once — that is the question — so it walks the narrow covering index
/// `readings_hash`, never the table, and builds no temporary B-tree; the rows
/// of each shared hash are then looked up through the same index.
#[test]
fn the_siblings_query_walks_the_hash_index_and_never_the_table() {
    let (mirror, _name, _keeper) = memory();
    let workspaces: Vec<String> = (0..40).map(|_| WorkspaceId::new().to_storage()).collect();
    {
        let db = mirror.connection();
        db.execute_batch("BEGIN").unwrap();
        for i in 0..20_000_u32 {
            let ws = &workspaces[(i % 40) as usize];
            // Every tenth file's bytes are also in the next workspace.
            let h = format!("b3:{:064x}", if i % 10 == 1 { i - 1 } else { i });
            db.execute(
                "INSERT INTO readings VALUES (?1, ?2, ?3, 1, 'read', NULL, 'text-layer', 3)",
                params![ws, DocumentId::new().to_storage(), h],
            )
            .unwrap();
        }
        // No `ANALYZE`: `readings.db` never runs one, and a planner with no
        // statistics is the one that chose wrong before (ROADMAP §3).
        db.execute_batch("COMMIT").unwrap();
    }
    let rows: Vec<String> = mirror
        .connection()
        .prepare(&format!("EXPLAIN QUERY PLAN {FULLER_SQL}"))
        .unwrap()
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let plan = rows.join(" | ");
    println!("best_shared: {plan}");
    assert!(
        !plan.contains("TEMP B-TREE"),
        "the siblings' query sorts: {plan}"
    );
    for step in &rows {
        assert!(
            !step.starts_with("SCAN readings") || step.contains("COVERING INDEX readings_hash"),
            "the siblings' query scans the table: {plan}"
        );
    }
    assert!(plan.contains("readings_hash"), "{plan}");
    let started = std::time::Instant::now();
    let shared = mirror.best_shared().unwrap();
    println!(
        "best_shared over 20,000 rows: {} shared hashes in {:?}",
        shared.len(),
        started.elapsed()
    );
    assert_eq!(shared.len(), 2_000);
}

/// **The best settled reading of each shared hash, across workspaces**: a
/// read beats a capped partial, a higher generation beats both, a reading
/// still going is not offered, and a hash held once is not listed.
#[test]
fn the_best_shared_reading_is_the_fullest_settled_one_on_the_root() {
    let (mut mirror, _name, _keeper) = memory();
    let (ws, other) = (WorkspaceId::new(), WorkspaceId::new());
    let (shared, going, alone) = ("b3:aa".to_owned(), "b3:bb".to_owned(), "b3:cc".to_owned());
    let mut capped = reading(ReadingStatus::Partial, &[(1, "first")]);
    capped.reason = Some(Reason::PageCap);
    capped.pages_done = Some(200);
    let full = reading(ReadingStatus::Read, &[(1, "first"), (2, "second")]);
    let still = reading(ReadingStatus::Partial, &[(1, "first")]);
    let mut newer = full.clone();
    newer.generation = GENERATION + 1;
    for (workspace, entries) in [
        (
            ws,
            vec![
                (shared.clone(), capped.clone()),
                (going.clone(), still.clone()),
            ],
        ),
        (
            other,
            vec![(shared.clone(), full.clone()), (going.clone(), still)],
        ),
        (ws, vec![(alone.clone(), full.clone())]),
    ] {
        mirror
            .apply(&applied(workspace, DocumentId::new(), entries))
            .unwrap();
    }
    let best = mirror.best_shared().unwrap();
    assert_eq!(best.get(&shared), Some(&rank_of(&full)));
    assert!(
        rank_of(&full) > rank_of(&capped),
        "read beats a capped partial"
    );
    assert!(!best.contains_key(&going), "nothing settled to offer");
    assert!(!best.contains_key(&alone), "held once: no sibling");

    mirror
        .apply(&applied(
            ws,
            DocumentId::new(),
            vec![(shared.clone(), newer.clone())],
        ))
        .unwrap();
    assert_eq!(
        mirror.best_shared().unwrap().get(&shared),
        Some(&rank_of(&newer))
    );
}

// ------------------------------------------------- outcomes (OCR Plan 2.4)

/// **How each entry ended, for the counts a person reads**: read and empty
/// are read, a capped partial is partly read, a failure carries its reason,
/// and a partial still being read is not settled at all.
#[test]
fn outcomes_say_how_each_entry_ended() {
    let (mut mirror, _name, _keeper) = memory();
    let ws = WorkspaceId::new();
    let (read, empty, capped, locked, going) = (
        DocumentId::new(),
        DocumentId::new(),
        DocumentId::new(),
        DocumentId::new(),
        DocumentId::new(),
    );
    let h = "b3:aa".to_owned();
    let mut cap = reading(ReadingStatus::Partial, &[(1, "words")]);
    cap.reason = Some(Reason::PageCap);
    let mut password = reading(ReadingStatus::Failed, &[]);
    password.reason = Some(Reason::Password);
    for (parent, entry) in [
        (read, reading(ReadingStatus::Read, &[(1, "words")])),
        (empty, reading(ReadingStatus::Empty, &[])),
        (capped, cap),
        (locked, password),
        (going, reading(ReadingStatus::Partial, &[(1, "words")])),
    ] {
        mirror
            .apply(&applied(ws, parent, vec![(h.clone(), entry)]))
            .unwrap();
    }
    let outcomes = mirror.outcomes_in(ws).unwrap();
    let of = |parent| outcomes.get(&(parent, h.clone())).unwrap().settled();
    assert_eq!(of(read), Some(Settled::Read));
    assert_eq!(of(empty), Some(Settled::Read), "no words is not a failure");
    assert_eq!(of(capped), Some(Settled::PartlyRead));
    assert_eq!(of(locked), Some(Settled::Failed("password".into())));
    assert_eq!(of(going), None);
    assert!(mirror.outcomes_in(WorkspaceId::new()).unwrap().is_empty());
}

// ------------------------------------------------ requests (OCR Plan 3.2)

/// Version 1's tables exactly as that build created them: no `read_requests`.
const VERSION_1_SCHEMA: &str = "
CREATE TABLE readings (
    workspace_id TEXT NOT NULL, parent_document_id TEXT NOT NULL, hash TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation >= 0),
    status TEXT CHECK (status IN ('read', 'empty', 'partial', 'failed')),
    reason TEXT CHECK (reason IN ('password', 'damaged', 'page-cap')),
    source TEXT CHECK (source IN ('text-layer', 'recognized', 'mixed', 'plain')),
    page_count INTEGER NOT NULL CHECK (page_count >= 0),
    PRIMARY KEY (parent_document_id, hash)
) WITHOUT ROWID;
CREATE INDEX readings_workspace_hash ON readings(workspace_id, hash);
CREATE INDEX readings_hash ON readings(hash);
CREATE TABLE reading_pages (
    workspace_id TEXT NOT NULL, hash TEXT NOT NULL,
    page INTEGER NOT NULL CHECK (page >= 1), text TEXT NOT NULL,
    PRIMARY KEY (workspace_id, hash, page)
) WITHOUT ROWID;
CREATE TABLE reading_sources (
    document_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
    parent_document_id TEXT NOT NULL, revision BLOB NOT NULL
);
CREATE INDEX reading_sources_workspace ON reading_sources(workspace_id);
CREATE TABLE read_attempts (
    hash TEXT NOT NULL, reader TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation >= 0),
    code TEXT NOT NULL, attempts INTEGER NOT NULL CHECK (attempts >= 1),
    last_at INTEGER NOT NULL,
    PRIMARY KEY (hash, reader, generation)
) WITHOUT ROWID;
PRAGMA user_version = 1;
";

/// **A version 1 mirror becomes version 2**: rebuilt, as every bump is — its
/// rows go and the catch-up brings them back from the reading documents —
/// and it now holds `read_requests`.
#[test]
fn a_version_one_mirror_is_rebuilt_at_version_two_with_the_requests_table() {
    assert_eq!(READINGS_SCHEMA_VERSION, 2);
    let name = koru_sqlite::MemoryName::fresh("readings.db");
    let keeper = koru_sqlite::open_memory(&name).unwrap();
    keeper.execute_batch(VERSION_1_SCHEMA).unwrap();
    keeper
        .execute(
            "INSERT INTO readings VALUES ('w', 'p', 'b3:aa', 1, 'read', NULL, 'text-layer', 1)",
            [],
        )
        .unwrap();
    keeper
        .execute(
            "INSERT INTO read_attempts VALUES ('b3:aa', 'text', 1, 'crashed', 1, 0)",
            [],
        )
        .unwrap();
    let had_requests: bool = keeper
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'read_requests')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!had_requests, "version 1 has no requests table");

    let mut mirror = Mirror::in_named_memory(&name).unwrap();
    let version: i64 = mirror
        .connection()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 2);
    assert_eq!(mirror.counts().unwrap().by_status.len(), 0, "rebuilt");
    assert!(
        mirror.attempts("text").unwrap().is_empty(),
        "local state too"
    );
    let ws = WorkspaceId::new();
    assert!(mirror.request(ws, "b3:aa", 5).unwrap());
    assert!(mirror.requested(ws, "b3:aa").unwrap());

    // Opened again at version 2: nothing is rebuilt, the request stands.
    drop(mirror);
    let again = Mirror::in_named_memory(&name).unwrap();
    assert!(again.requested(ws, "b3:aa").unwrap());
}

/// **A request is one row per workspace and hash**: asking twice keeps the
/// first time; they are met oldest first; met, the row goes; another
/// workspace's request is its own.
#[test]
fn requests_are_one_row_each_oldest_first_and_go_when_met() {
    let (mut mirror, _name, _keeper) = memory();
    let (ws, other) = (WorkspaceId::new(), WorkspaceId::new());
    assert!(mirror.request(ws, "b3:bb", 20).unwrap());
    assert!(mirror.request(ws, "b3:aa", 30).unwrap());
    assert!(!mirror.request(ws, "b3:bb", 40).unwrap(), "already asked");
    assert!(mirror.request(other, "b3:aa", 10).unwrap());
    assert_eq!(mirror.requests_in(ws).unwrap(), vec!["b3:bb", "b3:aa"]);
    assert_eq!(
        mirror.all_requests().unwrap(),
        vec![
            (other, "b3:aa".to_owned()),
            (ws, "b3:bb".to_owned()),
            (ws, "b3:aa".to_owned())
        ],
        "oldest first, across workspaces"
    );
    mirror.request_met(ws, "b3:bb").unwrap();
    assert!(!mirror.requested(ws, "b3:bb").unwrap());
    assert!(mirror.requested(other, "b3:aa").unwrap(), "its own");
    assert_eq!(mirror.requests_in(ws).unwrap(), vec!["b3:aa"]);
}

/// **The partly-read items, as ids, by one statement** (for Plan 2.5's list):
/// every capped `partial` on the root with where it stopped, and nothing
/// else — not a read one, not one still being read, not a failure.
#[test]
fn partly_read_lists_the_capped_items_as_ids() {
    let (mut mirror, _name, _keeper) = memory();
    let (ws, other) = (WorkspaceId::new(), WorkspaceId::new());
    let (a, b, c, d) = (
        DocumentId::new(),
        DocumentId::new(),
        DocumentId::new(),
        DocumentId::new(),
    );
    let mut capped = reading(ReadingStatus::Partial, &[(1, "one"), (2, "two")]);
    capped.reason = Some(Reason::PageCap);
    capped.pages_done = Some(200);
    let mut newer = capped.clone();
    newer.generation = GENERATION + 1;
    let mut failed = reading(ReadingStatus::Failed, &[]);
    failed.reason = Some(Reason::Password);
    for (workspace, parent, entries) in [
        (ws, a, vec![("b3:aa".to_owned(), capped.clone())]),
        (
            ws,
            b,
            vec![
                (
                    "b3:bb".to_owned(),
                    reading(ReadingStatus::Read, &[(1, "x")]),
                ),
                (
                    "b3:cc".to_owned(),
                    reading(ReadingStatus::Partial, &[(1, "x")]),
                ),
            ],
        ),
        (other, c, vec![("b3:dd".to_owned(), newer)]),
        (other, d, vec![("b3:ee".to_owned(), failed)]),
    ] {
        mirror.apply(&applied(workspace, parent, entries)).unwrap();
    }
    let mut expected = vec![
        PartlyRead {
            workspace: ws,
            entry: a,
            hash: "b3:aa".into(),
            pages_done: 200,
        },
        PartlyRead {
            workspace: other,
            entry: c,
            hash: "b3:dd".into(),
            pages_done: 200,
        },
    ];
    expected.sort_by_key(|p| p.workspace.to_storage());
    assert_eq!(mirror.partly_read().unwrap(), expected);
}

/// **The requests' and the partly-read list's plans, pinned** on a mirror of
/// realistic size (20,000 readings, 2,000 capped; 500 requests). The
/// requests are read through their primary key. The partly-read list is one
/// pass over `readings` with no sort — the question is "every capped item on
/// the root", and no index narrows it without a schema change this plan does
/// not make — and it stays a few milliseconds at this size.
#[test]
fn the_request_and_partly_read_queries_state_their_plans() {
    let (mut mirror, _name, _keeper) = memory();
    let workspaces: Vec<WorkspaceId> = (0..40).map(|_| WorkspaceId::new()).collect();
    {
        let db = mirror.connection();
        db.execute_batch("BEGIN").unwrap();
        for i in 0..20_000_u32 {
            let ws = workspaces[(i % 40) as usize].to_storage();
            let h = format!("b3:{i:064x}");
            let (status, reason) = if i % 10 == 0 {
                ("partial", Some("page-cap"))
            } else {
                ("read", None)
            };
            db.execute(
                "INSERT INTO readings VALUES (?1, ?2, ?3, 1, ?4, ?5, 'recognized', 200)",
                params![ws, DocumentId::new().to_storage(), h, status, reason],
            )
            .unwrap();
        }
        db.execute_batch("COMMIT").unwrap();
    }
    for i in 0..500_u32 {
        mirror
            .request(
                workspaces[(i % 40) as usize],
                &format!("b3:{i:064x}"),
                i64::from(i),
            )
            .unwrap();
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
    let ws = workspaces[0].to_storage();
    for (name, sql, parameters) in [
        (
            "requested",
            "SELECT EXISTS (SELECT 1 FROM read_requests WHERE workspace_id = ?1 AND hash = ?2)",
            vec![ws.clone(), "b3:aa".to_owned()],
        ),
        (
            "requests_in",
            "SELECT hash FROM read_requests WHERE workspace_id = ?1
              ORDER BY requested_at, hash",
            vec![ws.clone()],
        ),
        (
            "request_met",
            "DELETE FROM read_requests WHERE workspace_id = ?1 AND hash = ?2",
            vec![ws.clone(), "b3:aa".to_owned()],
        ),
    ] {
        let plan = plan(sql, parameters);
        println!("{name}: {plan}");
        assert!(
            plan.contains("USING PRIMARY KEY") || plan.contains("USING INDEX"),
            "{name} does not use the key: {plan}"
        );
        assert!(!plan.contains("SCAN read_requests"), "{name}: {plan}");
    }
    let partly = plan(PARTLY_READ_SQL, vec![GENERATION.to_string()]);
    println!("partly_read: {partly}");
    assert!(!partly.contains("TEMP B-TREE"), "no sort: {partly}");
    let started = std::time::Instant::now();
    let found = mirror.partly_read().unwrap();
    println!(
        "partly_read over 20,000 rows: {} items in {:?}",
        found.len(),
        started.elapsed()
    );
    assert_eq!(found.len(), 2_000);
}

/// The outcome carries the source, so the queue knows whether going on needs
/// the reader.
#[test]
fn an_outcome_says_where_its_words_came_from() {
    let (mut mirror, _name, _keeper) = memory();
    let ws = WorkspaceId::new();
    let parent = DocumentId::new();
    let mut capped = reading(ReadingStatus::Partial, &[(1, "x")]);
    capped.reason = Some(Reason::PageCap);
    capped.source = Some(Source::Recognized);
    mirror
        .apply(&applied(ws, parent, vec![("b3:aa".into(), capped)]))
        .unwrap();
    let outcomes = mirror.outcomes_in(ws).unwrap();
    assert_eq!(
        outcomes
            .get(&(parent, "b3:aa".to_owned()))
            .unwrap()
            .source
            .as_deref(),
        Some("recognized")
    );
}
