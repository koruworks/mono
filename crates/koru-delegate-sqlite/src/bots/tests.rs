// SPDX-License-Identifier: MPL-2.0

use super::*;
mod boot;
mod admission_read;
use crate::{AttemptPhase, CrashPoint, LedgerError, Moment, RunsDb, WriteOp, fixtures};
use koru_delegate_core::{
    BotAccess, BotProvenance, BotWorkingFolder, CapturedBotPolicy, PendingInput, WorkspaceScope,
};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use uuid::Uuid;

#[derive(Clone)]
struct Time(Arc<AtomicI64>);
impl crate::Clock for Time {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
impl Time {
    fn set(&self, t: i64) {
        self.0.store(t, Ordering::SeqCst)
    }
}
fn memory() -> (RunsDb, Time) {
    let time = Time(Arc::new(AtomicI64::new(1000)));
    (RunsDb::in_memory(Box::new(time.clone())), time)
}
pub(super) fn submission(n: u8) -> BotSubmission {
    let mut request = fixtures::request(n);
    request.scope = WorkspaceScope::BotFolder {
        folder: BotWorkingFolder::new("/private/bot-folder".into()).unwrap(),
    };
    BotSubmission {
        provenance: BotProvenance {
            routine: None,
            owner: BotOwner {
                user_scope: serde_json::from_value(serde_json::json!(fixtures::v4(0xb0))).unwrap(),
                device: request.executing_device,
            },
            bot: BotId::from_uuid(fixtures::v4(0xb1)),
            submission: koru_delegate_core::BotRequestId::from_uuid(fixtures::v4(n)),
            chat_id: fixtures::v4(0xb2),
            ai_attempt_id: Some(fixtures::v4(0xb3)),
            assistant_message_id: Some(fixtures::v4(0xb4)),
            config_revision: 1,
            policy_revision: 1,
            continued_from: None,
        },
        policy: CapturedBotPolicy {
            workspace: fixtures::workspace(0xa2),
            destination: fixtures::workspace(0xa2),
            access: BotAccess::Default,
            selected_pages: Default::default(),
        },
        workspace: fixtures::workspace(0xa2),
        request,
        input: PendingInput {
            idempotency_key: fixtures::key(n),
            text: "PRIVATE-TASK-SENTINEL".into(),
            attachment_ids: vec![],
            references: vec![],
            mode: None,
            fast: None,
            reasoning: None,
            skills: vec![],
            model: None,
        },
    }
}
fn key(s: &BotSubmission) -> BotSubmissionKey {
    BotSubmissionKey {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        request: s.provenance.submission,
    }
}
fn pause(s: &BotSubmission, epoch: u64, paused: bool) -> BotAdmissionBarrier {
    BotAdmissionBarrier {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        request: koru_delegate_core::BotRequestId::mint(),
        epoch,
        paused,
    }
}
fn running(db: &mut RunsDb, s: &BotSubmission) -> crate::AttemptRow {
    db.admit_bot(s).unwrap();
    db.satisfy_intent(s.request.run, fixtures::document(0xd0))
        .unwrap();
    assert_eq!(
        db.reserve_bot(s.request.run).unwrap(),
        BotReservation::Acquired
    );
    let a = fixtures::running_attempt(db, s.request.run, 1);
    db.activate_bot(a.run, a.attempt, a.generation).unwrap();
    a
}
fn send(a: &crate::AttemptRow) -> BotInitialSend {
    BotInitialSend {
        attempt: a.attempt,
        generation: a.generation,
        key: fixtures::key(0xc1),
        fingerprint: [7; 32],
        turn: None,
    }
}
fn retired(db: &mut RunsDb, a: &crate::AttemptRow) {
    db.advance(a.run, a.generation, AttemptPhase::Interrupted)
        .unwrap();
    db.retire(a.run, a.generation).unwrap();
}

#[test]
fn admission_binds_entire_payload_and_never_adopts_an_ordinary_run() {
    let (mut db, _) = memory();
    let s = submission(1);
    let first = db.admit_bot(&s).unwrap();
    assert_eq!(db.admit_bot(&s).unwrap(), first);
    assert_eq!(db.reconcile_bot(&key(&s)).unwrap(), Some(first.clone()));
    for change in 0..4 {
        let mut other = s.clone();
        match change {
            0 => other.input.text.push_str(" changed"),
            1 => other.policy.access = BotAccess::Workspace,
            2 => other.request.run = fixtures::run_id(2),
            _ => other.provenance.config_revision += 1,
        }
        assert_eq!(db.admit_bot(&other), Err(BotLedgerError::Conflict));
    }
    let mut other = s.clone();
    other.provenance.submission = koru_delegate_core::BotRequestId::mint();
    assert_eq!(db.admit_bot(&other), Err(BotLedgerError::Conflict));
    let ordinary = submission(3);
    db.record_intent(&ordinary.request, ordinary.workspace)
        .unwrap();
    assert_eq!(db.admit_bot(&ordinary), Err(BotLedgerError::Conflict));
    assert!(db.bot_admission(ordinary.request.run).unwrap().is_none());
    let mut foreign = key(&s);
    foreign.owner.user_scope = Default::default();
    assert!(db.reconcile_bot(&foreign).unwrap().is_none());
    assert!(!format!("{first:?}").contains("PRIVATE-TASK-SENTINEL"));
    assert!(!format!("{first:?}").contains("private/bot-folder"));
}

#[test]
fn ordinary_write_wrapper_keeps_its_commit_and_rollback_behavior() {
    for at in Moment::ALL {
        let (mut db, _) = memory();
        let request = fixtures::request(1);
        let point = CrashPoint {
            op: WriteOp::RecordIntent,
            at,
        };
        db.arm_crash(point);
        assert_eq!(
            db.record_intent(&request, fixtures::workspace(0xa2)),
            Err(LedgerError::Crashed(point))
        );
        assert_eq!(
            db.intent(request.run).unwrap().is_some(),
            at == Moment::AfterCommit
        );
        db.restart_in_memory();
        let retry = db
            .record_intent(&request, fixtures::workspace(0xa2))
            .unwrap();
        assert_eq!(
            retry,
            if at == Moment::AfterCommit {
                crate::WriteOutcome::AlreadyApplied
            } else {
                crate::WriteOutcome::Applied
            }
        );
    }
}

#[test]
fn rejected_invalid_or_over_capacity_admission_leaves_no_intent() {
    let (mut db, _) = memory();
    for n in 1..=50 {
        db.admit_bot(&submission(n)).unwrap();
    }
    let extra = submission(51);
    assert_eq!(db.admit_bot(&extra), Err(BotLedgerError::Capacity));
    assert!(db.intent(extra.request.run).unwrap().is_none());
    assert_eq!(db.queued_bots(0, 1000).unwrap().len(), 50);
    assert_eq!(db.queued_bots(0, 0).unwrap().len(), 1);
    let page = db.queued_bots(10, 2).unwrap();
    assert_eq!(
        page.iter().map(|q| q.sequence).collect::<Vec<_>>(),
        vec![11, 12]
    );
    assert!(db.queued_bots(u64::MAX, 1000).unwrap().is_empty());
    let mut invalid = submission(52);
    invalid.provenance.config_revision = 0;
    assert_eq!(db.admit_bot(&invalid), Err(BotLedgerError::Invalid));
    invalid = submission(52);
    invalid.input.text = "x".repeat(4 * 1024 * 1024);
    assert_eq!(db.admit_bot(&invalid), Err(BotLedgerError::Invalid));
    assert!(db.intent(invalid.request.run).unwrap().is_none());
    // Queue listing must not decode or fetch current-task payloads.
    db.conn()
        .execute(
            "UPDATE bot_admissions SET payload_json='{}' WHERE queue_sequence=1",
            [],
        )
        .unwrap();
    assert_eq!(db.queued_bots(0, 1).unwrap()[0].sequence, 1);
    assert!(db.bot_admission(fixtures::run_id(1)).is_err());
}

#[test]
fn rollback_after_second_insert_and_reply_loss_keep_admission_atomic() {
    for at in Moment::ALL {
        let (mut db, _) = memory();
        let s = submission(1);
        let point = CrashPoint {
            op: WriteOp::AdmitBot,
            at,
        };
        db.arm_crash(point);
        assert_eq!(
            db.admit_bot(&s),
            Err(BotLedgerError::Ledger(LedgerError::Crashed(point)))
        );
        let committed = at == Moment::AfterCommit;
        assert_eq!(db.intent(s.request.run).unwrap().is_some(), committed);
        assert_eq!(
            db.bot_admission(s.request.run).unwrap().is_some(),
            committed
        );
        db.restart_in_memory();
        assert_eq!(db.admit_bot(&s).unwrap().sequence, 1);
        assert_eq!(db.intents().unwrap().len(), 1);
    }
    let (mut db, _) = memory();
    db.conn().execute_batch("CREATE TRIGGER reject_bot BEFORE INSERT ON bot_admissions BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(db.admit_bot(&submission(1)).is_err());
    assert!(db.intent(fixtures::run_id(1)).unwrap().is_none());
}

#[test]
fn pause_is_monotonic_and_existing_queued_capture_survives_resume() {
    let (mut db, _) = memory();
    let s = submission(1);
    db.admit_bot(&s).unwrap();
    let p = pause(&s, 2, true);
    db.apply_bot_barrier(&p).unwrap();
    assert_eq!(db.apply_bot_barrier(&p).unwrap(), p);
    let mut changed = p.clone();
    changed.paused = false;
    assert_eq!(
        db.apply_bot_barrier(&changed),
        Err(BotLedgerError::Conflict)
    );
    assert_eq!(
        db.apply_bot_barrier(&pause(&s, 1, false)),
        Err(BotLedgerError::Stale)
    );
    assert_eq!(
        db.reserve_bot(s.request.run).unwrap(),
        BotReservation::Paused
    );
    assert_eq!(db.admit_bot(&submission(2)), Err(BotLedgerError::Stale));
    let mut new = submission(3);
    new.provenance.config_revision = 2;
    assert_eq!(db.admit_bot(&new), Err(BotLedgerError::Fenced));
    db.restart_in_memory();
    assert_eq!(db.bot_barrier(p.owner, p.bot).unwrap(), Some(p));
    db.apply_bot_barrier(&pause(&s, 3, false)).unwrap();
    assert_eq!(
        db.reserve_bot(s.request.run).unwrap(),
        BotReservation::Acquired
    );
    assert_eq!(
        db.bot_admission(s.request.run).unwrap().unwrap().submission,
        s
    );
}

#[test]
fn reservations_enforce_one_per_bot_two_per_device_and_hold_unknown_work() {
    let (mut db, _) = memory();
    let a = submission(1);
    let b = submission(2);
    let mut c = submission(3);
    c.provenance.bot = BotId::mint();
    let mut d = submission(4);
    d.provenance.bot = BotId::mint();
    d.provenance.owner.user_scope = Default::default();
    let mut e = submission(5);
    e.provenance.owner.device = koru_delegate_core::DeviceId::from_uuid(Uuid::new_v4());
    e.request.executing_device = e.provenance.owner.device;
    for s in [&a, &b, &c, &d, &e] {
        db.admit_bot(s).unwrap();
    }
    assert_eq!(
        db.reserve_bot(a.request.run).unwrap(),
        BotReservation::Acquired
    );
    assert_eq!(
        db.reserve_bot(a.request.run).unwrap(),
        BotReservation::AlreadyOwned
    );
    assert_eq!(
        db.reserve_bot(b.request.run).unwrap(),
        BotReservation::BotBusy
    );
    assert_eq!(
        db.reserve_bot(c.request.run).unwrap(),
        BotReservation::Acquired
    );
    assert_eq!(
        db.reserve_bot(d.request.run).unwrap(),
        BotReservation::HostBusy
    );
    assert_eq!(
        db.reserve_bot(e.request.run).unwrap(),
        BotReservation::Acquired
    );
    db.satisfy_intent(a.request.run, fixtures::document(8))
        .unwrap();
    let opened = db.open_attempt(a.request.run).unwrap();
    assert_eq!(
        db.settle_bot(a.request.run, BotSettlement::Settled),
        Err(BotLedgerError::Fenced)
    );
    db.restart_in_memory();
    assert_eq!(
        db.reserve_bot(b.request.run).unwrap(),
        BotReservation::BotBusy
    );
    db.advance(opened.run, opened.generation, AttemptPhase::Interrupted)
        .unwrap();
    assert_eq!(
        db.settle_bot(a.request.run, BotSettlement::Settled),
        Err(BotLedgerError::Fenced)
    );
    db.retire(opened.run, opened.generation).unwrap();
    db.settle_bot(a.request.run, BotSettlement::Settled)
        .unwrap();
    assert_eq!(
        db.reserve_bot(b.request.run).unwrap(),
        BotReservation::Acquired
    );
    db.settle_bot(b.request.run, BotSettlement::Refused)
        .unwrap();
    assert_eq!(
        db.intent(b.request.run).unwrap().unwrap().state,
        crate::IntentState::Abandoned
    );
}

#[test]
fn active_clock_excludes_wait_preserves_restart_and_never_resets_budget() {
    let (mut db, time) = memory();
    let s = submission(1);
    let a = running(&mut db, &s);
    time.set(1100);
    db.activate_bot(a.run, a.attempt, a.generation).unwrap();
    assert_eq!(
        db.bot_admission(a.run).unwrap().unwrap().active_since_ms,
        Some(1000)
    );
    time.set(1500);
    db.set_bot_active(a.run, a.attempt, a.generation, false)
        .unwrap();
    time.set(900000);
    db.set_bot_active(a.run, a.attempt, a.generation, false)
        .unwrap();
    assert_eq!(
        db.bot_admission(a.run).unwrap().unwrap().active_elapsed_ms,
        500
    );
    db.restart_in_memory();
    db.set_bot_active(a.run, a.attempt, a.generation, true)
        .unwrap();
    time.set(900200);
    assert_eq!(
        db.bot_admission(a.run)
            .unwrap()
            .unwrap()
            .active_elapsed_at(900200),
        700
    );
    time.set(899999);
    db.set_bot_active(a.run, a.attempt, a.generation, false)
        .unwrap();
    assert!(db.bot_admission(a.run).unwrap().unwrap().active_elapsed_ms >= BOT_ACTIVE_BUDGET_MS);
    assert_eq!(
        db.set_bot_active(a.run, a.attempt, a.generation, true),
        Err(BotLedgerError::Fenced)
    );
    assert_eq!(
        db.settle_bot(a.run, BotSettlement::Settled),
        Err(BotLedgerError::Fenced)
    );
    retired(&mut db, &a);
    db.settle_bot(a.run, BotSettlement::Settled).unwrap();
    assert_eq!(
        db.activate_bot(a.run, a.attempt, a.generation),
        Err(BotLedgerError::Fenced)
    );
}

#[test]
fn unknown_attempt_keeps_capacity_until_the_execution_owner_proves_retirement() {
    let (mut db, _) = memory();
    let s = submission(1);
    let a = running(&mut db, &s);
    let queued = submission(2);
    db.admit_bot(&queued).unwrap();
    db.advance(a.run, a.generation, AttemptPhase::Unknown)
        .unwrap();
    db.restart_in_memory();
    assert_eq!(
        db.settle_bot(a.run, BotSettlement::Settled),
        Err(BotLedgerError::Fenced)
    );
    assert_eq!(
        db.reserve_bot(queued.request.run).unwrap(),
        BotReservation::BotBusy
    );
    // Retirement is the execution owner's explicit fencing fact, never an
    // inference this ledger makes from an Unknown phase or elapsed deadline.
    db.retire(a.run, a.generation).unwrap();
    db.settle_bot(a.run, BotSettlement::Settled).unwrap();
    assert_eq!(
        db.reserve_bot(queued.request.run).unwrap(),
        BotReservation::Acquired
    );
}

#[test]
fn initial_send_is_immutable_and_delivery_requires_exact_ai_and_attempt_binding() {
    let (mut db, _) = memory();
    let s = submission(1);
    let a = running(&mut db, &s);
    let initial = send(&a);
    let mut receipt = BotDeliveryReceipt {
        run: a.run,
        attempt: a.attempt,
        generation: a.generation,
        chat_id: s.provenance.chat_id,
        ai_attempt_id: s.provenance.ai_attempt_id.unwrap(),
        assistant_message_id: s.provenance.assistant_message_id.unwrap(),
        confirmed_at_ms: 2000,
    };
    assert_eq!(
        db.record_bot_delivery(&receipt),
        Err(BotLedgerError::Fenced)
    );
    db.prepare_bot_initial_send(a.run, &initial).unwrap();
    let mut changed = initial.clone();
    changed.fingerprint[0] ^= 1;
    assert_eq!(
        db.prepare_bot_initial_send(a.run, &changed),
        Err(BotLedgerError::Fenced)
    );
    changed = initial.clone();
    changed.attempt = RunAttemptId::mint();
    assert_eq!(
        db.prepare_bot_initial_send(a.run, &changed),
        Err(BotLedgerError::Fenced)
    );
    let mut ack = initial.clone();
    ack.turn = Some(fixtures::v4(0xc2).to_string().parse().unwrap());
    db.acknowledge_bot_initial_send(a.run, &ack).unwrap();
    assert_eq!(db.prepare_bot_initial_send(a.run, &initial).unwrap(), ack);
    assert_eq!(db.acknowledge_bot_initial_send(a.run, &ack).unwrap(), ack);
    let mut wrong = receipt.clone();
    wrong.assistant_message_id = Uuid::new_v4();
    assert_eq!(db.record_bot_delivery(&wrong), Err(BotLedgerError::Fenced));
    db.record_bot_delivery(&receipt).unwrap();
    db.record_bot_delivery(&receipt).unwrap();
    receipt.confirmed_at_ms += 1;
    assert_eq!(
        db.record_bot_delivery(&receipt),
        Err(BotLedgerError::Conflict)
    );
    retired(&mut db, &a);
    db.settle_bot(a.run, BotSettlement::Settled).unwrap();
    // An ordinary caller must not be able to reset this Bot task's first send.
    let fresh = db.open_attempt(a.run).unwrap();
    assert_eq!(
        db.activate_bot(a.run, fresh.attempt, fresh.generation),
        Err(BotLedgerError::Fenced)
    );
}

#[test]
fn every_bot_write_keeps_the_existing_crash_commit_boundary() {
    for op in [
        WriteOp::ApplyBotBarrier,
        WriteOp::ReserveBot,
        WriteOp::ActivateBot,
        WriteOp::SetBotActive,
        WriteOp::SettleBot,
        WriteOp::PrepareBotInitialSend,
        WriteOp::AcknowledgeBotInitialSend,
        WriteOp::RecordBotDelivery,
    ] {
        for at in Moment::ALL {
            let (mut db, time) = memory();
            let s = submission(1);
            let a = running(&mut db, &s);
            let initial = send(&a);
            let mut ack = initial.clone();
            ack.turn = Some(fixtures::v4(0xc2).to_string().parse().unwrap());
            if matches!(
                op,
                WriteOp::AcknowledgeBotInitialSend | WriteOp::RecordBotDelivery
            ) {
                db.prepare_bot_initial_send(a.run, &initial).unwrap();
            }
            if op == WriteOp::RecordBotDelivery {
                db.acknowledge_bot_initial_send(a.run, &ack).unwrap();
            }
            if op == WriteOp::SettleBot {
                retired(&mut db, &a);
            }
            let queued = submission(2);
            db.admit_bot(&queued).unwrap();
            if op == WriteOp::ReserveBot {
                retired(&mut db, &a);
                db.settle_bot(a.run, BotSettlement::Settled).unwrap();
            }
            // Activate has a separately reserved, journaled first attempt.
            let activate = if op == WriteOp::ActivateBot {
                retired(&mut db, &a);
                db.settle_bot(a.run, BotSettlement::Settled).unwrap();
                db.satisfy_intent(queued.request.run, fixtures::document(9))
                    .unwrap();
                db.reserve_bot(queued.request.run).unwrap();
                Some(db.open_attempt(queued.request.run).unwrap())
            } else {
                None
            };
            let barrier = pause(&s, 2, true);
            let receipt = BotDeliveryReceipt {
                run: a.run,
                attempt: a.attempt,
                generation: a.generation,
                chat_id: s.provenance.chat_id,
                ai_attempt_id: s.provenance.ai_attempt_id.unwrap(),
                assistant_message_id: s.provenance.assistant_message_id.unwrap(),
                confirmed_at_ms: 2000,
            };
            let action = |db: &mut RunsDb| -> Result<(), BotLedgerError> {
                match op {
                    WriteOp::ApplyBotBarrier => db.apply_bot_barrier(&barrier).map(|_| ()),
                    WriteOp::ReserveBot => db.reserve_bot(queued.request.run).map(|_| ()),
                    WriteOp::ActivateBot => {
                        let a = activate.unwrap();
                        db.activate_bot(a.run, a.attempt, a.generation)
                    }
                    WriteOp::SetBotActive => {
                        db.set_bot_active(a.run, a.attempt, a.generation, false)
                    }
                    WriteOp::SettleBot => db.settle_bot(a.run, BotSettlement::Settled),
                    WriteOp::PrepareBotInitialSend => {
                        db.prepare_bot_initial_send(a.run, &initial).map(|_| ())
                    }
                    WriteOp::AcknowledgeBotInitialSend => {
                        db.acknowledge_bot_initial_send(a.run, &ack).map(|_| ())
                    }
                    WriteOp::RecordBotDelivery => db.record_bot_delivery(&receipt).map(|_| ()),
                    _ => unreachable!(),
                }
            };
            let before = db.bot_admission(a.run).unwrap();
            let before_queue = db.bot_admission(queued.request.run).unwrap();
            time.set(2000);
            let point = CrashPoint { op, at };
            db.arm_crash(point);
            assert_eq!(
                action(&mut db),
                Err(BotLedgerError::Ledger(LedgerError::Crashed(point))),
                "{point:?}"
            );
            if at != Moment::AfterCommit {
                assert_eq!(db.bot_admission(a.run).unwrap(), before);
                assert_eq!(db.bot_admission(queued.request.run).unwrap(), before_queue);
                assert!(
                    db.bot_barrier(barrier.owner, barrier.bot)
                        .unwrap()
                        .is_none()
                );
            }
            db.restart_in_memory();
            action(&mut db).unwrap();
            let settled = db.bot_admission(a.run).unwrap();
            let queue = db.bot_admission(queued.request.run).unwrap();
            action(&mut db).unwrap();
            assert_eq!(db.bot_admission(a.run).unwrap(), settled);
            assert_eq!(db.bot_admission(queued.request.run).unwrap(), queue);
        }
    }
}

#[test]
fn execution_projection_and_reserved_candidates_keep_recovery_identity_without_payload() {
    let (mut db, _) = memory();
    let s = submission(61);
    let admission = db.admit_bot(&s).unwrap();
    let projection = db.bot_execution(s.request.run).unwrap().unwrap();
    assert_eq!(projection, admission.execution());
    assert_eq!(
        db.reconcile_bot_execution(&key(&s)).unwrap(),
        Some(projection.clone())
    );
    assert_eq!(
        db.reserve_bot(s.request.run).unwrap(),
        BotReservation::Acquired
    );
    let active = db.active_bot_executions(s.provenance.owner.device).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(
        active[0].state,
        koru_delegate_core::BotAdmissionState::Reserved
    );
    assert_eq!(db.queued_bots(0, 1).unwrap()[0].run, s.request.run);
    assert!(!format!("{:?}", active[0]).contains("PRIVATE-TASK-SENTINEL"));
}

fn delivered(db: &mut RunsDb) -> (BotSubmission, BotDeliveryReceipt) {
    let s = submission(1);
    let a = running(db, &s);
    let mut initial = send(&a);
    db.prepare_bot_initial_send(a.run, &initial).unwrap();
    initial.turn = Some(fixtures::v4(0xc2).to_string().parse().unwrap());
    db.acknowledge_bot_initial_send(a.run, &initial).unwrap();
    let receipt = BotDeliveryReceipt {
        run: a.run,
        attempt: a.attempt,
        generation: a.generation,
        chat_id: s.provenance.chat_id,
        ai_attempt_id: s.provenance.ai_attempt_id.unwrap(),
        assistant_message_id: s.provenance.assistant_message_id.unwrap(),
        confirmed_at_ms: 2000,
    };
    (s, receipt)
}

#[test]
fn delivery_publication_identity_is_atomic_unique_and_survives_lost_ack() {
    for moment in Moment::ALL {
        let (mut db, _) = memory();
        let (_, receipt) = delivered(&mut db);
        let point = CrashPoint {
            op: WriteOp::RecordBotDelivery,
            at: moment,
        };
        db.arm_crash(point);
        assert!(
            matches!(db.record_bot_delivery(&receipt), Err(BotLedgerError::Ledger(LedgerError::Crashed(p))) if p == point)
        );
        let after = db.bot_admission(receipt.run).unwrap().unwrap();
        assert_eq!(after.delivery.is_some(), moment == Moment::AfterCommit);
        assert_eq!(
            after.delivery_publication.is_some(),
            moment == Moment::AfterCommit
        );
        db.restart_in_memory();
        let event = db.record_bot_delivery(&receipt).unwrap();
        if let Some(retained) = after.delivery_publication {
            assert_eq!(event, retained);
        }
        assert_eq!(db.record_bot_delivery(&receipt).unwrap(), event);
        let mut changed = receipt.clone();
        changed.confirmed_at_ms += 1;
        assert_eq!(
            db.record_bot_delivery(&changed),
            Err(BotLedgerError::Conflict)
        );
        assert_eq!(
            db.bot_admission(receipt.run)
                .unwrap()
                .unwrap()
                .delivery_publication,
            Some(event)
        );
    }
}

#[test]
fn populated_runs_five_delivery_migrates_and_repairs_once_with_exact_receipt() {
    let (mut db, _) = memory();
    let (submission, receipt) = delivered(&mut db);
    db.record_bot_delivery(&receipt).unwrap();
    // Remove later additions to construct the actual v5 shape,
    // retaining populated canonical v5 admission, attempt and delivery columns.
    db.conn().execute_batch("DROP TABLE bot_memory_edit_allocations; DROP TABLE native_result_intents; DROP TABLE bot_policy_barriers; DROP INDEX bot_delivery_publication_identity; ALTER TABLE bot_admissions DROP COLUMN delivery_publication_event_id; PRAGMA user_version=5;").unwrap();
    db.migrate_in_memory();
    db.migrate_in_memory();
    let migrated = db.bot_admission(receipt.run).unwrap().unwrap();
    assert_eq!(migrated.submission, submission);
    assert_eq!(migrated.delivery, Some(receipt.clone()));
    assert_eq!(migrated.delivery_publication, None);
    let mut changed = receipt.clone();
    changed.ai_attempt_id = Uuid::new_v4();
    assert_eq!(
        db.record_bot_delivery(&changed),
        Err(BotLedgerError::Conflict)
    );
    assert_eq!(
        db.bot_admission(receipt.run)
            .unwrap()
            .unwrap()
            .delivery_publication,
        None
    );
    let event = db.record_bot_delivery(&receipt).unwrap();
    db.restart_in_memory();
    assert_eq!(db.record_bot_delivery(&receipt).unwrap(), event);
    assert_eq!(db.schema_version().unwrap(), crate::RUNS_SCHEMA_VERSION);
}

#[test]
fn legacy_delivery_id_repair_rolls_back_or_reuses_committed_identity() {
    for moment in Moment::ALL {
        let (mut db, _) = memory();
        let (_, receipt) = delivered(&mut db);
        db.record_bot_delivery(&receipt).unwrap();
        db.conn()
            .execute(
                "UPDATE bot_admissions SET delivery_publication_event_id=NULL",
                [],
            )
            .unwrap();
        let point = CrashPoint {
            op: WriteOp::RecordBotDelivery,
            at: moment,
        };
        db.arm_crash(point);
        assert!(db.record_bot_delivery(&receipt).is_err());
        let known = db
            .bot_admission(receipt.run)
            .unwrap()
            .unwrap()
            .delivery_publication;
        assert_eq!(known.is_some(), moment == Moment::AfterCommit);
        db.restart_in_memory();
        let event = db.record_bot_delivery(&receipt).unwrap();
        if let Some(known) = known {
            assert_eq!(known, event);
        }
        assert_eq!(db.record_bot_delivery(&receipt).unwrap(), event);
    }
}

#[test]
fn delivery_publication_constraints_and_decoder_refuse_corruption() {
    let (mut db, _) = memory();
    let (_, receipt) = delivered(&mut db);
    let good = koru_delegate_core::EventId::mint();
    assert!(
        db.conn()
            .execute(
                "UPDATE bot_admissions SET delivery_publication_event_id=?1",
                [good.to_string()]
            )
            .is_err()
    );
    db.record_bot_delivery(&receipt).unwrap();
    for bad in [
        "",
        "not-an-id",
        "00000000-0000-0000-0000-000000000000",
        "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
        "00000000-0000-7000-8000-000000000000",
    ] {
        assert!(
            db.conn()
                .execute(
                    "UPDATE bot_admissions SET delivery_publication_event_id=?1",
                    [bad]
                )
                .is_err()
        );
    }
    db.conn().execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE bot_admissions SET delivery_publication_event_id='not-an-id'; PRAGMA ignore_check_constraints=OFF;").unwrap();
    assert!(matches!(
        db.bot_admission(receipt.run),
        Err(BotLedgerError::Ledger(LedgerError::Corrupt { .. }))
    ));
}

#[test]
fn unsettled_work_of_one_bot_is_its_live_admission_and_a_bounded_queue_never_its_history() {
    let (mut db, _) = memory();
    let executing = submission(1);
    let owner = executing.provenance.owner;
    let bot = executing.provenance.bot;
    let attempt = running(&mut db, &executing);
    for n in 2..=4 {
        db.admit_bot(&submission(n)).unwrap();
    }
    // Another Bot's queued work is never this Bot's.
    let mut other = submission(5);
    other.provenance.bot = BotId::from_uuid(fixtures::v4(0xb9));
    db.admit_bot(&other).unwrap();
    // Three thousand settled admissions of the same Bot.
    {
        let conn = db.conn();
        let payload: String = conn
            .query_row(
                "SELECT payload_json FROM bot_admissions WHERE run_id=?1",
                [executing.request.run.to_storage()],
                |r| r.get(0),
            )
            .unwrap();
        for n in 0..3000i64 {
            let run = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO run_intents(run_id,workspace_id,payload_json,state,created_at_ms,updated_at_ms) VALUES(?1,?2,'{}','abandoned',1,1)",
                rusqlite::params![run, executing.workspace.to_storage()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO bot_admissions(run_id,queue_sequence,user_scope,device_id,bot_id,request_id,config_revision,policy_revision,payload_json,state,created_at_ms,updated_at_ms)
                 VALUES(?1,?2,?3,?4,?5,?6,1,1,?7,'settled',1,1)",
                rusqlite::params![
                    run,
                    10_000 + n,
                    owner.user_scope.to_storage(),
                    owner.device.to_storage(),
                    bot.to_storage(),
                    Uuid::new_v4().to_string(),
                    payload,
                ],
            )
            .unwrap();
        }
    }
    let (all, more) = db.unsettled_bot(owner, bot, 64).unwrap();
    assert!(!more);
    assert_eq!(
        all.iter().map(|r| r.state).collect::<Vec<_>>(),
        vec![
            BotAdmissionState::Executing,
            BotAdmissionState::Queued,
            BotAdmissionState::Queued,
            BotAdmissionState::Queued
        ]
    );
    assert_eq!(all[0].run, executing.request.run);
    assert!(all.iter().all(|r| r.bot == bot && r.owner == owner));
    assert!(all[1..].windows(2).all(|w| w[0].sequence < w[1].sequence));
    let (page, more) = db.unsettled_bot(owner, bot, 1).unwrap();
    assert!(more);
    assert_eq!(page.len(), 2);
    // A foreign owner, or a Bot with nothing unsettled, reads empty.
    let mut foreign = owner;
    foreign.user_scope = Default::default();
    assert_eq!(db.unsettled_bot(foreign, bot, 64).unwrap(), (vec![], false));
    // Both reads use their named index and never sort or walk settled rows.
    let (execution, queue) = super::repository::unsettled_queries();
    for (query, index) in [
        (execution, "bot_admissions_execution"),
        (queue, "bot_admissions_queue"),
    ] {
        let plan = db
            .conn()
            .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
            .unwrap()
            .query_map(
                rusqlite::params_from_iter(
                    [
                        rusqlite::types::Value::from(owner.user_scope.to_storage()),
                        owner.device.to_storage().into(),
                        bot.to_storage().into(),
                        65i64.into(),
                    ]
                    .into_iter()
                    .take(if index == "bot_admissions_queue" {
                        4
                    } else {
                        3
                    }),
                ),
                |r| r.get::<_, String>(3),
            )
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join(" ");
        assert!(plan.contains(index), "{plan}");
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
        assert!(!plan.contains("sqlite_autoindex"), "{plan}");
    }
    // Settled work leaves the read; a settled Bot reads as settled.
    retired(&mut db, &attempt);
    db.settle_bot(executing.request.run, BotSettlement::Settled)
        .unwrap();
    for n in 2..=4 {
        db.settle_bot(submission(n).request.run, BotSettlement::Settled)
            .unwrap();
    }
    assert_eq!(db.unsettled_bot(owner, bot, 64).unwrap(), (vec![], false));
}
