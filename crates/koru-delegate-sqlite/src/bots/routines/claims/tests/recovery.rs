// SPDX-License-Identifier: MPL-2.0
//! Actual retained claims and bounded read-only recovery; no execution is enabled.
use super::*;
use crate::SlotState;

fn next_subject(f: &mut Fixture) {
    f.route.bot = BotId::mint();
    f.route.schedule = ScheduleId::mint();
    f.route.creation_request = BotRequestId::mint();
    f.prepared.slot.bot = f.route.bot;
    f.prepared.slot.schedule = f.route.schedule;
    f.prepared.ids = RoutineExecutionIds {
        submission: BotRequestId::mint(),
        run: RunId::mint(),
        chat: uuid::Uuid::new_v4(),
        attempt: uuid::Uuid::new_v4(),
        user_message: uuid::Uuid::new_v4(),
        assistant_message: uuid::Uuid::new_v4(),
    };
    let p = &mut f.prepared.submission.provenance;
    p.bot = f.route.bot;
    p.submission = f.prepared.ids.submission;
    p.chat_id = f.prepared.ids.chat;
    p.ai_attempt_id = Some(f.prepared.ids.attempt);
    p.assistant_message_id = Some(f.prepared.ids.assistant_message);
    p.routine.as_mut().unwrap().schedule = f.route.schedule;
    f.prepared.submission.request.run = f.prepared.ids.run;
    f.prepared.submission.request.origin = RunOrigin::Scheduled {
        schedule: f.route.schedule,
        occurrence_ms: f.prepared.slot.occurrence_ms,
        revision: 1,
    };
    f.fresh.global.key.bot = f.route.bot;
    f.fresh.global.key.request = BotRequestId::mint().into();
    let r = f.fresh.routine.as_mut().unwrap();
    r.bot = f.route.bot;
    r.schedule = f.route.schedule;
    r.request = BotRequestId::mint().into();
    f.admission.bot = f.route.bot;
    f.admission.request = BotRequestId::mint();
    f.db.register_routine_route(&f.route).unwrap();
    f.db.apply_bot_barrier(&f.admission).unwrap();
    f.db.install_native_policy(&f.fresh.global).unwrap();
    f.db.install_routine_policy(f.fresh.routine.as_ref().unwrap())
        .unwrap();
}
fn held(count: usize) -> Fixture {
    let mut f = Fixture::new();
    for n in 0..count {
        if n != 0 {
            next_subject(&mut f);
        }
        f.claim();
    }
    f
}

#[test]
fn routine_recovery_valid_ordinary_slots_keep_existing_canonical_and_executable_reads() {
    let mut f = Fixture::new();
    let ordinary_schedule = ScheduleId::mint();
    let slot = f.db.claim_slot(ordinary_schedule, 800, 1).unwrap();
    let ordinary = f.db.ordinary_unsettled_claims().unwrap();
    assert_eq!(ordinary.len(), 1);
    assert_eq!(ordinary[0].run, slot.run);
    assert_eq!(ordinary, f.db.unsettled_claims().unwrap());
    f.claim();
    assert_eq!(f.db.ordinary_unsettled_claims().unwrap(), ordinary);
    assert_eq!(f.db.unsettled_claims().unwrap().len(), 2);
}

#[test]
fn routine_recovery_pages_are_read_only_and_forward_complete_claim_facts() {
    let mut f = Fixture::new();
    let record = f.claim();
    let changes: i64 =
        f.db.conn()
            .query_row("SELECT total_changes()", [], |r| r.get(0))
            .unwrap();
    let page = f.db.held_routine_page(None).unwrap();
    assert_eq!(page.entries.len(), 1);
    assert!(page.next.is_none());
    assert_eq!(page.entries[0].1.as_ref().unwrap().record, record);
    assert_eq!(page.entries[0].1.as_ref().unwrap().route, f.route);
    let q = f.db.queued_routine_page(None).unwrap();
    assert_eq!(
        q.entries[0].1.as_ref().unwrap().as_ref().unwrap().record,
        record
    );
    let slots = f.db.routine_slots_page(&f.route, None).unwrap();
    assert_eq!(
        slots.entries[0].1.as_ref().unwrap().slot.state,
        SlotState::Claimed
    );
    assert_eq!(
        slots.entries[0].1.as_ref().unwrap().observation,
        RoutineClaimObservation::Claimed { record }
    );
    assert_eq!(f.db.unsettled_claims().unwrap().len(), 1);
    assert!(f.db.ordinary_unsettled_claims().unwrap().is_empty());
    assert!(f.db.pending_intents().unwrap().is_empty());
    assert_eq!(
        f.db.conn()
            .query_row("SELECT total_changes()", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        changes
    );
}

#[test]
fn routine_recovery_held_pages_bound_rows_and_continue_from_each_selected_entry() {
    let f = held(20);
    let first = f.db.held_routine_page(None).unwrap();
    assert_eq!(first.entries.len(), 16);
    assert!(first.next.is_some());
    let second = f.db.held_routine_page(first.next).unwrap();
    assert_eq!(second.entries.len(), 4);
    assert!(second.next.is_none());
    let resumed = f.db.held_routine_page(Some(first.entries[0].0)).unwrap();
    assert_eq!(resumed.entries.len(), 16);
    assert_eq!(
        resumed.entries[0].1.as_ref().unwrap().record,
        first.entries[1].1.as_ref().unwrap().record
    );
    assert_eq!(
        format!("{:?}", first.entries[0].0),
        "RoutineHeldCursor([REDACTED])"
    );
}

#[test]
fn routine_recovery_held_cursor_survives_ack_timestamp_without_skipping_remaining_rows() {
    let mut f = held(3);
    let page = f.db.held_routine_page(None).unwrap();
    let claim = page.entries[0].1.as_ref().unwrap().record.claim.clone();
    f.clock.store(100_000, Ordering::SeqCst);
    f.db.acknowledge_routine_link(&RoutineLinkAcknowledgement {
        claim,
        outcome: RoutineLinkOutcome::Linked {},
    })
    .unwrap();
    let after = f.db.held_routine_page(Some(page.entries[0].0)).unwrap();
    assert_eq!(after.entries.len(), 2);
    assert!(after.entries.iter().all(|(_, result)| result.is_ok()));
}

#[test]
fn routine_recovery_corrupt_large_sort_key_advances_without_becoming_absence() {
    let f = held(3);
    f.db.conn()
        .execute_batch("PRAGMA foreign_keys=OFF")
        .unwrap();
    f.db.conn().execute("UPDATE bot_routine_links SET user_scope=?1 WHERE rowid=(SELECT MIN(rowid) FROM bot_routine_links)",
        ["!".repeat(100_000)]).unwrap();
    let page = f.db.held_routine_page(None).unwrap();
    assert!(page.entries[0].1.is_err());
    let after = f.db.held_routine_page(Some(page.entries[0].0)).unwrap();
    assert_eq!(after.entries.len(), 2);
    assert!(after.entries.iter().all(|(_, result)| result.is_ok()));
}

#[test]
fn routine_recovery_missing_anchor_is_error_not_restart_or_absence() {
    let f = held(1);
    let cursor = f.db.held_routine_page(None).unwrap().entries[0].0;
    f.db.conn()
        .execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM bot_routine_links;")
        .unwrap();
    assert!(f.db.held_routine_page(Some(cursor)).is_err());
}

#[test]
fn routine_recovery_marked_corrupt_slot_is_partitioned_before_decode_and_audit_advances() {
    let mut f = Fixture::new();
    f.claim();
    f.db.conn()
        .execute_batch("PRAGMA foreign_keys=OFF")
        .unwrap();
    f.db.conn()
        .execute(
            "UPDATE schedule_claims SET run_id=?1",
            ["x".repeat(100_000)],
        )
        .unwrap();
    f.db.conn().execute("INSERT INTO schedule_claims(schedule_id,occurrence_ms,revision,run_id,outcome,claimed_at_ms) VALUES(?1,901,1,?2,'skipped_missed',1000)",
        params![f.route.schedule.to_storage(),RunId::mint().to_storage()]).unwrap();
    assert!(f.db.ordinary_unsettled_claims().unwrap().is_empty());
    let page = f.db.routine_slots_page(&f.route, None).unwrap();
    assert!(page.entries[0].1.is_err());
    assert!(page.entries[1].1.is_ok());
    let next =
        f.db.routine_slots_page(&f.route, Some(page.entries[0].0))
            .unwrap();
    assert_eq!(next.entries.len(), 1);
    assert!(next.entries[0].1.is_ok());
}

#[test]
fn routine_recovery_ordinary_only_queued_page_preserves_physical_continuation() {
    let mut f = Fixture::new();
    for n in 2..=17 {
        let mut s = crate::bots::tests::submission(n);
        s.provenance.bot = BotId::mint();
        f.db.admit_bot(&s).unwrap();
    }
    let record = f.claim();
    let page = f.db.queued_routine_page(None).unwrap();
    assert_eq!(page.entries.len(), 16);
    assert!(page.entries.iter().all(|(_, r)| matches!(r, Ok(None))));
    let next = f.db.queued_routine_page(page.next).unwrap();
    assert_eq!(next.entries.len(), 1);
    assert_eq!(
        next.entries[0].1.as_ref().unwrap().as_ref().unwrap().record,
        record
    );
}

#[test]
fn routine_recovery_queued_v2_missing_all_companions_remains_corrupt_and_nonordinary() {
    let mut f = Fixture::new();
    f.claim();
    f.db.conn().execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM bot_routine_links; DELETE FROM schedule_claims; DELETE FROM bot_routine_routes;").unwrap();
    assert!(
        f.db.queued_routine_page(None).unwrap().entries[0]
            .1
            .is_err()
    );
    assert!(f.db.pending_intents().unwrap().is_empty());
}

#[test]
fn routine_recovery_orphan_link_independently_denies_v1_manual_downgrade() {
    let mut f = Fixture::new();
    f.claim();
    let mut s = f.prepared.submission.clone();
    s.provenance.routine = None;
    s.request.origin = RunOrigin::Manual;
    f.db.conn()
        .execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM bot_routine_routes;")
        .unwrap();
    f.db.conn()
        .execute(
            "UPDATE bot_admissions SET submission_version=1,payload_json=?1",
            [serde_json::to_string(&s).unwrap()],
        )
        .unwrap();
    f.db.conn()
        .execute(
            "UPDATE run_intents SET payload_json=?1",
            [serde_json::to_string(&s.request).unwrap()],
        )
        .unwrap();
    assert_eq!(f.db.unsettled_claims().unwrap().len(), 1);
    assert!(f.db.validate_ordinary_schedule_run(s.request.run).is_err());
    assert!(f.db.ordinary_unsettled_claims().unwrap().is_empty());
    assert!(
        f.db.queued_routine_page(None).unwrap().entries[0]
            .1
            .is_err()
    );
}

#[test]
fn routine_recovery_slot_pages_preserve_skip_truth_and_reject_foreign_route_cursor() {
    let mut f = Fixture::new();
    let first_route = f.route.clone();
    for n in 0..20 {
        f.db.conn().execute("INSERT INTO schedule_claims(schedule_id,occurrence_ms,revision,run_id,outcome,claimed_at_ms) VALUES(?1,?2,1,?3,'skipped_missed',1000)",
            params![f.route.schedule.to_storage(),n,RunId::mint().to_storage()]).unwrap();
    }
    let page = f.db.routine_slots_page(&first_route, None).unwrap();
    assert_eq!(page.entries.len(), 16);
    assert!(page.entries.iter().all(|(_, r)| matches!(
        &r.as_ref().unwrap().observation,
        RoutineClaimObservation::Skipped { .. }
    )));
    assert_eq!(
        f.db.routine_slots_page(&first_route, page.next)
            .unwrap()
            .entries
            .len(),
        4
    );
    next_subject(&mut f);
    assert!(
        f.db.routine_slots_page(&f.route, Some(page.entries[0].0))
            .is_err()
    );
    let mut changed = first_route;
    changed.seed_hash = RoutineDigest32::from_bytes([77; 32]);
    assert!(f.db.routine_slots_page(&changed, None).is_err());
}

#[test]
fn routine_recovery_query_plans_use_existing_bounded_keyspaces_without_history_sort() {
    let f = held(20);
    for (index, query) in super::super::super::recovery::query_shapes()
        .into_iter()
        .enumerate()
    {
        let mut s =
            f.db.conn()
                .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
                .unwrap();
        let values = match index {
            1 => vec![
                rusqlite::types::Value::Integer(1),
                rusqlite::types::Value::Integer(1000),
            ],
            3 => vec![rusqlite::types::Value::Integer(1)],
            4 => vec![rusqlite::types::Value::Text(f.route.schedule.to_storage())],
            5 => vec![
                rusqlite::types::Value::Text(f.route.schedule.to_storage()),
                rusqlite::types::Value::Integer(1),
            ],
            _ => vec![],
        };
        let details = s
            .query_map(rusqlite::params_from_iter(values), |r| {
                r.get::<_, String>(3)
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n");
        let expected = match index {
            0 | 1 => "bot_routine_link_recovery",
            2 | 3 => "bot_admissions_queue",
            _ => "sqlite_autoindex_schedule_claims",
        };
        assert!(details.contains(expected), "{details}");
        assert!(!details.contains("TEMP B-TREE"), "{details}");
        assert!(!details.contains("SCAN bot_admissions\n"), "{details}");
    }
}
