// SPDX-License-Identifier: MPL-2.0
//! ai27 → ai28 (a person's Bots on their linked desktops, M8 P8.3) on the
//! actual migrator and linked SQLite. In-memory, plus one temporary file for
//! the real open path. No worker, Store, delegate or provider, and never a
//! real database.
//!
//! Cohort `m8_ai_copies_migration`.
use super::library::{foreign_keys_held, snapshot};
use super::routine_controls::foreign_keys;
use super::team_pause::ai27;
use super::*;

/// The ten tables the step adds: four on the computer that runs the Bots,
/// six on a linked desktop.
const OWNER_TABLES: [&str; 4] = [
    "bot_client_feed",
    "bot_client_feed_boots",
    "bot_client_feed_meta",
    "bot_client_receipts",
];
const CLIENT_TABLES: [&str; 6] = [
    "bot_mirror_bots",
    "bot_mirror_chats",
    "bot_mirror_commands",
    "bot_mirror_messages",
    "bot_mirror_owners",
    "bot_mirror_team",
];

fn pad(id: &str) -> String {
    format!("{id:0>36}")
}

/// The populated fixture every earlier step is tested with, migrated to ai27
/// and no further, with what only an ai27 build could hold and what this
/// step reads: a team pause, messages in Bot conversations and in an
/// ordinary chat, a private Bot conversation, and a conversation one Bot
/// opened in another that its owner has not admitted.
fn populated_ai27() -> Connection {
    let mut c = super::children::populated_ai25();
    koru_sqlite::prepare_with_preflight(&ai27(), &mut c, refuse_superseded_18).unwrap();
    assert_eq!(user_version(&c).unwrap(), 27);
    let request = pad("child-request");
    c.execute_batch(&format!(
        "INSERT INTO bot_team_pause(user_scope_id,owner_device_id,paused,revision,request_id,fingerprint,stage,changed_at_ms,acknowledged_at_ms)
           VALUES('scope','device',1,3,'{team}','{print}','pending',9,NULL);
         INSERT INTO bot_team_pause(user_scope_id,owner_device_id,paused,revision,request_id,fingerprint,stage,changed_at_ms,acknowledged_at_ms)
           VALUES('another-scope','device',0,1,'{team}','{print}','pending',9,NULL);
         INSERT INTO messages(id,chat_id,ordinal,role,state,markdown,created_at_ms) VALUES('m-two-0','chat-two',0,'user','complete','kept text',5);
         INSERT INTO messages(id,chat_id,ordinal,role,state,markdown,created_at_ms) VALUES('m-two-1','chat-two',1,'assistant','pending','partial',6);
         INSERT INTO chats(id,title,current_provider,current_model,created_at_ms,updated_at_ms) VALUES('ordinary','mine','openai','default',1,2);
         INSERT INTO messages(id,chat_id,ordinal,role,state,markdown,created_at_ms) VALUES('m-ordinary','ordinary',0,'user','complete','not a Bot',5);
         INSERT INTO chats(id,title,current_provider,current_model,created_at_ms,updated_at_ms,private) VALUES('chat-private','private','agent-codex','default',7,8,1);
         INSERT INTO bot_chats(chat_id,bot_id,purpose,created_at_ms) VALUES('chat-private','bot-two','additional',7);
         INSERT INTO messages(id,chat_id,ordinal,role,state,markdown,created_at_ms) VALUES('m-private','chat-private',0,'user','complete','private',7);
         INSERT INTO chats(id,title,current_provider,current_model,created_at_ms,updated_at_ms) VALUES('chat-asked','asked','agent-codex','default',8,9);
         INSERT INTO bot_chats(chat_id,bot_id,purpose,created_at_ms) VALUES('chat-asked','bot-two','additional',8);
         INSERT INTO messages(id,chat_id,ordinal,role,state,markdown,created_at_ms) VALUES('m-asked','chat-asked',0,'user','complete','asked',8);
         INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,planned_run_id,captured_config_revision,captured_policy_revision,submission_version,submission_json,created_at_ms,updated_at_ms)
           VALUES('scope','{request}','bot-two','print','start','prepared','{run}',1,1,1,'{{}}',8,8);
         INSERT INTO bot_child_chats(user_scope_id,child_request_id,parent_bot_id,parent_chat_id,parent_run_id,parent_assistant_message_id,ask_key,ordinal,ask_fingerprint,wait_id,chat_id,created_at_ms)
           VALUES('scope','{request}','bot-two','chat-two','{parent}','{message}','{ask}',0,'print','{wait}','chat-asked',8);",
        team = "r".repeat(36),
        print = "f".repeat(64),
        run = pad("child-run"),
        parent = pad("parent-run"),
        message = pad("parent-message"),
        ask = pad("ask"),
        wait = pad("wait"),
    ))
    .unwrap();
    c
}

fn columns(c: &Connection, table: &str) -> Vec<String> {
    c.prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
        .unwrap()
        .query_map([table], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// `(kind, subject, removed)` in `seq` order.
fn markers(c: &Connection) -> Vec<(String, String, i64)> {
    c.prepare("SELECT kind, subject_id, removed FROM bot_client_feed ORDER BY seq")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Messages of Bot conversations that are shown.
fn shown_messages(c: &Connection) -> i64 {
    count(
        c,
        "SELECT count(*) FROM messages m JOIN bot_chats bc ON bc.chat_id=m.chat_id
          WHERE m.chat_id NOT IN ('chat-private','chat-asked')",
    )
}

fn refused(c: &Connection, sql: &str, why: &str) {
    let error = c.execute_batch(sql).expect_err(&format!("accepted: {sql}"));
    assert!(error.to_string().contains(why), "{why}: {error}\n{sql}");
}

#[test]
fn m8_ai_copies_migration_keeps_every_populated_ai27_row_and_marks_every_bot_conversation_and_message()
 {
    let mut c = populated_ai27();
    println!("Rust-linked SQLite {}", rusqlite::version());
    let before = snapshot(&c);
    let keys_before = foreign_keys(&c);
    assert!(
        before.values().filter(|(_, rows)| !rows.is_empty()).count() > 10,
        "the fixture is not populated"
    );
    let bots = before["bots"].1.len() as i64;
    let chats = before["bot_chats"].1.len() as i64;
    assert!(bots >= 2 && chats >= 4);
    let triggers = count(
        &c,
        "SELECT count(*) FROM sqlite_master WHERE type='trigger'",
    );
    let shown = shown_messages(&c);
    assert!(shown >= 2);

    prepare(&mut c).unwrap();
    assert_eq!(SCHEMA_VERSION, 28);
    assert_eq!(user_version(&c).unwrap(), 28);
    let after = snapshot(&c);

    // No table was rebuilt, rewritten or re-encoded, and no column was
    // added: every definition and every raw row is as it was.
    for (name, (sql, rows)) in &before {
        let (new_sql, new_rows) = &after[name];
        assert_eq!(sql, new_sql, "definition changed: {name}");
        assert_eq!(rows, new_rows, "raw rows changed: {name}");
    }
    // What is new is exactly the ten tables, their indexes, and no trigger.
    let added: Vec<&str> = after
        .keys()
        .filter(|name| !before.contains_key(*name))
        .map(String::as_str)
        .collect();
    let mut tables: Vec<&str> = OWNER_TABLES.into_iter().chain(CLIENT_TABLES).collect();
    tables.sort_unstable();
    let added_tables: Vec<&str> = added
        .iter()
        .copied()
        .filter(|name| {
            count(
                &c,
                &format!("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='{name}'"),
            ) == 1
        })
        .collect();
    assert_eq!(added_tables, tables);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM sqlite_master WHERE type='trigger'"
        ),
        triggers,
        "the step added a trigger"
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM temp.sqlite_master WHERE name='bot_ai28_guard'"
        ),
        0
    );

    // Every existing Bot, Bot conversation and message of one has exactly
    // one marker; nothing of an ordinary chat has any.
    assert_eq!(
        count(&c, "SELECT count(*) FROM bot_client_feed WHERE kind='bot'"),
        bots
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM bots b WHERE (SELECT count(*) FROM bot_client_feed f WHERE f.kind='bot' AND f.subject_id=b.bot_id AND f.bot_id=b.bot_id)<>1"
        ),
        0
    );
    assert_eq!(
        count(&c, "SELECT count(*) FROM bot_client_feed WHERE kind='chat'"),
        chats
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM bot_chats bc WHERE (SELECT count(*) FROM bot_client_feed f WHERE f.kind='chat' AND f.subject_id=bc.chat_id AND f.bot_id=bc.bot_id AND f.chat_id=bc.chat_id)<>1"
        ),
        0
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM messages m JOIN bot_chats bc ON bc.chat_id=m.chat_id WHERE (SELECT count(*) FROM bot_client_feed f WHERE f.kind='message' AND f.subject_id=m.id AND f.bot_id=bc.bot_id AND f.chat_id=m.chat_id)<>1"
        ),
        0
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM bot_client_feed WHERE kind='message'"
        ),
        count(
            &c,
            "SELECT count(*) FROM messages m JOIN bot_chats bc ON bc.chat_id=m.chat_id"
        )
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM bot_client_feed WHERE subject_id IN ('ordinary','m-ordinary') OR chat_id='ordinary'"
        ),
        0
    );
    // One team marker for the computer, though two people's rows name it.
    assert_eq!(
        markers(&c)
            .into_iter()
            .filter(|(kind, _, _)| kind == "team_pause")
            .collect::<Vec<_>>(),
        [("team_pause".to_owned(), "device".to_owned(), 0)]
    );
    // Bots first, then conversations, then messages, then the team: a
    // linked desktop meets a Bot before anything of it.
    let order: Vec<String> = markers(&c).into_iter().map(|(kind, _, _)| kind).collect();
    let mut sorted = order.clone();
    sorted.sort_by_key(|kind| match kind.as_str() {
        "bot" => 0,
        "chat" => 1,
        "message" => 2,
        _ => 3,
    });
    assert_eq!(order, sorted);
    // A private conversation, and one a Bot opened in another that was not
    // admitted, are marked as not shown, and so is each message of them.
    let hidden: Vec<(String, String, i64)> = markers(&c)
        .into_iter()
        .filter(|(_, _, removed)| *removed == 1)
        .collect();
    let mut hidden_subjects: Vec<&str> = hidden.iter().map(|(_, id, _)| id.as_str()).collect();
    hidden_subjects.sort_unstable();
    assert_eq!(
        hidden_subjects,
        ["chat-asked", "chat-private", "m-asked", "m-private"]
    );
    assert_eq!(
        count(&c, "SELECT count(*) FROM bot_client_feed WHERE withheld<>0"),
        0
    );
    // The counters are what a count of the markers gives.
    assert_eq!(
        c.query_row(
            "SELECT live_bots, live_chats, live_messages FROM bot_client_feed_meta",
            [],
            |r| Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?
            ))
        )
        .unwrap(),
        (
            count(
                &c,
                "SELECT count(*) FROM bot_client_feed WHERE kind='bot' AND removed=0"
            ),
            count(
                &c,
                "SELECT count(*) FROM bot_client_feed WHERE kind='chat' AND removed=0"
            ),
            count(
                &c,
                "SELECT count(*) FROM bot_client_feed WHERE kind='message' AND removed=0"
            ),
        )
    );
    assert_eq!(
        count(&c, "SELECT live_messages FROM bot_client_feed_meta"),
        shown
    );
    // The feed has an identity, and nothing else was written.
    let feed: String = c
        .query_row("SELECT feed_id FROM bot_client_feed_meta", [], |r| r.get(0))
        .unwrap();
    assert!(feed.len() == 32 && feed.bytes().all(|b| b.is_ascii_hexdigit()));
    for table in ["bot_client_feed_boots", "bot_client_receipts"]
        .into_iter()
        .chain(CLIENT_TABLES)
    {
        assert!(after[table].1.is_empty(), "{table}");
    }
    // No foreign key moved, none is violated.
    let keys_after = foreign_keys(&c);
    for (table, keys) in &keys_before {
        assert_eq!(&keys_after[table], keys, "{table}");
    }
    foreign_keys_held(&c);

    // Reopened, it is already current and nothing runs again: the same
    // feed, the same markers.
    prepare(&mut c).unwrap();
    assert_eq!(snapshot(&c), after);
}

#[test]
fn m8_ai_copies_migration_empty_and_fresh_databases_reach_28_and_the_owner_side_holds_no_content_column()
 {
    let mut empty = koru_sqlite::open_in_memory(&ai27()).unwrap();
    assert_eq!(user_version(&empty).unwrap(), 27);
    prepare(&mut empty).unwrap();
    assert_eq!(user_version(&empty).unwrap(), 28);
    let fresh = open_in_memory().unwrap();
    assert_eq!(MIGRATIONS.len(), 28);
    assert_eq!(MIGRATIONS[27].to, 28);
    assert_eq!(MIGRATIONS[27].name, "028_ai_bot_client_copies.sql");
    assert_eq!(user_version(&fresh).unwrap(), 28);
    // The same schema and rows, apart from each feed's own identity.
    let mut a = snapshot(&empty);
    let mut b = snapshot(&fresh);
    let feed_a = a.get_mut("bot_client_feed_meta").unwrap().1.pop().unwrap();
    let feed_b = b.get_mut("bot_client_feed_meta").unwrap().1.pop().unwrap();
    assert_ne!(feed_a, feed_b, "two databases share a feed identity");
    assert_eq!(a, b);
    for c in [&empty, &fresh] {
        assert_eq!(
            columns(c, "bot_client_feed"),
            [
                "seq",
                "kind",
                "subject_id",
                "bot_id",
                "chat_id",
                "removed",
                "withheld"
            ]
        );
        assert_eq!(
            columns(c, "bot_client_feed_meta"),
            [
                "singleton",
                "feed_id",
                "live_bots",
                "live_chats",
                "live_messages"
            ]
        );
        assert_eq!(
            columns(c, "bot_client_feed_boots"),
            ["ordinal", "boot_id", "start_seq"]
        );
        // No owner-side column can hold a message, a title, a task or a
        // credential. The copy on a linked desktop does hold what the owner
        // sent, in `record_json` and `command_json`, and nowhere else.
        for table in OWNER_TABLES {
            for column in columns(c, table) {
                for word in [
                    "task", "title", "text", "content", "markdown", "json", "token", "secret",
                    "name", "folder", "path",
                ] {
                    assert!(!column.contains(word), "{table}.{column}");
                }
            }
        }
        for table in CLIENT_TABLES {
            for column in columns(c, table) {
                let holds_content = column == "record_json" || column == "command_json";
                for word in ["text", "markdown", "json", "token", "secret", "name"] {
                    assert!(holds_content || !column.contains(word), "{table}.{column}");
                }
            }
        }
        assert_eq!(count(c, "SELECT count(*) FROM bot_client_feed"), 0);
        assert_eq!(count(c, "SELECT count(*) FROM bot_client_feed_meta"), 1);
        assert_eq!(count(c, "PRAGMA foreign_keys"), 1);
        assert_eq!(count(c, "SELECT count(*) FROM pragma_foreign_key_check"), 0);
    }
}

#[test]
fn m8_ai_copies_migration_failure_at_any_point_or_a_failing_guard_leaves_27_and_every_row() {
    let original = MIGRATIONS[27].sql;
    let early = original.replace(
        "CREATE TABLE bot_client_feed_meta",
        "SELECT * FROM injected_ai28_early_failure;\nCREATE TABLE bot_client_feed_meta",
    );
    assert_ne!(early, original);
    // The guard: a backfill that leaves a message of a Bot conversation
    // without a marker is refused by the step itself.
    let unmarked = original.replace(
        "FROM messages m JOIN bot_client_feed f ON f.kind = 'chat' AND f.subject_id = m.chat_id",
        "FROM messages m JOIN bot_client_feed f ON f.kind = 'chat' AND f.subject_id = m.chat_id AND m.id <> 'm-two-0'",
    );
    assert_ne!(unmarked, original);
    for (sql, marker) in [
        (early, "injected_ai28_early_failure"),
        (
            format!("{original}\nSELECT * FROM injected_ai28_late_failure;"),
            "injected_ai28_late_failure",
        ),
        (unmarked, "CHECK constraint failed"),
    ] {
        let mut c = populated_ai27();
        let before = snapshot(&c);
        let steps = Box::leak(
            vec![Migration {
                to: 28,
                name: "028_failure_fixture.sql",
                sql: Box::leak(sql.into_boxed_str()),
                columns: &[],
            }]
            .into_boxed_slice(),
        );
        let schema = Schema {
            database: crate::DATABASE_FILE,
            understood: 28,
            migrations: steps,
        };
        let error =
            koru_sqlite::prepare_with_preflight(&schema, &mut c, refuse_superseded_18).unwrap_err();
        assert!(error.to_string().contains(marker), "{error}");
        // The stamp, the tables and every row are as they were: the step is
        // one transaction.
        assert_eq!(user_version(&c).unwrap(), 27);
        assert_eq!(snapshot(&c), before);
        for table in OWNER_TABLES.into_iter().chain(CLIENT_TABLES) {
            assert_eq!(
                count(
                    &c,
                    &format!("SELECT count(*) FROM sqlite_master WHERE name='{table}'")
                ),
                0,
                "{table}"
            );
        }
        foreign_keys_held(&c);
        // The real step still applies to what the failure left.
        prepare(&mut c).unwrap();
        assert_eq!(user_version(&c).unwrap(), 28);
        assert_eq!(
            count(&c, "SELECT live_messages FROM bot_client_feed_meta"),
            shown_messages(&c)
        );
    }
}

#[test]
fn m8_ai_copies_migration_an_ai27_build_refuses_28_before_preflight_or_configuration() {
    let mut c = populated_ai27();
    prepare(&mut c).unwrap();
    let before = snapshot(&c);
    c.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA synchronous=OFF")
        .unwrap();
    let error =
        koru_sqlite::prepare_with_preflight(&ai27(), &mut c, |_| panic!("old preflight ran"))
            .unwrap_err();
    assert!(matches!(
        error,
        OpenError::SchemaTooNew {
            discovered: 28,
            supported: 27,
            ..
        }
    ));
    assert_eq!(user_version(&c).unwrap(), 28);
    assert_eq!(snapshot(&c), before);
    // This build refuses a later one the same way.
    c.execute_batch("PRAGMA user_version=29").unwrap();
    assert!(matches!(
        prepare(&mut c).unwrap_err(),
        OpenError::SchemaTooNew {
            discovered: 29,
            supported: 28,
            ..
        }
    ));
}

/// The real open path, on a temporary file: a populated ai27 file opened by
/// this build is migrated once, keeps every row, and reopens with the same
/// feed and the same markers.
#[test]
fn m8_ai_copies_migration_through_the_real_open_of_a_populated_ai27_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ai.db");
    let before = {
        let mut c = Connection::open(&path).unwrap();
        koru_sqlite::prepare_with_preflight(&ai27(), &mut c, refuse_superseded_18).unwrap();
        c.execute_batch(
            "INSERT INTO chats(id,title,current_provider,current_model,created_at_ms,updated_at_ms) VALUES('chat','kept','agent-codex','default',1,2);
             INSERT INTO bots(bot_id,user_scope_id,home_workspace_id,owner_device_id,name,role_workspace_id,role_document_id,working_folder,provider,model,reasoning,revision,policy_revision,access,paused,created_at_ms,updated_at_ms,avatar,may_ask) VALUES('bot','scope','home','device','kept','home','role','folder','agent-codex','default','auto',4,2,'default',1,1,2,11,0);
             INSERT INTO bot_chats(chat_id,bot_id,purpose,created_at_ms) VALUES('chat','bot','main',1);
             INSERT INTO messages(id,chat_id,ordinal,role,state,markdown,created_at_ms) VALUES('message','chat',0,'user','complete','kept text',5);",
        )
        .unwrap();
        assert_eq!(user_version(&c).unwrap(), 27);
        snapshot(&c)
    };
    let mut first = None;
    for _ in 0..2 {
        let c = open(&path).unwrap();
        assert_eq!(user_version(&c).unwrap(), 28);
        let after = snapshot(&c);
        for (name, (sql, rows)) in &before {
            assert_eq!(sql, &after[name].0, "definition changed: {name}");
            assert_eq!(rows, &after[name].1, "raw rows changed: {name}");
        }
        assert_eq!(
            markers(&c),
            [
                ("bot".to_owned(), "bot".to_owned(), 0),
                ("chat".to_owned(), "chat".to_owned(), 0),
                ("message".to_owned(), "message".to_owned(), 0),
            ]
        );
        // The second open finds exactly what the first one left, the feed's
        // identity included.
        assert_eq!(*first.get_or_insert_with(|| after.clone()), after);
    }
}

/// The new tables' own keys and checks.
#[test]
fn m8_ai_copies_migration_the_tables_accept_only_well_formed_rows() {
    let c = open_in_memory().unwrap();
    let boot = "b".repeat(32);
    let request = "r".repeat(36);
    let print = "f".repeat(64);
    // One feed identity per database.
    refused(
        &c,
        "INSERT INTO bot_client_feed_meta(singleton, feed_id) VALUES(2, '00000000000000000000000000000000')",
        "CHECK constraint failed",
    );
    refused(
        &c,
        "UPDATE bot_client_feed_meta SET live_bots = -1",
        "CHECK constraint failed",
    );
    // One marker per subject; a known kind; flags that are flags.
    c.execute_batch("INSERT INTO bot_client_feed(kind, subject_id, bot_id) VALUES('bot','b','b')")
        .unwrap();
    for (sql, why) in [
        (
            "INSERT INTO bot_client_feed(kind, subject_id) VALUES('bot','b')",
            "UNIQUE constraint failed",
        ),
        (
            "INSERT INTO bot_client_feed(kind, subject_id) VALUES('routine','x')",
            "CHECK constraint failed",
        ),
        (
            "INSERT INTO bot_client_feed(kind, subject_id, removed) VALUES('chat','x',2)",
            "CHECK constraint failed",
        ),
        (
            "INSERT INTO bot_client_feed(kind, subject_id, withheld) VALUES('chat','x',2)",
            "CHECK constraint failed",
        ),
        (
            "INSERT INTO bot_client_feed_boots(boot_id, start_seq) VALUES('short', 0)",
            "CHECK constraint failed",
        ),
    ] {
        refused(&c, sql, why);
    }
    // A marker's seq is never reused, even after the marker is replaced.
    c.execute_batch(
        "DELETE FROM bot_client_feed WHERE kind='bot' AND subject_id='b';
         INSERT INTO bot_client_feed(kind, subject_id, bot_id) VALUES('bot','b','b');",
    )
    .unwrap();
    assert_eq!(count(&c, "SELECT seq FROM bot_client_feed"), 2);
    c.execute_batch(&format!(
        "INSERT INTO bot_client_feed_boots(boot_id, start_seq) VALUES('{boot}', 2)"
    ))
    .unwrap();
    refused(
        &c,
        &format!("INSERT INTO bot_client_feed_boots(boot_id, start_seq) VALUES('{boot}', 3)"),
        "UNIQUE constraint failed",
    );
    // A receipt is done exactly when it has an outcome.
    c.execute_batch(&format!(
        "INSERT INTO bot_client_receipts(peer_device_id,request_id,fingerprint,command_kind,boot_id,state,created_at_ms,updated_at_ms)
         VALUES('peer','{request}','{print}','pause_bot','{boot}','applying',1,1)"
    ))
    .unwrap();
    for (sql, why) in [
        (
            format!(
                "INSERT INTO bot_client_receipts(peer_device_id,request_id,fingerprint,command_kind,boot_id,state,created_at_ms,updated_at_ms) VALUES('peer','{request}','{print}','pause_bot','{boot}','applying',1,1)"
            ),
            "UNIQUE constraint failed",
        ),
        (
            "UPDATE bot_client_receipts SET state='done'".to_owned(),
            "CHECK constraint failed",
        ),
        (
            "UPDATE bot_client_receipts SET outcome='completed'".to_owned(),
            "CHECK constraint failed",
        ),
        (
            "UPDATE bot_client_receipts SET state='done', outcome='maybe'".to_owned(),
            "CHECK constraint failed",
        ),
    ] {
        refused(&c, &sql, why);
    }
    c.execute_batch("UPDATE bot_client_receipts SET state='done', outcome='unknown'")
        .unwrap();
    // A copy belongs to an owner, holds valid JSON, and goes with its owner.
    c.execute_batch(
        "INSERT INTO bot_mirror_owners(owner_device_id,state,created_at_ms,updated_at_ms) VALUES('owner','never',1,1);
         INSERT INTO bot_mirror_bots(owner_device_id,generation,bot_id,seq,home_workspace_id,lifecycle,revision,record_json) VALUES('owner',0,'b',1,'home','active',1,'{}');
         INSERT INTO bot_mirror_chats(owner_device_id,generation,chat_id,bot_id,seq,purpose,trashed,updated_at_ms,record_json) VALUES('owner',0,'c','b',2,'main',0,1,'{}');
         INSERT INTO bot_mirror_messages(owner_device_id,generation,message_id,chat_id,ordinal,seq,record_json) VALUES('owner',0,'m','c',0,3,'{}');
         INSERT INTO bot_mirror_team(owner_device_id,generation,seq,record_json) VALUES('owner',0,4,'{}');",
    )
    .unwrap();
    for (sql, why) in [
        (
            "INSERT INTO bot_mirror_bots(owner_device_id,generation,bot_id,seq,home_workspace_id,lifecycle,revision,record_json) VALUES('nobody',0,'b',1,'home','active',1,'{}')",
            "FOREIGN KEY constraint failed",
        ),
        (
            "INSERT INTO bot_mirror_bots(owner_device_id,generation,bot_id,seq,home_workspace_id,lifecycle,revision,record_json) VALUES('owner',0,'b2',1,'home','active',1,'not json')",
            "CHECK constraint failed",
        ),
        (
            "INSERT INTO bot_mirror_owners(owner_device_id,state,created_at_ms,updated_at_ms) VALUES('other','kept',1,1)",
            "CHECK constraint failed",
        ),
        (
            "UPDATE bot_mirror_owners SET staging_generation=0, staging_cursor=0",
            "CHECK constraint failed",
        ),
        (
            "UPDATE bot_mirror_owners SET staging_generation=1",
            "CHECK constraint failed",
        ),
    ] {
        refused(&c, sql, why);
    }
    // A command row has no owner key to cascade from: it outlives the copy.
    c.execute_batch(&format!(
        "INSERT INTO bot_mirror_commands(owner_device_id,request_id,fingerprint,command_kind,state,command_json,created_at_ms,updated_at_ms)
         VALUES('owner','{request}','{print}','pause_bot','unconfirmed','{{}}',1,1);
         DELETE FROM bot_mirror_owners WHERE owner_device_id='owner';"
    ))
    .unwrap();
    for table in [
        "bot_mirror_bots",
        "bot_mirror_chats",
        "bot_mirror_messages",
        "bot_mirror_team",
    ] {
        assert_eq!(
            count(&c, &format!("SELECT count(*) FROM {table}")),
            0,
            "{table}"
        );
    }
    assert_eq!(count(&c, "SELECT count(*) FROM bot_mirror_commands"), 1);
}
