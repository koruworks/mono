// SPDX-License-Identifier: MPL-2.0

use super::*;
use rusqlite::params;

fn candidate(value: PendingPublicationRunSeek) -> (PendingPublicationRunCursor, RunId) {
    match value {
        PendingPublicationRunSeek::Candidate { cursor, run } => (cursor, run),
        other => panic!("expected candidate, got {other:?}"),
    }
}

#[test]
fn decision_publication_seek_distinct_runs_preserves_per_run_fifo_and_wrap_bound() {
    let root = Root::new();
    let mut db = root.open();
    let mut runs = [
        created_run(&mut db, 1),
        created_run(&mut db, 2),
        created_run(&mut db, 3),
    ];
    runs.sort_by_key(|r| r.to_storage());
    // Interleaved global insertion order deliberately differs from Run key order.
    for (id, run) in [
        (event(8), runs[2]),
        (event(7), runs[0]),
        (event(6), runs[0]),
        (event(5), runs[1]),
    ] {
        db.enqueue(id, run, PublicationKind::Lifecycle, &body(1))
            .unwrap();
    }
    let anchor = db.pending_publication_run_anchor().unwrap().unwrap();
    let (first, run) = candidate(db.next_pending_publication_run(None, anchor, None).unwrap());
    assert_eq!(run, runs[0]);
    assert_eq!(
        db.decision_open_publications_for(run, 64)
            .unwrap()
            .iter()
            .map(|p| p.event)
            .collect::<Vec<_>>(),
        vec![event(7), event(6)]
    );
    let (second, run) = candidate(
        db.next_pending_publication_run(Some(first), anchor, None)
            .unwrap(),
    );
    assert_eq!(run, runs[1]);
    let (last, run) = candidate(
        db.next_pending_publication_run(Some(second), anchor, None)
            .unwrap(),
    );
    assert_eq!(run, runs[2]);
    assert_eq!(
        db.next_pending_publication_run(Some(last), anchor, None)
            .unwrap(),
        PendingPublicationRunSeek::Exhausted
    );
    let (wrapped, _) = candidate(
        db.next_pending_publication_run(None, anchor, Some(first))
            .unwrap(),
    );
    assert_eq!(wrapped, first);
    assert_eq!(
        db.next_pending_publication_run(Some(wrapped), anchor, Some(first))
            .unwrap(),
        PendingPublicationRunSeek::Exhausted
    );
}

#[test]
fn decision_publication_seek_reports_bad_run_and_advances_past_whole_bad_key() {
    let root = Root::new();
    let mut db = root.open();
    let run = created_run(&mut db, 1);
    db.enqueue(event(1), run, PublicationKind::Lifecycle, &body(1))
        .unwrap();
    db.conn().execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    for id in [event(2), event(3)] {
        db.conn().execute("INSERT INTO pending_publications(event_id,run_id,kind,body_json,attempts,queued_at_ms) VALUES(?1,'!malformed','lifecycle','{}',0,0)", [id.to_string()]).unwrap();
    }
    db.conn().execute_batch("PRAGMA foreign_keys=ON").unwrap();
    let anchor = db.pending_publication_run_anchor().unwrap().unwrap();
    let PendingPublicationRunSeek::Malformed { cursor } =
        db.next_pending_publication_run(None, anchor, None).unwrap()
    else {
        panic!("explicit corrupt candidate");
    };
    let (valid, actual) = candidate(
        db.next_pending_publication_run(Some(cursor), anchor, None)
            .unwrap(),
    );
    assert_eq!(actual, run);
    assert_eq!(
        db.next_pending_publication_run(Some(valid), anchor, None)
            .unwrap(),
        PendingPublicationRunSeek::Exhausted
    );
    assert_eq!(
        db.conn()
            .query_row(
                "SELECT count(*) FROM pending_publications WHERE run_id='!malformed'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2,
        "malformed debt is retained"
    );
}

#[test]
fn decision_publication_seek_deleted_or_reused_cursor_is_explicitly_stale() {
    let root = Root::new();
    let mut db = root.open();
    let run = created_run(&mut db, 1);
    db.enqueue(event(1), run, PublicationKind::Lifecycle, &body(1))
        .unwrap();
    let anchor = db.pending_publication_run_anchor().unwrap().unwrap();
    db.conn()
        .execute(
            "DELETE FROM pending_publications WHERE event_id=?1",
            [event(1).to_string()],
        )
        .unwrap();
    assert_eq!(
        db.next_pending_publication_run(None, anchor, None).unwrap(),
        PendingPublicationRunSeek::StaleCursor
    );
    db.enqueue(event(2), run, PublicationKind::Lifecycle, &body(2))
        .unwrap();
    assert_eq!(
        db.next_pending_publication_run(None, anchor, None).unwrap(),
        PendingPublicationRunSeek::StaleCursor,
        "rowid reuse cannot substitute another event"
    );
}

#[test]
fn decision_publication_fifo_aggregate_budget_yields_without_skipping_head() {
    let root = Root::new();
    let mut db = root.open();
    let run = created_run(&mut db, 1);
    let body = serde_json::to_string(&"x".repeat(2 * 1024 * 1024)).unwrap();
    for id in [event(1), event(2), event(3)] {
        db.enqueue(id, run, PublicationKind::Lifecycle, &body)
            .unwrap();
    }
    let first = db.decision_open_publications_for(run, 64).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].event, event(1));
    db.acknowledge(event(1)).unwrap();
    let next = db.decision_open_publications_for(run, 64).unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].event, event(2));
    assert!(db.decision_open_publications_for(run, 0).is_err());
    assert!(db.decision_open_publications_for(run, 65).is_err());
}

#[test]
fn decision_publication_fifo_corrupt_oversized_head_refuses_without_skip() {
    let root = Root::new();
    let mut db = root.open();
    let run = created_run(&mut db, 1);
    let body = serde_json::to_string(&"x".repeat(4 * 1024 * 1024)).unwrap();
    db.enqueue(event(1), run, PublicationKind::Lifecycle, &body)
        .unwrap();
    db.enqueue(event(2), run, PublicationKind::Lifecycle, "{}")
        .unwrap();
    assert!(db.decision_open_publications_for(run, 64).is_err());
    assert!(
        db.publication(event(1))
            .unwrap()
            .unwrap()
            .acknowledged_at_ms
            .is_none()
    );
    assert!(
        db.publication(event(2))
            .unwrap()
            .unwrap()
            .acknowledged_at_ms
            .is_none()
    );
}

#[test]
fn decision_publication_bad_event_anchor_refresh_keeps_valid_lower_bound() {
    let root = Root::new();
    let mut db = root.open();
    let mut runs = [
        created_run(&mut db, 1),
        created_run(&mut db, 2),
        created_run(&mut db, 3),
    ];
    runs.sort_by_key(|r| r.to_storage());
    db.enqueue(event(1), runs[0], PublicationKind::Lifecycle, "{}")
        .unwrap();
    db.enqueue(event(2), runs[1], PublicationKind::Lifecycle, "{}")
        .unwrap();
    db.conn().execute("INSERT INTO pending_publications(event_id,run_id,kind,body_json,attempts,queued_at_ms) VALUES(?1,?2,'lifecycle','{}',0,0)",
        params!["bad-event".repeat(1024),runs[2].to_storage()]).unwrap();
    let anchor = db.pending_publication_run_anchor().unwrap().unwrap();
    let (first, run) = candidate(db.next_pending_publication_run(None, anchor, None).unwrap());
    assert_eq!(run, runs[0]);
    db.acknowledge(event(1)).unwrap();
    assert_eq!(
        db.next_pending_publication_run(Some(first), anchor, None)
            .unwrap(),
        PendingPublicationRunSeek::StaleCursor
    );
    let refreshed = db.pending_publication_run_anchor().unwrap().unwrap();
    let (second, run) = candidate(
        db.next_pending_publication_run(Some(first), refreshed, None)
            .unwrap(),
    );
    assert_eq!(
        run, runs[1],
        "refresh did not restart before the valid lower bound"
    );
    let PendingPublicationRunSeek::Malformed { cursor } = db
        .next_pending_publication_run(Some(second), refreshed, None)
        .unwrap()
    else {
        panic!("bad event remains explicit");
    };
    assert_eq!(
        db.next_pending_publication_run(Some(cursor), refreshed, None)
            .unwrap(),
        PendingPublicationRunSeek::Exhausted
    );
}

#[test]
fn decision_publication_bad_event_epoch_rejects_rollback_and_restart() {
    let root = Root::new();
    let mut db = root.open();
    let run = created_run(&mut db, 1);
    db.conn().execute("INSERT INTO pending_publications(event_id,run_id,kind,body_json,attempts,queued_at_ms) VALUES('invalid',?1,'lifecycle','{}',0,0)",
        [run.to_storage()]).unwrap();
    let anchor = db.pending_publication_run_anchor().unwrap().unwrap();
    let tx = db.conn().unchecked_transaction().unwrap();
    tx.execute("UPDATE pending_publications SET attempts=attempts+1", [])
        .unwrap();
    tx.rollback().unwrap();
    assert_eq!(
        db.next_pending_publication_run(None, anchor, None).unwrap(),
        PendingPublicationRunSeek::StaleCursor
    );
    let fresh = db.pending_publication_run_anchor().unwrap().unwrap();
    assert!(matches!(
        db.next_pending_publication_run(None, fresh, None).unwrap(),
        PendingPublicationRunSeek::Malformed { .. }
    ));
    db.restart_in_memory();
    assert_eq!(
        db.next_pending_publication_run(None, fresh, None).unwrap(),
        PendingPublicationRunSeek::StaleCursor
    );
}
