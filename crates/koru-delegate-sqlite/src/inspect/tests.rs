// SPDX-License-Identifier: MPL-2.0

//! Read-only, worker-independent, content-free.

use koru_delegate_core::PublicationKind;
use rusqlite::Connection;

use super::*;
use crate::{
    AttemptPhase, RunsDb,
    fixtures::{
        BODY_SENTINEL, OBJECTIVE_SENTINEL, Root, created_run, event, request, running_attempt,
        workspace,
    },
};

#[test]
fn an_absent_or_unmigrated_ledger_is_reported_not_created() {
    let root = Root::new();
    assert_eq!(inspect(&root.db_path(), 5_000), Err(InspectError::Absent));
    assert!(!root.db_path().exists(), "inspecting creates nothing");
    std::fs::create_dir_all(root.db_path().parent().unwrap()).unwrap();
    Connection::open(root.db_path()).unwrap();
    let summary = inspect(&root.db_path(), 5_000).unwrap();
    assert_eq!(summary.schema_version, 0);
    assert_eq!(summary, Summary::default());
}

/// The counts, read with the ledger closed — the worker is stopped — and
/// read again while it is open and holding a write lock.
#[test]
fn counts_are_read_with_the_worker_stopped_and_while_it_writes() {
    let root = Root::new();
    let mut db = root.open();
    let a = created_run(&mut db, 1);
    let b = created_run(&mut db, 2);
    let c = created_run(&mut db, 3);
    db.record_intent(&request(4), workspace(0xa2)).unwrap();
    db.record_intent(&request(5), workspace(0xa2)).unwrap();
    running_attempt(&mut db, a, 1);
    db.open_attempt(b).unwrap();
    let cg = running_attempt(&mut db, c, 3).generation;
    db.advance(c, cg, AttemptPhase::Unknown).unwrap();
    db.enqueue(event(1), a, PublicationKind::Lifecycle, "{}")
        .unwrap();
    root.tick(40_000);
    db.enqueue(event(2), a, PublicationKind::Usage, "{}")
        .unwrap();
    db.enqueue(event(3), a, PublicationKind::Usage, "{}")
        .unwrap();
    db.acknowledge(event(3)).unwrap();
    let now = root.now_ms();
    drop(db);

    let expected = Summary {
        schema_version: RUNS_SCHEMA_VERSION,
        live_attempts: 2,
        unknown_attempts: 1,
        pending_intents: 2,
        open_publications: 2,
        oldest_open_publication_age_ms: Some(40_000),
        prepared_schedule_publications: 0,
        ready_schedule_publications: 0,
        oldest_prepared_schedule_publication_age_ms: None,
        oldest_ready_schedule_publication_age_ms: None,
    };
    assert_eq!(inspect(&root.db_path(), now).unwrap(), expected);

    // While the worker holds an open write transaction.
    let mut db = root.open();
    db.arm_crash(crate::CrashPoint {
        op: crate::WriteOp::AdvanceCursor,
        at: crate::Moment::AfterWriteBeforeCommit,
    });
    let _ = db.advance_cursor(a, crate::Projection::Bus, 1);
    let reopened = RunsDb::open(&root.db_path()).unwrap();
    let held = reopened.conn();
    held.execute_batch(
        "BEGIN IMMEDIATE; INSERT INTO projection_cursors VALUES ('x', 'bus', 1, 1);",
    )
    .unwrap();
    assert_eq!(inspect(&root.db_path(), now).unwrap(), expected);
    held.execute_batch("ROLLBACK;").unwrap();

    // Once the unknown attempt is decided and retired, it no longer counts.
    db = root.open();
    db.retire(c, cg).unwrap();
    db.acknowledge(event(1)).unwrap();
    let summary = inspect(&root.db_path(), now).unwrap();
    assert_eq!(summary.unknown_attempts, 0);
    assert_eq!(summary.open_publications, 1);
    assert_eq!(summary.oldest_open_publication_age_ms, Some(0));
}

#[test]
fn a_newer_ledger_is_refused_and_nothing_is_changed() {
    let root = Root::new();
    let path = root.db_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch(&format!(
            "PRAGMA journal_mode = DELETE; PRAGMA user_version = {};",
            RUNS_SCHEMA_VERSION + 1
        ))
        .unwrap();
    assert_eq!(
        inspect(&path, 1),
        Err(InspectError::SchemaTooNew {
            discovered: RUNS_SCHEMA_VERSION + 1,
            supported: RUNS_SCHEMA_VERSION
        })
    );
    let mode: String = Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "delete");
}

/// Counts only: no id, no payload, no body.
#[test]
fn a_summary_carries_no_content() {
    let root = Root::new();
    let mut db = root.open();
    let run = created_run(&mut db, 1);
    db.record_intent(&request(2), workspace(0xa2)).unwrap();
    db.enqueue(
        event(1),
        run,
        PublicationKind::Lifecycle,
        &format!("{{\"s\":\"{BODY_SENTINEL}\"}}"),
    )
    .unwrap();
    let schedule = crate::SchedulePublicationRef {
        schedule: crate::fixtures::schedule(1),
        workspace: workspace(0xa2),
        document: crate::fixtures::document(0xa3),
    };
    db.prepare_schedule_publication(
        event(2),
        schedule,
        crate::SchedulePublicationFact::Created { revision: 1 },
        &format!("{{\"s\":\"{BODY_SENTINEL}\"}}"),
    )
    .unwrap();
    drop(db);
    let rendered = format!("{:#?}", inspect(&root.db_path(), 9).unwrap());
    assert!(!rendered.contains(OBJECTIVE_SENTINEL));
    assert!(!rendered.contains(BODY_SENTINEL));
    assert!(!rendered.contains("00000000-0000-4000"), "not even an id");
}

#[test]
fn schedule_publication_diagnostics_report_only_state_count_and_age() {
    let root = Root::new();
    let mut db = root.open();
    let schedule = crate::SchedulePublicationRef {
        schedule: crate::fixtures::schedule(1),
        workspace: workspace(0xa2),
        document: crate::fixtures::document(0xa3),
    };
    db.prepare_schedule_publication(
        event(1),
        schedule,
        crate::SchedulePublicationFact::Created { revision: 1 },
        "{}",
    )
    .unwrap();
    root.tick(10);
    db.prepare_schedule_publication(
        event(2),
        schedule,
        crate::SchedulePublicationFact::Updated { revision: 2 },
        "{}",
    )
    .unwrap();
    db.mark_schedule_publication_ready(event(2)).unwrap();
    root.tick(20);
    let summary = inspect(&root.db_path(), root.now_ms()).unwrap();
    assert_eq!(summary.prepared_schedule_publications, 1);
    assert_eq!(summary.ready_schedule_publications, 1);
    assert_eq!(
        summary.oldest_prepared_schedule_publication_age_ms,
        Some(30)
    );
    assert_eq!(summary.oldest_ready_schedule_publication_age_ms, Some(20));
}
