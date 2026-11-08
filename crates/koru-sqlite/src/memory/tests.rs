// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn two_connections_to_one_name_share_one_database() {
    let name = MemoryName::fresh("shared");
    let writer = open_memory(&name).unwrap();
    let reader = open_memory(&name).unwrap();
    writer
        .execute_batch("CREATE TABLE t (v INTEGER); INSERT INTO t VALUES (7);")
        .unwrap();
    let v: i64 = reader
        .query_row("SELECT v FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, 7);
}

#[test]
fn the_database_lives_while_a_connection_holds_it_and_not_after() {
    let name = MemoryName::fresh("lifetime");
    let keeper = open_memory(&name).unwrap();
    keeper.execute_batch("CREATE TABLE t (v INTEGER);").unwrap();
    let again = open_memory(&name).unwrap();
    assert!(
        again.prepare("SELECT v FROM t").is_ok(),
        "kept by the keeper"
    );
    drop(again);
    drop(keeper);
    let fresh = open_memory(&name).unwrap();
    assert!(
        fresh.prepare("SELECT v FROM t").is_err(),
        "the last connection took the database with it"
    );
}

#[test]
fn names_are_never_shared_by_accident() {
    let a = MemoryName::fresh("same");
    let b = MemoryName::fresh("same");
    assert_ne!(a, b);
    let first = open_memory(&a).unwrap();
    first.execute_batch("CREATE TABLE t (v INTEGER);").unwrap();
    let second = open_memory(&b).unwrap();
    assert!(second.prepare("SELECT v FROM t").is_err());
}

#[test]
fn a_label_cannot_reach_the_uri() {
    let name = MemoryName::fresh("a?vfs=unix&mode=rw#x/../y");
    assert!(!name.as_str().contains(['?', '&', '#', '=']));
    assert!(name.as_str().starts_with('/'));
    open_memory(&name).unwrap();
}

/// No journal, WAL or temporary file can exist beside it, whatever a caller's
/// own pragmas ask for: what a composition promising a read-only filesystem
/// rests on.
#[test]
fn nothing_beside_the_database_is_a_file_even_after_asking_for_wal() {
    let name = MemoryName::fresh("pragmas");
    let connection = open_memory(&name).unwrap();
    crate::configure("memory.db", &connection).unwrap();
    let _ = connection.pragma_update(None, "journal_mode", "WAL");
    connection
        .execute_batch("CREATE TABLE t (v INTEGER); INSERT INTO t VALUES (1);")
        .unwrap();
    assert!(is_memory_only(&connection).unwrap());
}

#[test]
fn a_plain_connection_is_not_reported_as_memory_only() {
    // `:memory:` keeps its data in memory but its temporary store follows the
    // build default, which may be a file.
    let connection = Connection::open_in_memory().unwrap();
    connection
        .pragma_update(None, "temp_store", "FILE")
        .unwrap();
    assert!(!is_memory_only(&connection).unwrap());
}

/// WAL's property, which the code reading these databases relies on: a
/// writer commits while a reader is in the middle of a read transaction.
#[test]
fn a_writer_is_never_blocked_by_a_reader() {
    let name = MemoryName::fresh("writing");
    let writer = open_memory(&name).unwrap();
    writer
        .execute_batch("CREATE TABLE t (v INTEGER); INSERT INTO t VALUES (1);")
        .unwrap();
    let reader = open_memory(&name).unwrap();
    reader.execute_batch("BEGIN; SELECT * FROM t;").unwrap();
    writer.execute("INSERT INTO t VALUES (2)", []).unwrap();
    reader.execute_batch("COMMIT;").unwrap();
    let n: i64 = reader
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2);
}

/// And a reader reads while a writer holds an open transaction — seeing it,
/// which is the price `read_uncommitted` names.
#[test]
fn a_reader_is_never_blocked_by_a_writer() {
    let name = MemoryName::fresh("reading");
    let writer = open_memory(&name).unwrap();
    writer
        .execute_batch("CREATE TABLE t (v INTEGER); INSERT INTO t VALUES (1);")
        .unwrap();
    let reader = open_memory(&name).unwrap();
    writer
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO t VALUES (2);")
        .unwrap();
    let n: i64 = reader
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2, "uncommitted, and not blocked");
    writer.execute_batch("ROLLBACK;").unwrap();
}

/// A thread holding a read on one connection reads through another while a
/// writer is mid-transaction: the pattern that deadlocked under rollback
/// locking.
#[test]
fn a_read_held_across_a_second_connection_never_waits() {
    let name = MemoryName::fresh("nested");
    let writer = open_memory(&name).unwrap();
    writer
        .execute_batch("CREATE TABLE t (v INTEGER); INSERT INTO t VALUES (1), (2);")
        .unwrap();
    let first = open_memory(&name).unwrap();
    let second = open_memory(&name).unwrap();
    let mut rows = first.prepare("SELECT v FROM t").unwrap();
    let mut rows = rows.query([]).unwrap();
    rows.next().unwrap();
    writer
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO t VALUES (3);")
        .unwrap();
    let n: i64 = second
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 3);
    writer.execute_batch("COMMIT;").unwrap();
}
