// SPDX-License-Identifier: MPL-2.0

//! The retry queue: keyed by `EventId`, idempotent both ways, oldest
//! first, and the body never rendered.

use koru_delegate_core::PublicationKind;

use super::*;
use crate::fixtures::{BODY_SENTINEL, Root, created_run, event};
mod engine_retirement;

fn body(n: u8) -> String {
    format!("{{\"sentinel\":\"{BODY_SENTINEL}\",\"n\":{n}}}")
}

#[test]
fn facts_in_the_same_clock_tick_keep_insertion_order() {
    let mut db = RunsDb::open_with_clock(
        std::path::Path::new(":memory:"),
        Box::new(crate::fixtures::FakeClock(std::sync::Arc::new(
            std::sync::atomic::AtomicI64::new(1_000),
        ))),
    )
    .unwrap();
    let run = created_run(&mut db, 1);
    let expected = vec![event(3), event(2), event(1)];
    for id in &expected {
        db.enqueue(*id, run, PublicationKind::Lifecycle, &body(1))
            .unwrap();
    }
    let open = db.open_publications(10).unwrap();
    assert!(
        open.iter()
            .all(|row| row.queued_at_ms == open[0].queued_at_ms)
    );
    assert_eq!(
        open.iter().map(|row| row.event).collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        db.publications(run)
            .unwrap()
            .iter()
            .map(|row| row.event)
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        db.pending_publications(run)
            .unwrap()
            .iter()
            .map(|row| row.event)
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn publications_queue_oldest_first_and_close_on_acknowledgement() {
    let root = Root::new();
    let mut db = root.open();
    let run = created_run(&mut db, 1);
    assert_eq!(
        db.enqueue(event(2), run, PublicationKind::Lifecycle, &body(2))
            .unwrap(),
        WriteOutcome::Applied
    );
    root.tick(5);
    db.enqueue(event(1), run, PublicationKind::Usage, &body(1))
        .unwrap();
    root.tick(5);
    db.enqueue(
        event(3),
        run,
        PublicationKind::ArtifactRegistration,
        &body(3),
    )
    .unwrap();

    let open = db.open_publications(10).unwrap();
    let ids: Vec<_> = open.iter().map(|p| p.event).collect();
    assert_eq!(ids, vec![event(2), event(1), event(3)]);
    assert_eq!(open[1].kind, PublicationKind::Usage);
    assert_eq!(open[1].body.as_str(), body(1));
    assert_eq!(open[1].attempts, 0);
    assert_eq!(open[1].queued_at_ms, open[0].queued_at_ms + 5);
    assert_eq!(open[2].run, run);
    assert_eq!(db.open_publications(2).unwrap().len(), 2, "the limit holds");

    assert_eq!(db.acknowledge(event(1)).unwrap(), WriteOutcome::Applied);
    let ids: Vec<_> = db
        .open_publications(10)
        .unwrap()
        .iter()
        .map(|p| p.event)
        .collect();
    assert_eq!(ids, vec![event(2), event(3)]);
    let rows: i64 = db
        .conn()
        .query_row("SELECT count(*) FROM pending_publications", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 3, "an acknowledged row stays: a row is a fact");
}

/// Duplicate requests and lost acknowledgements are both answered by the
/// event id.
#[test]
fn enqueue_and_acknowledge_are_idempotent_on_the_event_id() {
    let root = Root::new();
    let mut db = root.open();
    let run = created_run(&mut db, 1);
    db.enqueue(event(1), run, PublicationKind::Lifecycle, &body(1))
        .unwrap();
    assert_eq!(
        db.enqueue(event(1), run, PublicationKind::Lifecycle, &body(1))
            .unwrap(),
        WriteOutcome::AlreadyApplied
    );
    assert_eq!(
        db.enqueue(event(1), run, PublicationKind::Usage, &body(9))
            .unwrap(),
        WriteOutcome::AlreadyApplied,
        "an event is one event forever; the first body stands"
    );
    assert_eq!(db.open_publications(10).unwrap().len(), 1);
    assert_eq!(db.open_publications(10).unwrap()[0].body.as_str(), body(1));

    assert_eq!(db.acknowledge(event(1)).unwrap(), WriteOutcome::Applied);
    assert_eq!(
        db.acknowledge(event(1)).unwrap(),
        WriteOutcome::AlreadyApplied
    );
    assert_eq!(
        db.enqueue(event(1), run, PublicationKind::Lifecycle, &body(1))
            .unwrap(),
        WriteOutcome::AlreadyApplied,
        "a late retry of a done publication reopens nothing"
    );
    assert!(db.open_publications(10).unwrap().is_empty());
    assert_eq!(
        db.acknowledge(event(2)),
        Err(LedgerError::UnknownPublication)
    );
    assert_eq!(
        db.bump_attempts(event(2)),
        Err(LedgerError::UnknownPublication)
    );
}

#[test]
fn delivery_attempts_are_counted_and_the_body_must_be_json() {
    let root = Root::new();
    let mut db = root.open();
    let run = created_run(&mut db, 1);
    assert_eq!(
        db.enqueue(event(1), run, PublicationKind::Lifecycle, "not json"),
        Err(LedgerError::BodyNotJson)
    );
    db.enqueue(event(1), run, PublicationKind::Lifecycle, &body(1))
        .unwrap();
    assert_eq!(db.bump_attempts(event(1)).unwrap(), 1);
    assert_eq!(db.bump_attempts(event(1)).unwrap(), 2);
    assert_eq!(db.open_publications(10).unwrap()[0].attempts, 2);
    assert_eq!(db.pending_publications(run).unwrap()[0].attempts, 2);
    assert_eq!(db.pending_publications(run).unwrap()[0].event, event(1));
}

/// The body is content: it is in the file, it is what a retry sends, and
/// it is in nothing this crate renders.
#[test]
fn the_body_is_returned_for_retry_and_never_rendered() {
    let root = Root::new();
    let mut db = root.open();
    let run = created_run(&mut db, 1);
    db.enqueue(event(1), run, PublicationKind::Lifecycle, &body(1))
        .unwrap();
    let open = db.open_publications(10).unwrap();
    assert!(open[0].body.as_str().contains(BODY_SENTINEL));
    let renderings = [
        format!("{:?}", open[0]),
        format!("{:#?}", open),
        format!("{:?}", open[0].body),
        format!("{:?}", db.pending_publications(run)),
        format!("{:?}", db.attempt_state(run)),
    ];
    for rendered in renderings {
        assert!(!rendered.contains(BODY_SENTINEL), "{rendered}");
    }
    assert_eq!(
        format!("{:?}", open[0].body),
        format!("PublicationBody(<{} bytes>)", body(1).len())
    );
}

/// Retry identity outlives acknowledgement: a row is readable by its id
/// and in its run's order after it was acknowledged, with the moment it
/// was queued — what a re-publication after a restart is stamped with.
#[test]
fn a_publication_is_readable_by_id_and_by_run_after_acknowledgement() {
    let root = Root::new();
    let mut db = root.open();
    let run = created_run(&mut db, 1);
    let other = created_run(&mut db, 2);
    db.enqueue(event(1), run, PublicationKind::Lifecycle, &body(1))
        .unwrap();
    let queued_at = root.now_ms();
    root.tick(5);
    db.enqueue(event(2), run, PublicationKind::Usage, &body(2))
        .unwrap();
    db.enqueue(event(3), other, PublicationKind::Lifecycle, &body(3))
        .unwrap();
    db.acknowledge(event(1)).unwrap();

    let first = db.publication(event(1)).unwrap().expect("recorded");
    assert_eq!(first.run, run);
    assert_eq!(first.kind, PublicationKind::Lifecycle);
    assert_eq!(first.queued_at_ms, queued_at);
    assert!(first.acknowledged_at_ms.is_some());
    assert_eq!(first.body.as_str(), body(1));
    assert!(db.publication(event(9)).unwrap().is_none());

    let of_run: Vec<_> = db
        .publications(run)
        .unwrap()
        .into_iter()
        .map(|record| (record.event, record.acknowledged_at_ms.is_some()))
        .collect();
    assert_eq!(of_run, vec![(event(1), true), (event(2), false)]);
    assert!(!format!("{first:?}").contains(BODY_SENTINEL));
}
