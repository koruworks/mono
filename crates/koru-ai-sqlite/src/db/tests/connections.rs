// SPDX-License-Identifier: MPL-2.0
//! ai24 → ai25 (private connections, M5) on the actual migrator and linked
//! SQLite. In-memory, plus one temporary file for the real open path. No
//! worker, Store, delegate, vendor or credential store, and never a real
//! database.
use super::avatar::{ai24, populated_ai23};
use super::library::{foreign_keys_held, snapshot};
use super::routine_controls::foreign_keys;
use super::*;
use koru_ai_core::connections as ai;

/// The populated fixture every earlier step is tested with, migrated to ai24
/// and no further, with a face picked so the ai24 column holds a value.
fn populated_ai24() -> Connection {
    let mut c = populated_ai23();
    koru_sqlite::prepare_with_preflight(&ai24(), &mut c, refuse_superseded_18).unwrap();
    assert_eq!(user_version(&c).unwrap(), 24);
    c.execute("UPDATE bots SET avatar=7", []).unwrap();
    c
}

/// Every object ai25 creates, by type.
const ADDED: &[(&str, &str)] = &[
    ("table", "connections"),
    ("index", "connections_live_account"),
    ("index", "connections_live_name"),
    ("index", "connections_owner"),
    ("table", "connection_items"),
    ("index", "connection_items_live"),
    ("table", "bot_connection_grants"),
    ("index", "bot_connection_grants_connection"),
    ("table", "connection_command_receipts"),
    ("index", "connection_command_open"),
    ("index", "connection_connect_open"),
    ("trigger", "connections_identity"),
    ("trigger", "connections_retain"),
    ("trigger", "connection_items_identity"),
    ("trigger", "connection_items_retain"),
    ("trigger", "connection_command_receipts_identity"),
    ("trigger", "connection_command_receipts_retain"),
];

fn columns(c: &Connection, table: &str) -> Vec<String> {
    c.prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
        .unwrap()
        .query_map([table], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn m5_ai25_migration_keeps_every_populated_ai24_row_and_object_and_adds_four_tables() {
    let mut c = populated_ai24();
    println!("Rust-linked SQLite {}", rusqlite::version());
    let before = snapshot(&c);
    let keys_before = foreign_keys(&c);
    assert!(
        before.values().filter(|(_, rows)| !rows.is_empty()).count() > 10,
        "the fixture is not populated"
    );
    prepare(&mut c).unwrap();
    assert_eq!(SCHEMA_VERSION, 25);
    assert_eq!(user_version(&c).unwrap(), 25);
    let after = snapshot(&c);

    // Everything that existed is there, definition for definition and row
    // for row: no table was rebuilt, rewritten or re-encoded.
    for (name, (sql, rows)) in &before {
        let (new_sql, new_rows) = &after[name];
        assert_eq!(sql, new_sql, "definition changed: {name}");
        assert_eq!(rows, new_rows, "raw rows changed: {name}");
    }
    // What is new is exactly the listed objects (and the automatic indexes
    // SQLite makes for their keys, which `snapshot` leaves out by name).
    let added: std::collections::BTreeSet<&str> = after
        .keys()
        .filter(|name| !before.contains_key(*name))
        .map(String::as_str)
        .collect();
    assert_eq!(
        added,
        ADDED.iter().map(|(_, name)| *name).collect(),
        "ai25 created something the contract does not list"
    );
    for (kind, name) in ADDED {
        let found: String = c
            .query_row(
                "SELECT type FROM sqlite_master WHERE name=?1",
                [name],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found, *kind, "{name}");
    }
    // The new tables start empty, and no existing foreign key moved.
    for table in [
        "connections",
        "connection_items",
        "bot_connection_grants",
        "connection_command_receipts",
    ] {
        assert!(after[table].1.is_empty(), "{table}");
    }
    let keys_after = foreign_keys(&c);
    for (table, keys) in &keys_before {
        assert_eq!(&keys_after[table], keys, "{table}");
    }
    foreign_keys_held(&c);

    // Reopened, it is already current and nothing runs again.
    prepare(&mut c).unwrap();
    assert_eq!(snapshot(&c), after);
}

#[test]
fn m5_ai25_migration_fresh_database_reaches_25_with_no_secret_column() {
    let c = open_in_memory().unwrap();
    assert_eq!(SCHEMA_VERSION, 25);
    assert_eq!(MIGRATIONS.len(), 25);
    assert_eq!(MIGRATIONS[24].to, 25);
    assert_eq!(MIGRATIONS[24].name, "025_ai_bot_connections.sql");
    assert_eq!(user_version(&c).unwrap(), 25);
    // The exact columns, so a column that could hold a secret cannot arrive
    // unnoticed. `credential_slot_digest` is a digest of a slot's name.
    assert_eq!(
        columns(&c, "connections"),
        [
            "connection_id",
            "user_scope_id",
            "owner_device_id",
            "provider",
            "service_origin",
            "name",
            "account_subject_digest",
            "account_label",
            "scopes_json",
            "credential_slot_digest",
            "state",
            "vendor_revoke",
            "revision",
            "connected_at_ms",
            "refreshed_at_ms",
            "disconnected_at_ms",
            "created_at_ms",
            "updated_at_ms"
        ]
    );
    assert_eq!(
        columns(&c, "connection_items"),
        [
            "item_id",
            "connection_id",
            "item_ref",
            "item_ref_digest",
            "identity_digest",
            "title",
            "state",
            "added_at_ms",
            "changed_at_ms",
            "updated_at_ms"
        ]
    );
    assert_eq!(
        columns(&c, "bot_connection_grants"),
        ["bot_id", "connection_id", "operation", "granted_at_ms"]
    );
    assert_eq!(
        columns(&c, "connection_command_receipts"),
        [
            "user_scope_id",
            "request_id",
            "owner_device_id",
            "command_kind",
            "fingerprint",
            "planned_connection_id",
            "connection_id",
            "stage",
            "failure",
            "expected_revision",
            "result_revision",
            "created_at_ms",
            "updated_at_ms"
        ]
    );
    for table in [
        "connections",
        "connection_items",
        "bot_connection_grants",
        "connection_command_receipts",
    ] {
        for column in columns(&c, table) {
            for word in ["token", "secret", "password", "code", "verifier", "content"] {
                assert!(!column.contains(word), "{table}.{column}");
            }
        }
    }
    for (pragma, expected) in [
        ("PRAGMA foreign_keys", 1),
        ("SELECT count(*) FROM pragma_foreign_key_check", 0),
    ] {
        assert_eq!(
            c.query_row(pragma, [], |r| r.get::<_, i64>(0)).unwrap(),
            expected,
            "{pragma}"
        );
    }
}

#[test]
fn m5_ai25_migration_failure_rolls_back_every_table_and_the_stamp() {
    let original = MIGRATIONS[24].sql;
    let after_tables = original.replace(
        "CREATE TRIGGER connections_identity",
        "SELECT * FROM injected_ai25_middle_failure;\nCREATE TRIGGER connections_identity",
    );
    assert_ne!(after_tables, original);
    for (sql, marker) in [
        (after_tables, "injected_ai25_middle_failure"),
        (
            format!("{original}\nSELECT * FROM injected_ai25_late_failure;"),
            "injected_ai25_late_failure",
        ),
    ] {
        let mut c = populated_ai24();
        let before = snapshot(&c);
        let steps = Box::leak(
            vec![Migration {
                to: 25,
                name: "025_failure_fixture.sql",
                sql: Box::leak(sql.into_boxed_str()),
                columns: &[],
            }]
            .into_boxed_slice(),
        );
        let schema = Schema {
            database: crate::DATABASE_FILE,
            understood: 25,
            migrations: steps,
        };
        let error =
            koru_sqlite::prepare_with_preflight(&schema, &mut c, refuse_superseded_18).unwrap_err();
        assert!(error.to_string().contains(marker), "{error}");
        assert_eq!(user_version(&c).unwrap(), 24);
        assert_eq!(snapshot(&c), before);
        foreign_keys_held(&c);
        // The real step still applies to what the failure left.
        prepare(&mut c).unwrap();
        assert_eq!(user_version(&c).unwrap(), 25);
    }
}

#[test]
fn m5_ai25_migration_foreign_key_guard_refuses_a_database_with_a_broken_reference() {
    // O149: a database that already holds a broken reference is not
    // upgraded. Nothing is changed and the version stays 24.
    let mut c = populated_ai24();
    c.execute_batch(
        "PRAGMA foreign_keys=OFF;
         INSERT INTO bot_chats(chat_id,bot_id,purpose,created_at_ms) VALUES('orphan-chat','no-such-bot','additional',1);",
    )
    .unwrap();
    let before = snapshot(&c);
    let error = prepare(&mut c).unwrap_err();
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "{error}"
    );
    assert_eq!(user_version(&c).unwrap(), 24);
    assert_eq!(snapshot(&c), before);
}

#[test]
fn m5_ai25_migration_an_ai24_build_refuses_25_before_preflight_or_configuration() {
    let mut c = populated_ai24();
    prepare(&mut c).unwrap();
    let before = snapshot(&c);
    c.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA synchronous=OFF")
        .unwrap();
    let error =
        koru_sqlite::prepare_with_preflight(&ai24(), &mut c, |_| panic!("old preflight ran"))
            .unwrap_err();
    assert!(matches!(
        error,
        OpenError::SchemaTooNew {
            discovered: 25,
            supported: 24,
            ..
        }
    ));
    assert_eq!(user_version(&c).unwrap(), 25);
    assert_eq!(snapshot(&c), before);
    for pragma in ["PRAGMA foreign_keys", "PRAGMA synchronous"] {
        assert_eq!(c.query_row(pragma, [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }
}

/// The real open path, on a temporary file: a populated ai24 file opened by
/// this build is migrated once, keeps every row, and reopens unchanged.
#[test]
fn m5_ai25_migration_through_the_real_open_of_a_populated_ai24_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ai.db");
    let before = {
        let mut c = Connection::open(&path).unwrap();
        koru_sqlite::prepare_with_preflight(&ai24(), &mut c, refuse_superseded_18).unwrap();
        c.execute_batch(
            "INSERT INTO chats(id,title,current_provider,current_model,created_at_ms,updated_at_ms) VALUES('chat','kept','agent-codex','default',1,2);
             INSERT INTO bots(bot_id,user_scope_id,home_workspace_id,owner_device_id,name,role_workspace_id,role_document_id,working_folder,provider,model,reasoning,revision,policy_revision,access,created_at_ms,updated_at_ms,avatar) VALUES('bot','scope','home','device','kept','home','role','folder','agent-codex','default','auto',1,1,'default',1,2,11);
             INSERT INTO bot_chats(chat_id,bot_id,purpose,created_at_ms) VALUES('chat','bot','main',1);",
        )
        .unwrap();
        assert_eq!(user_version(&c).unwrap(), 24);
        snapshot(&c)
    };
    let mut first = None;
    for _ in 0..2 {
        let c = open(&path).unwrap();
        assert_eq!(user_version(&c).unwrap(), 25);
        let after = snapshot(&c);
        for (name, (sql, rows)) in &before {
            assert_eq!(sql, &after[name].0, "definition changed: {name}");
            assert_eq!(rows, &after[name].1, "raw rows changed: {name}");
        }
        assert!(after.contains_key("connections"));
        // The second open finds exactly what the first one left.
        assert_eq!(*first.get_or_insert_with(|| after.clone()), after);
    }
    // A connection written after the migration is kept across a reopen.
    {
        let c = open(&path).unwrap();
        c.execute_batch(&connection_row("c1", "connected", "NULL", "NULL"))
            .unwrap();
    }
    let c = open(&path).unwrap();
    assert_eq!(
        c.query_row("SELECT state FROM connections", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "connected"
    );
}

fn pad(id: &str) -> String {
    format!("{id:0>36}")
}

/// One valid `connections` row; `id` is padded to 36 characters and also
/// makes the account and the name distinct.
fn connection_row(id: &str, state: &str, revoke: &str, disconnected: &str) -> String {
    format!(
        "INSERT INTO connections(connection_id,user_scope_id,owner_device_id,provider,service_origin,name,account_subject_digest,account_label,scopes_json,credential_slot_digest,state,vendor_revoke,revision,connected_at_ms,disconnected_at_ms,created_at_ms,updated_at_ms)
         VALUES('{}','{}','{}','{}','https://vendor.example','name-{id}',CAST('{}' AS BLOB),'label','[]',zeroblob(32),'{state}',{revoke},1,1,{disconnected},1,1);",
        pad(id),
        pad("scope"),
        pad("device"),
        ai::vendors::QUALIFIED[0],
        format!("{id:x>32}")
    )
}

fn migrated() -> Connection {
    let c = open_in_memory().unwrap();
    c.execute_batch(&connection_row("c1", "connected", "NULL", "NULL"))
        .unwrap();
    c
}

fn refused(c: &Connection, sql: &str, why: &str) {
    let error = c.execute_batch(sql).expect_err(&format!("accepted: {sql}"));
    assert!(error.to_string().contains(why), "{why}: {error}\n{sql}");
}

#[test]
fn m5_connection_rows_checks_unique_indexes_and_the_fixed_name() {
    let c = migrated();
    // A provider outside the closed list, a state outside it, a short id.
    refused(
        &c,
        &connection_row("c2", "connected", "NULL", "NULL")
            .replace(ai::vendors::QUALIFIED[0], "fake"),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &connection_row("c2", "paused", "NULL", "NULL"),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &connection_row("c2", "connected", "NULL", "NULL").replace(&pad("c2"), "short"),
        "CHECK constraint failed",
    );
    // A digest that is not 32 bytes of blob; scopes that are not JSON.
    refused(
        &c,
        &connection_row("c2", "connected", "NULL", "NULL").replace("zeroblob(32)", "zeroblob(31)"),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &connection_row("c2", "connected", "NULL", "NULL").replace("zeroblob(32)", "'text'"),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &connection_row("c2", "connected", "NULL", "NULL").replace("'[]'", "'not json'"),
        "CHECK constraint failed",
    );
    // Disconnected needs its time and what the vendor said; nothing else
    // may carry either.
    refused(
        &c,
        &connection_row("c2", "disconnected", "NULL", "NULL"),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &connection_row("c2", "disconnected", "'confirmed'", "NULL"),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &connection_row("c2", "connected", "'confirmed'", "5"),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &connection_row("c2", "disconnected", "'maybe'", "5"),
        "CHECK constraint failed",
    );
    // The same live account twice, and the same live name twice.
    refused(
        &c,
        &connection_row("c2", "connected", "NULL", "NULL")
            .replace(&format!("{:x>32}", "c2"), &format!("{:x>32}", "c1")),
        "UNIQUE constraint failed",
    );
    refused(
        &c,
        &connection_row("c2", "connected", "NULL", "NULL").replace("name-c2", "name-c1"),
        "UNIQUE constraint failed",
    );
    // The name, the account, the slot, the owner and the provider are fixed.
    let id = pad("c1");
    for change in [
        "name='other'",
        "account_subject_digest=zeroblob(32)",
        "credential_slot_digest=CAST('yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy' AS BLOB)",
        "user_scope_id='000000000000000000000000000000other'",
        "owner_device_id='000000000000000000000000000000other'",
        "service_origin='https://other.example'",
        "created_at_ms=9",
        "revision=0",
    ] {
        refused(
            &c,
            &format!("UPDATE connections SET {change} WHERE connection_id='{id}'"),
            "",
        );
    }
    // The label may be refreshed while the connection lives.
    c.execute_batch(&format!(
        "UPDATE connections SET account_label='renamed',revision=2 WHERE connection_id='{id}'"
    ))
    .unwrap();
    // A revision never goes back.
    refused(
        &c,
        &format!("UPDATE connections SET revision=1 WHERE connection_id='{id}'"),
        "immutable connection identity",
    );
    refused(
        &c,
        &format!("DELETE FROM connections WHERE connection_id='{id}'"),
        "retained connection",
    );
}

#[test]
fn m5_connection_rows_the_fence_and_the_tombstone_are_final() {
    let c = migrated();
    let id = pad("c1");
    c.execute_batch(&format!(
        "UPDATE connections SET state='needs_sign_in',revision=2 WHERE connection_id='{id}';
         UPDATE connections SET state='connected',revision=3 WHERE connection_id='{id}';
         UPDATE connections SET state='disconnecting',revision=4 WHERE connection_id='{id}';"
    ))
    .unwrap();
    // From the fence there is no way back.
    for state in ["connected", "needs_sign_in"] {
        refused(
            &c,
            &format!("UPDATE connections SET state='{state}' WHERE connection_id='{id}'"),
            "immutable connection identity",
        );
    }
    c.execute_batch(&format!(
        "UPDATE connections SET state='disconnected',vendor_revoke='unconfirmed',disconnected_at_ms=9,revision=5 WHERE connection_id='{id}'"
    ))
    .unwrap();
    // The tombstone admits no change at all, the label included.
    for change in [
        "state='connected',vendor_revoke=NULL,disconnected_at_ms=NULL",
        "state='disconnecting',vendor_revoke=NULL,disconnected_at_ms=NULL",
        "revision=6",
        "account_label='other'",
        "scopes_json='[\"more\"]'",
        "vendor_revoke='confirmed'",
        "disconnected_at_ms=10",
    ] {
        refused(
            &c,
            &format!("UPDATE connections SET {change} WHERE connection_id='{id}'"),
            "immutable connection identity",
        );
    }
    refused(
        &c,
        &format!("DELETE FROM connections WHERE connection_id='{id}'"),
        "retained connection",
    );
    // The same account and the same name connect again as a new row: the
    // live indexes do not count a tombstone.
    c.execute_batch(
        &connection_row("c2", "connected", "NULL", "NULL")
            .replace(&format!("{:x>32}", "c2"), &format!("{:x>32}", "c1"))
            .replace("name-c2", "name-c1"),
    )
    .unwrap();
}

#[test]
fn m5_connection_rows_items_and_grants_hold_their_keys() {
    let c = migrated();
    let id = pad("c1");
    let item = |item: &str, digest: &str, state: &str, changed: &str| {
        format!(
            "INSERT INTO connection_items(item_id,connection_id,item_ref,item_ref_digest,identity_digest,title,state,added_at_ms,changed_at_ms,updated_at_ms)
             VALUES('{}','{id}','ref',CAST('{digest:y>32}' AS BLOB),zeroblob(32),'title','{state}',1,{changed},1);",
            pad(item)
        )
    };
    c.execute_batch(&item("i1", "a", "available", "NULL"))
        .unwrap();
    // The same vendor reference twice on one connection.
    refused(
        &c,
        &item("i2", "a", "available", "NULL"),
        "UNIQUE constraint failed",
    );
    // A state and its time go together.
    refused(
        &c,
        &item("i2", "b", "changed", "NULL"),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &item("i2", "b", "available", "5"),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &item("i2", "b", "gone", "NULL"),
        "CHECK constraint failed",
    );
    // An item of no connection.
    refused(
        &c,
        &item("i2", "b", "available", "NULL").replace(&id, &pad("nope")),
        "FOREIGN KEY constraint failed",
    );
    let i1 = pad("i1");
    // Its identity may change (the person picked it again); its reference
    // may not.
    c.execute_batch(&format!(
        "UPDATE connection_items SET state='changed',changed_at_ms=3 WHERE item_id='{i1}';
         UPDATE connection_items SET state='available',changed_at_ms=NULL,identity_digest=CAST('zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz' AS BLOB) WHERE item_id='{i1}';"
    ))
    .unwrap();
    for change in [
        "item_ref='other'",
        "item_ref_digest=zeroblob(32)",
        "added_at_ms=2",
    ] {
        refused(
            &c,
            &format!("UPDATE connection_items SET {change} WHERE item_id='{i1}'"),
            "immutable connection item identity",
        );
    }
    refused(
        &c,
        &format!("DELETE FROM connection_items WHERE item_id='{i1}'"),
        "retained connection item",
    );

    // A grant needs its Bot and its connection, and one operation.
    c.execute_batch(
        "INSERT INTO bots(bot_id,user_scope_id,home_workspace_id,owner_device_id,name,role_workspace_id,role_document_id,working_folder,provider,model,reasoning,revision,policy_revision,access,created_at_ms,updated_at_ms) VALUES('bot','scope','home','device','kept','home','role','folder','agent-codex','default','auto',1,1,'default',1,2);",
    )
    .unwrap();
    let grant = |bot: &str, connection: &str, operation: &str| {
        format!(
            "INSERT INTO bot_connection_grants(bot_id,connection_id,operation,granted_at_ms) VALUES('{bot}','{connection}','{operation}',1);"
        )
    };
    c.execute_batch(&grant("bot", &id, "read_item")).unwrap();
    refused(
        &c,
        &grant("bot", &id, "read_item"),
        "UNIQUE constraint failed",
    );
    refused(
        &c,
        &grant("bot", &id, "write_item"),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &grant("nobody", &id, "read_item"),
        "FOREIGN KEY constraint failed",
    );
    refused(
        &c,
        &grant("bot", &pad("nope"), "read_item"),
        "FOREIGN KEY constraint failed",
    );
    // A grant row is the grant: removing access deletes it.
    c.execute_batch("DELETE FROM bot_connection_grants")
        .unwrap();
}

#[test]
fn m5_connection_receipts_checks_open_indexes_and_immutability() {
    let c = migrated();
    let id = pad("c1");
    let receipt = |request: &str,
                   kind: &str,
                   planned: &str,
                   connection: &str,
                   stage: &str,
                   failure: &str,
                   expected: &str,
                   result: &str| {
        format!(
            "INSERT INTO connection_command_receipts(user_scope_id,request_id,owner_device_id,command_kind,fingerprint,planned_connection_id,connection_id,stage,failure,expected_revision,result_revision,created_at_ms,updated_at_ms)
             VALUES('{}','{}','{}','{kind}','{}',{planned},{connection},'{stage}',{failure},{expected},{result},1,1);",
            pad("scope"),
            pad(request),
            pad("device"),
            "a".repeat(64)
        )
    };
    let planned = format!("'{}'", pad("planned"));
    let existing = format!("'{id}'");
    // A connect names the id it will use; nothing else does.
    refused(
        &c,
        &receipt(
            "r0", "connect", "NULL", "NULL", "prepared", "NULL", "NULL", "NULL",
        ),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &receipt(
            "r0",
            "disconnect",
            &planned,
            &existing,
            "prepared",
            "NULL",
            "1",
            "NULL",
        ),
        "CHECK constraint failed",
    );
    // A command on an existing connection names it and the revision it saw.
    refused(
        &c,
        &receipt(
            "r0",
            "disconnect",
            "NULL",
            "NULL",
            "prepared",
            "NULL",
            "1",
            "NULL",
        ),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &receipt(
            "r0",
            "add_items",
            "NULL",
            &existing,
            "prepared",
            "NULL",
            "NULL",
            "NULL",
        ),
        "CHECK constraint failed",
    );
    // A failure has a listed reason; a completion has its connection and
    // its resulting revision.
    refused(
        &c,
        &receipt(
            "r0", "connect", &planned, "NULL", "failed", "NULL", "NULL", "NULL",
        ),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &receipt(
            "r0",
            "connect",
            &planned,
            "NULL",
            "failed",
            "'because'",
            "NULL",
            "NULL",
        ),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &receipt(
            "r0",
            "connect",
            &planned,
            "NULL",
            "prepared",
            "'canceled'",
            "NULL",
            "NULL",
        ),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &receipt(
            "r0",
            "connect",
            &planned,
            "NULL",
            "completed",
            "NULL",
            "NULL",
            "1",
        ),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &receipt(
            "r0",
            "connect",
            &planned,
            &existing,
            "completed",
            "NULL",
            "NULL",
            "NULL",
        ),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &receipt(
            "r0", "rename", "NULL", &existing, "prepared", "NULL", "1", "NULL",
        ),
        "CHECK constraint failed",
    );
    // The planned id needs no connection row: it names the slot first.
    c.execute_batch(&receipt(
        "r1", "connect", &planned, "NULL", "prepared", "NULL", "NULL", "NULL",
    ))
    .unwrap();
    // One connect at a time per person.
    refused(
        &c,
        &receipt(
            "r2",
            "connect",
            &format!("'{}'", pad("other")),
            "NULL",
            "prepared",
            "NULL",
            "NULL",
            "NULL",
        ),
        "UNIQUE constraint failed",
    );
    // One unfinished command per connection.
    c.execute_batch(&receipt(
        "r3",
        "add_items",
        "NULL",
        &existing,
        "prepared",
        "NULL",
        "1",
        "NULL",
    ))
    .unwrap();
    refused(
        &c,
        &receipt(
            "r4",
            "disconnect",
            "NULL",
            &existing,
            "prepared",
            "NULL",
            "1",
            "NULL",
        ),
        "UNIQUE constraint failed",
    );
    // The same request twice.
    refused(
        &c,
        &receipt(
            "r1",
            "connect",
            &planned,
            "NULL",
            "failed",
            "'canceled'",
            "NULL",
            "NULL",
        ),
        "UNIQUE constraint failed",
    );
    let r1 = pad("r1");
    for change in [
        "fingerprint='bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'",
        "command_kind='disconnect'",
        "planned_connection_id=NULL",
        "created_at_ms=2",
        "owner_device_id='000000000000000000000000000000other'",
    ] {
        refused(
            &c,
            &format!("UPDATE connection_command_receipts SET {change} WHERE request_id='{r1}'"),
            "",
        );
    }
    // A finished receipt admits no other outcome.
    c.execute_batch(&format!(
        "UPDATE connection_command_receipts SET stage='failed',failure='interrupted',updated_at_ms=5 WHERE request_id='{r1}'"
    ))
    .unwrap();
    for change in [
        "stage='prepared',failure=NULL",
        "failure='canceled'",
        "stage='completed',failure=NULL,connection_id='000000000000000000000000000000000c1',result_revision=1",
    ] {
        refused(
            &c,
            &format!("UPDATE connection_command_receipts SET {change} WHERE request_id='{r1}'"),
            "immutable connection receipt",
        );
    }
    refused(
        &c,
        "DELETE FROM connection_command_receipts",
        "retained connection receipt",
    );
    // A second connect may begin once the first has finished.
    c.execute_batch(&receipt(
        "r5",
        "connect",
        &format!("'{}'", pad("other")),
        "NULL",
        "prepared",
        "NULL",
        "NULL",
        "NULL",
    ))
    .unwrap();
}

/// The closed lists in the migration are the lists Rust decodes: a token
/// added to one side alone fails here.
#[test]
fn m5_ai25_migration_rust_vocabularies_mirror_the_check_lists() {
    let sql: String = MIGRATIONS[24]
        .sql
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let list = |tokens: Vec<&str>| {
        format!(
            "({})",
            tokens
                .iter()
                .map(|token| format!("'{token}'"))
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    let tokens = |all: Vec<&'static str>| list(all);
    for (column, expected) in [
        ("providerIN", list(ai::vendors::QUALIFIED.to_vec())),
        (
            "stateIN",
            tokens(ai::ConnectionState::ALL.iter().map(|v| v.token()).collect()),
        ),
        (
            "vendor_revokeIN",
            tokens(ai::VendorRevoke::ALL.iter().map(|v| v.token()).collect()),
        ),
        (
            "command_kindIN",
            tokens(
                ai::ConnectionCommandKind::ALL
                    .iter()
                    .map(|v| v.token())
                    .collect(),
            ),
        ),
        (
            "stageIN",
            tokens(
                ai::ConnectionReceiptStage::ALL
                    .iter()
                    .map(|v| v.token())
                    .collect(),
            ),
        ),
        (
            "failureIN",
            tokens(
                ai::ConnectionFailure::ALL
                    .iter()
                    .map(|v| v.token())
                    .collect(),
            ),
        ),
    ] {
        assert!(
            sql.contains(&format!("{column}{expected}")),
            "{column}{expected}"
        );
    }
    assert!(sql.contains("stateIN('available','changed')"));
    assert!(sql.contains("operationIN('read_item')"));
}
