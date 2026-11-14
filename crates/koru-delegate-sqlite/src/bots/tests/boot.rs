// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn boot_inventory_uses_state_sequence_seeks_without_full_queue_sort() {
    use rusqlite::types::Value;
    let (db, _) = memory();
    let owner = submission(1).provenance.owner;
    let base = vec![
        Value::from("queued".to_owned()),
        owner.user_scope.to_storage().into(),
        owner.device.to_storage().into(),
    ];
    let mut page = base.clone();
    page.extend([0i64.into(), 999i64.into(), 17i64.into()]);
    for (query, values) in [
        (super::super::repository::BOT_BOOT_ROWS_SQL, page),
        (super::super::repository::BOT_BOOT_HIGH_WATER_SQL, base),
    ] {
        let details = db
            .conn()
            .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
            .unwrap()
            .query_map(rusqlite::params_from_iter(values), |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join(" ");
        assert!(details.contains("bot_admissions_queue"), "{details}");
        assert!(!details.contains("TEMP B-TREE"), "{details}");
        assert!(!details.contains("SCAN bot_admissions"), "{details}");
    }
}

#[test]
fn reservation_recovery_retains_each_independent_evidence_guard() {
    // These deliberately isolate evidence that a damaged/interrupted ledger
    // could retain. Production never disables its constraints.
    for evidence in [
        "UPDATE bot_admissions SET state='executing'",
        "UPDATE bot_admissions SET active_elapsed_ms=1",
        "PRAGMA ignore_check_constraints=ON; UPDATE bot_admissions SET active_since_ms=1; PRAGMA ignore_check_constraints=OFF",
        "UPDATE bot_admissions SET initial_send_key='11111111-1111-4111-8111-111111111111',initial_send_fingerprint=zeroblob(32),initial_send_attempt_id='22222222-2222-4222-8222-222222222222',initial_send_generation=1",
        "UPDATE bot_admissions SET delivery_outcome='confirmed',delivery_attempt_id='11111111-1111-4111-8111-111111111111',delivery_generation=1,delivery_chat_id='11111111-1111-4111-8111-111111111111',delivery_ai_attempt_id='11111111-1111-4111-8111-111111111111',delivery_assistant_message_id='11111111-1111-4111-8111-111111111111',delivery_confirmed_at_ms=1",
    ] {
        let (mut db, _) = memory();
        let s = submission(1);
        db.admit_bot(&s).unwrap();
        db.reserve_bot(s.request.run, None).unwrap();
        let key = BotRunKey {
            owner: s.provenance.owner,
            bot: s.provenance.bot,
            run: s.request.run,
        };
        db.conn().execute_batch(evidence).unwrap();
        assert!(!db.recover_bot_reservation(&key).unwrap(), "{evidence}");
        let state: String = db
            .conn()
            .query_row("SELECT state FROM bot_admissions", [], |r| r.get(0))
            .unwrap();
        assert!(state == "reserved" || state == "executing");
    }
    let (mut db, _) = memory();
    let s = submission(1);
    db.admit_bot(&s).unwrap();
    db.reserve_bot(s.request.run, None).unwrap();
    let key = BotRunKey {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        run: s.request.run,
    };
    let mut foreign = key.clone();
    foreign.owner.user_scope = Default::default();
    assert!(!db.recover_bot_reservation(&foreign).unwrap());
    foreign = key.clone();
    foreign.bot = BotId::mint();
    assert!(!db.recover_bot_reservation(&foreign).unwrap());
    foreign = key.clone();
    foreign.owner.device = koru_delegate_core::DeviceId::from_uuid(Uuid::new_v4());
    assert!(!db.recover_bot_reservation(&foreign).unwrap());
    // Orphan native evidence without an attempt is inconsistent, but cannot be
    // interpreted as proof that a provider/effect never existed.
    db.conn().execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    db.conn().execute("INSERT INTO native_result_intents(run_id,operation_id,attempt_id,generation,turn_id,kind,request_fingerprint,destination_workspace_id,target_document_id,store_request_id,requested_title,stage,lease_state,created_at_ms,updated_at_ms) VALUES(?1,?2,?2,1,?2,'create_page',zeroblob(32),?2,?2,?2,'fixture','prepared','none',1,1)", rusqlite::params![key.run.to_storage(), Uuid::new_v4().to_string()]).unwrap();
    db.conn().execute_batch("PRAGMA foreign_keys=ON").unwrap();
    assert!(!db.recover_bot_reservation(&key).unwrap());
    assert_eq!(db.active_bot_executions(key.owner.device).unwrap().len(), 1);
}

#[test]
fn boot_inventory_includes_executing_and_caps_pages_without_task_payloads() {
    let (mut db, _) = memory();
    let s = submission(1);
    running(&mut db, &s);
    for n in 2..=20 {
        db.admit_bot(&submission(n)).unwrap();
    }
    // Inventory must remain readable even if a task payload is undecodable.
    db.conn()
        .execute("UPDATE bot_admissions SET payload_json='{}'", [])
        .unwrap();
    let page = db.bot_boot_inventory(s.provenance.owner, 0, None).unwrap();
    assert_eq!(page.entries.len(), 16);
    assert_eq!(page.entries[0].run, s.request.run);
    assert_eq!(page.through, 20);
    assert!(page.more);
    let next = db
        .bot_boot_inventory(s.provenance.owner, page.scanned_through, Some(page.through))
        .unwrap();
    assert_eq!(next.entries.len(), 4);
    assert!(!next.more);
    assert_eq!(next.scanned_through, 20);
    let mut foreign = s.provenance.owner;
    foreign.device = koru_delegate_core::DeviceId::from_uuid(Uuid::new_v4());
    assert!(
        db.bot_boot_inventory(foreign, 0, None)
            .unwrap()
            .entries
            .is_empty()
    );
    assert!(!db.bot_has_unsettled(foreign, s.provenance.bot).unwrap());
}

#[test]
fn boot_inventory_high_water_excludes_new_rows_and_advances_after_settlement() {
    let (mut db, _) = memory();
    for n in 1..=18 {
        db.admit_bot(&submission(n)).unwrap();
    }
    let owner = submission(1).provenance.owner;
    let page = db.bot_boot_inventory(owner, 0, None).unwrap();
    db.admit_bot(&submission(19)).unwrap();
    db.settle_bot(submission(17).request.run, BotSettlement::Refused)
        .unwrap();
    let next = db
        .bot_boot_inventory(owner, page.scanned_through, Some(page.through))
        .unwrap();
    assert_eq!(
        next.entries
            .iter()
            .map(|row| row.sequence)
            .collect::<Vec<_>>(),
        vec![18]
    );
    assert_eq!(next.scanned_through, 18);
    assert!(!next.more);
    assert!(db.bot_boot_inventory(owner, 19, Some(18)).is_err());
}

#[test]
fn restart_requeues_only_proven_never_started_reservations() {
    let (mut db, _) = memory();
    let s = submission(1);
    db.admit_bot(&s).unwrap();
    assert_eq!(
        db.reserve_bot(s.request.run, None).unwrap(),
        BotReservation::Acquired
    );
    let key = BotRunKey {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        run: s.request.run,
    };
    assert!(db.recover_bot_reservation(&key).unwrap());
    assert!(!db.recover_bot_reservation(&key).unwrap());
    assert_eq!(
        db.bot_admission(key.run).unwrap().unwrap().state,
        BotAdmissionState::Queued
    );
    assert!(
        db.active_bot_executions(key.owner.device)
            .unwrap()
            .is_empty()
    );
    db.reserve_bot(key.run, None).unwrap();
    db.satisfy_intent(key.run, fixtures::document(0xd0))
        .unwrap();
    db.open_attempt(key.run).unwrap();
    assert!(!db.recover_bot_reservation(&key).unwrap());
    // Even a retired terminal attempt is evidence of prior provider ownership.
    db.conn()
        .execute("UPDATE run_attempts SET retired=1,state='ended'", [])
        .unwrap();
    assert!(!db.recover_bot_reservation(&key).unwrap());
    assert_eq!(db.active_bot_executions(key.owner.device).unwrap().len(), 1);
}

#[test]
fn reservation_recovery_reply_loss_is_idempotent_and_failed_read_retains_claim() {
    for at in Moment::ALL {
        let (mut db, _) = memory();
        let s = submission(1);
        db.admit_bot(&s).unwrap();
        db.reserve_bot(s.request.run, None).unwrap();
        let key = BotRunKey {
            owner: s.provenance.owner,
            bot: s.provenance.bot,
            run: s.request.run,
        };
        let point = CrashPoint {
            op: WriteOp::RecoverBotReservation,
            at,
        };
        db.arm_crash(point);
        assert_eq!(
            db.recover_bot_reservation(&key),
            Err(BotLedgerError::Ledger(LedgerError::Crashed(point)))
        );
        db.restart_in_memory();
        assert_eq!(
            db.recover_bot_reservation(&key).unwrap(),
            at != Moment::AfterCommit
        );
        assert!(
            db.active_bot_executions(key.owner.device)
                .unwrap()
                .is_empty()
        );
    }
    let (mut db, _) = memory();
    let s = submission(1);
    db.admit_bot(&s).unwrap();
    db.reserve_bot(s.request.run, None).unwrap();
    let key = BotRunKey {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        run: s.request.run,
    };
    db.conn()
        .execute_batch("ALTER TABLE native_result_intents RENAME TO unavailable_native_evidence")
        .unwrap();
    assert!(db.recover_bot_reservation(&key).is_err());
    assert_eq!(db.active_bot_executions(key.owner.device).unwrap().len(), 1);
}
