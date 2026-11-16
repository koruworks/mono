// SPDX-License-Identifier: MPL-2.0

//! The decision half of a private operation: cohorts `m5_private_ledger`,
//! `m5_private_restart`, `m5_private_occupancy`, `m5_decision_kind_sites` and
//! the decision half of `m5_private_operation_ids`. In-memory ledgers, a fake
//! clock, real owners; nothing here calls a vendor or holds content.
use super::*;
use crate::private_effects::tests::{capture_grant, next_barrier, proposal};
use crate::{PrivateEffectOutcome, PrivateLedgerError};
use lifecycle::expected;
use std::sync::{Arc, atomic::AtomicI64, atomic::Ordering};

const DISPATCHED: PrivateEffectTransition = PrivateEffectTransition::Dispatched {};
const CONFIRMED: PrivateEffectTransition = PrivateEffectTransition::Confirmed {
    outcome_bytes: 12,
    withheld: false,
};
const REFUSED: PrivateEffectTransition = PrivateEffectTransition::Refused {
    code: PrivateEffectRefusal::CredentialLocked,
};
const UNKNOWN: PrivateEffectTransition = PrivateEffectTransition::Unknown {};

/// A private question, prepared and published, with its cleanup intent frozen
/// (the state `lifecycle::koru_enrollment_ready` builds for an edit).
fn asked() -> (Harness, DecisionRecord, Arc<AtomicI64>) {
    let clock = Arc::new(AtomicI64::new(1000));
    let mut h = Harness::with_db(RunsDb::in_memory(Box::new(fixtures::FakeClock(
        clock.clone(),
    ))));
    h.private();
    let prepared =
        h.db.prepare_private_decision(&h.binding, &h.proof, h.boot, &projection::base(&h))
            .unwrap();
    let f = h.origin();
    let initial =
        h.db.next_decision_projection(f.owner, f.run)
            .unwrap()
            .unwrap();
    projection::apply(&mut h, &initial);
    let record = crate::test_support::request_enrollment(&mut h.db, &prepared).unwrap();
    assert_eq!(record.receipt.decision, prepared.receipt.decision);
    (h, record, clock)
}

/// Allowed, resumed in its recorded successor, and that successor's
/// continuation acknowledged: the exact retry may now consume.
fn allowed() -> (Harness, DecisionRecord, BotExecutionFence, Arc<AtomicI64>) {
    let (mut h, record, clock) = asked();
    let record = crate::test_support::record_enrollment(&mut h.db, &record).unwrap();
    let record = crate::test_support::stop_origin(&mut h.db, &record, 1000).unwrap();
    let host = HostInstanceId::from_uuid(uuid::Uuid::new_v4());
    let record = crate::test_support::bind_successor(
        &mut h.db,
        &record,
        host,
        SessionId::mint(),
        &h.proof,
        h.boot,
    )
    .unwrap();
    let (record, fence) =
        crate::test_support::acknowledge_successor(&mut h.db, &record, AgentTurnId::mint())
            .unwrap();
    assert_eq!(record.receipt.state.kind, DecisionKind::PrivateOperation);
    assert_eq!(
        (
            record.receipt.state.authorization,
            record.receipt.state.phase
        ),
        (DecisionAuthorization::Granted, DecisionPhase::AwaitRetry)
    );
    (h, record, fence, clock)
}

fn consume(
    h: &mut Harness,
    record: &DecisionRecord,
    fence: &BotExecutionFence,
    dispatcher: PrivateDispatcherId,
) -> Result<PrivateEffectOutcome, PrivateLedgerError> {
    let proposal = h.proposal();
    h.db.consume_private_decision(
        &expected(record),
        DecisionTransitionId::mint(),
        dispatcher,
        &proposal,
        fence,
        &h.proof,
        h.boot,
    )
    .unwrap()
    .outcome
}

/// Consumed by `dispatcher`, the effect at `prepared`.
fn consumed() -> (
    Harness,
    DecisionRecord,
    BotExecutionFence,
    PrivateDispatcherId,
    PrivateEffectKey,
) {
    let (mut h, record, fence, _clock) = allowed();
    let dispatcher = PrivateDispatcherId::mint();
    let first = consume(&mut h, &record, &fence, dispatcher).unwrap();
    assert_eq!(first.disposition, PrivateEffectDisposition::Execute);
    let key = first.record.key.clone();
    let record = h.read(record.receipt.decision).unwrap();
    (h, record, fence, dispatcher, key)
}

fn advance(
    h: &mut Harness,
    key: &PrivateEffectKey,
    dispatcher: PrivateDispatcherId,
    transition: PrivateEffectTransition,
) -> PrivateEffectOutcome {
    h.db.advance_private_effect(key, dispatcher, &transition)
        .unwrap()
        .outcome
        .unwrap()
}

fn count(h: &Harness, table: &str) -> i64 {
    h.count(table)
}

#[test]
fn m5_private_ledger_prepare_writes_a_content_free_question_and_replays_first() {
    let mut h = Harness::new();
    h.private();
    let base = projection::base(&h);
    let first =
        h.db.prepare_private_decision(&h.binding, &h.proof, h.boot, &base)
            .unwrap();
    assert_eq!(first.receipt.state.kind, DecisionKind::PrivateOperation);
    assert_eq!(
        (first.receipt.state.authorization, first.receipt.state.phase),
        (DecisionAuthorization::Pending, DecisionPhase::Publishing)
    );
    assert_eq!(
        first.wait.reason,
        BotWaitReason::Decision {
            decision: first.receipt.decision
        }
    );
    assert_eq!(first.receipt.binding, h.binding);
    assert!(first.receipt.deadline.native_deadline_at_ms.is_none());
    assert_eq!(
        first.receipt.deadline.deadline_at_ms,
        first.receipt.deadline.created_at_ms + DECISION_WAIT_MILLIS
    );
    // The scalar columns repeat the binding's identities; no companion table.
    let (kind, operation, prompt): (String, Option<String>, Option<String>) =
        h.db.conn()
            .query_row(
                "SELECT kind,operation_id,native_prompt_id FROM bot_decisions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
    assert_eq!(kind, "private_operation");
    assert_eq!(operation, Some(h.proposal().operation_id.to_string()));
    assert_eq!(prompt, None);
    assert_eq!(
        count(&h, "bot_private_effects"),
        0,
        "no effect before a consume"
    );
    assert_eq!(count(&h, "native_result_intents"), 0);
    assert_eq!(count(&h, "native_decision_leases"), 0);

    // Replay first: the same binding answers the same decision, under any
    // boot and even after the policy moved, and writes nothing.
    let rows = (
        count(&h, "bot_decisions"),
        count(&h, "bot_decision_publications"),
    );
    let again =
        h.db.prepare_private_decision(&h.binding, &h.proof, DelegateBootId::mint(), &base)
            .unwrap();
    assert_eq!(again, first);
    h.revoke();
    assert_eq!(
        h.db.prepare_private_decision(&h.binding, &h.proof, h.boot, &base)
            .unwrap(),
        first
    );
    assert_eq!(
        (
            count(&h, "bot_decisions"),
            count(&h, "bot_decision_publications")
        ),
        rows
    );
    // The lookups find it by its operation id, each under its own kind.
    let f = h.origin();
    let operation = h.proposal().operation_id;
    assert_eq!(
        h.db.bot_private_decision_for_operation(f.owner, f.run, operation)
            .unwrap()
            .unwrap(),
        first
    );
    assert!(
        h.db.bot_decision_for_operation(f.owner, f.run, operation)
            .unwrap()
            .is_none(),
        "the edit lookup never answers a private decision"
    );
}

#[test]
fn m5_private_ledger_prepare_is_atomic_across_decision_wait_and_control() {
    for moment in crate::Moment::ALL {
        let mut h = Harness::new();
        h.private();
        let base = projection::base(&h);
        h.db.arm_crash(crate::CrashPoint {
            op: crate::WriteOp::RecordDecisionProjection,
            at: moment,
        });
        assert!(
            h.db.prepare_private_decision(&h.binding, &h.proof, h.boot, &base)
                .is_err()
        );
        let committed = i64::from(moment == crate::Moment::AfterCommit);
        for table in [
            "bot_decisions",
            "bot_wait_runtime",
            "bot_decision_publications",
        ] {
            assert_eq!(count(&h, table), committed, "{table} at {moment:?}");
        }
        h.db.restart_in_memory();
        h.db.prepare_private_decision(&h.binding, &h.proof, h.boot, &base)
            .unwrap();
        for table in [
            "bot_decisions",
            "bot_wait_runtime",
            "bot_decision_publications",
        ] {
            assert_eq!(count(&h, table), 1);
        }
    }
}

#[test]
fn m5_private_ledger_prepare_refuses_granted_routine_owned_and_conflicting() {
    // Both policies hold the grant: no question is needed.
    let mut h = Harness::new();
    h.private();
    let grant = h.proposal().grant();
    let f = h.origin();
    capture_grant(&h.db, f.run, grant);
    let DecisionBindingV1::PrivateOperation(binding) = &mut h.binding else {
        unreachable!()
    };
    binding.captured_policy.connections.insert(grant);
    let granted = next_barrier(&mut h.db, &h.proof.global.clone(), Some(grant), true);
    binding.global_policy = DecisionGlobalPolicy {
        key: granted.key.clone(),
        config_revision: rev(granted.config_revision),
        policy_revision: rev(granted.policy_revision),
        digest: DecisionDigest::from_bytes(crate::native_results::policy_digest(&granted).unwrap()),
    };
    h.proof.global = granted;
    assert_eq!(
        h.db.prepare_private_decision(&h.binding, &h.proof, h.boot, &projection::base(&h))
            .unwrap_err(),
        PrivateLedgerError::ConnectionGranted
    );
    // The shared body would refuse it too, without the code.
    assert_eq!(
        h.db.prepare_bot_decision(&h.binding, &h.proof, h.boot, &projection::base(&h))
            .unwrap_err(),
        BotLedgerError::Fenced
    );
    assert_eq!(count(&h, "bot_decisions"), 0);

    // Only one side holds it: that is the question (D7).
    for captured in [true, false] {
        let mut h = Harness::new();
        h.private();
        let grant = h.proposal().grant();
        let f = h.origin();
        let DecisionBindingV1::PrivateOperation(binding) = &mut h.binding else {
            unreachable!()
        };
        if captured {
            capture_grant(&h.db, f.run, grant);
            binding.captured_policy.connections.insert(grant);
        } else {
            let current = next_barrier(&mut h.db, &h.proof.global.clone(), Some(grant), true);
            binding.global_policy = DecisionGlobalPolicy {
                key: current.key.clone(),
                config_revision: rev(current.config_revision),
                policy_revision: rev(current.policy_revision),
                digest: DecisionDigest::from_bytes(
                    crate::native_results::policy_digest(&current).unwrap(),
                ),
            };
            h.proof.global = current;
        }
        h.db.prepare_private_decision(&h.binding, &h.proof, h.boot, &projection::base(&h))
            .unwrap();
    }

    // A Routine Run never has a private question (D6).
    {
        let mut h = Harness::new();
        h.private();
        h.routine(true);
        assert_eq!(
            h.db.prepare_private_decision(&h.binding, &h.proof, h.boot, &projection::base(&h))
                .unwrap_err(),
            PrivateLedgerError::RoutineDecisionUnsupported
        );
        // The shared body refuses it too, whatever its own code.
        assert!(
            h.db.prepare_bot_decision(&h.binding, &h.proof, h.boot, &projection::base(&h))
                .is_err()
        );
        assert_eq!(count(&h, "bot_decisions"), 0);
    }

    // Another decision owns the Run.
    let mut h = Harness::new();
    h.private();
    let base = projection::base(&h);
    h.db.prepare_private_decision(&h.binding, &h.proof, h.boot, &base)
        .unwrap();
    let mut second = h.binding.clone();
    let DecisionBindingV1::PrivateOperation(v) = &mut second else {
        unreachable!()
    };
    v.operation_id = uuid::Uuid::new_v4();
    assert_eq!(
        h.db.prepare_private_decision(&second, &h.proof, h.boot, &base)
            .unwrap_err(),
        PrivateLedgerError::DecisionOwned
    );
    // The same operation id with another proposal is a conflict.
    let mut changed = h.binding.clone();
    let DecisionBindingV1::PrivateOperation(v) = &mut changed else {
        unreachable!()
    };
    v.connection_revision = rev(9);
    assert_eq!(
        h.db.prepare_private_decision(&changed, &h.proof, h.boot, &base)
            .unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Conflict)
    );
    // A stale proof is fenced, an edit binding is not a private question.
    let mut h = Harness::new();
    h.private();
    let stale = h.proof.clone();
    h.revoke();
    assert_eq!(
        h.db.prepare_private_decision(&h.binding, &stale, h.boot, &projection::base(&h))
            .unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Fenced)
    );
    let mut h = Harness::new();
    assert_eq!(
        h.db.prepare_private_decision(&h.binding, &h.proof, h.boot, &projection::base(&h))
            .unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Invalid)
    );
    assert_eq!(count(&h, "bot_decisions"), 0);
}

#[test]
fn m5_private_ledger_duplicate_and_stale_answers_follow_the_shared_owner() {
    let (mut h, record, _clock) = asked();
    let record = crate::test_support::record_enrollment(&mut h.db, &record).unwrap();
    let held = crate::test_support::stop_origin(&mut h.db, &record, 1000).unwrap();
    assert_eq!(
        (held.receipt.state.phase, held.wait.phase),
        (DecisionPhase::Held, BotWaitPhase::Retired)
    );
    // A native choice is not an answer to a private question.
    let allow = h.answer_input(&held, true);
    assert_eq!(allow.choice, DecisionAnswerChoice::AllowOnce {});
    let transition = DecisionTransitionId::mint();
    let granted =
        h.db.answer_bot_decision(&allow, transition, &h.proof, h.boot)
            .unwrap();
    assert_eq!(
        (
            granted.receipt.state.authorization,
            granted.receipt.state.phase
        ),
        (DecisionAuthorization::Granted, DecisionPhase::Held)
    );
    // The same answer request replays the same receipt.
    let replay =
        h.db.answer_bot_decision(&allow, transition, &h.proof, h.boot)
            .unwrap();
    assert_eq!(replay.receipt, granted.receipt);
    // A second, different answer after a committed one is refused.
    let mut deny = h.answer_input(&granted, false);
    assert!(
        h.db.answer_bot_decision(&deny, DecisionTransitionId::mint(), &h.proof, h.boot)
            .is_err()
    );
    // A stale revision and a wrong binding digest are refused.
    deny.expected_revision = held.receipt.state.revision;
    assert!(
        h.db.answer_bot_decision(&deny, DecisionTransitionId::mint(), &h.proof, h.boot)
            .is_err()
    );
    assert_eq!(
        h.read(held.receipt.decision).unwrap().receipt,
        granted.receipt
    );

    // An answer before the wait is held is refused, and so is one from
    // another person or after the deadline.
    let (mut h, record, clock) = asked();
    let early = h.answer_input(&record, true);
    assert_eq!(
        h.db.answer_bot_decision(&early, DecisionTransitionId::mint(), &h.proof, h.boot)
            .unwrap_err(),
        BotLedgerError::Stale
    );
    let record = crate::test_support::record_enrollment(&mut h.db, &record).unwrap();
    let held = crate::test_support::stop_origin(&mut h.db, &record, 1000).unwrap();
    let mut stranger = h.answer_input(&held, true);
    stranger.human = ProfileId::from_uuid(uuid::Uuid::new_v4());
    assert!(
        h.db.answer_bot_decision(&stranger, DecisionTransitionId::mint(), &h.proof, h.boot)
            .is_err()
    );
    clock.store(1000 + DECISION_WAIT_MILLIS, Ordering::SeqCst);
    let late = h.answer_input(&held, true);
    assert_eq!(
        h.db.answer_bot_decision(&late, DecisionTransitionId::mint(), &h.proof, h.boot)
            .unwrap_err(),
        BotLedgerError::Fenced
    );
    // The deadline's own request is accepted, exactly as for an edit (O157).
    let ticket = completion::ticket(&h);
    h.db.request_decision_cleanup(
        &ticket,
        DecisionTransitionId::mint(),
        DecisionClosureReason::Expired,
    )
    .unwrap();
    let expired = h.read(held.receipt.decision).unwrap();
    assert_eq!(
        (
            expired.receipt.state.authorization,
            expired.receipt.state.phase
        ),
        (DecisionAuthorization::Expired, DecisionPhase::StopPending)
    );
}

#[test]
fn m5_private_ledger_deny_stops_without_an_effect() {
    let (mut h, record, _clock) = asked();
    let record = crate::test_support::record_enrollment(&mut h.db, &record).unwrap();
    let held = crate::test_support::stop_origin(&mut h.db, &record, 1000).unwrap();
    let deny = h.answer_input(&held, false);
    let denied =
        h.db.answer_bot_decision(&deny, DecisionTransitionId::mint(), &h.proof, h.boot)
            .unwrap();
    assert_eq!(
        (
            denied.receipt.state.authorization,
            denied.receipt.state.phase
        ),
        (DecisionAuthorization::Denied, DecisionPhase::StopPending)
    );
    assert_eq!(count(&h, "bot_private_effects"), 0);
    let f = h.origin();
    assert_eq!(
        h.db.decision_retained_effect(f.owner, f.run, denied.receipt.decision)
            .unwrap(),
        None
    );
}

#[test]
fn m5_private_ledger_consume_is_atomic_and_answers_execute_once() {
    for moment in crate::Moment::ALL {
        let (mut h, record, fence, _clock) = allowed();
        let dispatcher = PrivateDispatcherId::mint();
        let proposal = h.proposal();
        let transition = DecisionTransitionId::mint();
        h.db.arm_crash(crate::CrashPoint {
            op: crate::WriteOp::RecordDecisionProjection,
            at: moment,
        });
        let crashed =
            h.db.consume_private_decision(
                &expected(&record),
                transition,
                dispatcher,
                &proposal,
                &fence,
                &h.proof,
                h.boot,
            )
            .unwrap();
        assert!(crashed.outcome.is_err());
        let committed = moment == crate::Moment::AfterCommit;
        assert_eq!(
            h.read(record.receipt.decision)
                .unwrap()
                .receipt
                .state
                .authorization
                == DecisionAuthorization::Consumed,
            committed
        );
        assert_eq!(count(&h, "bot_private_effects"), i64::from(committed));
        assert_eq!(
            h.db.decision_projection(fence.owner, fence.run, transition)
                .unwrap()
                .is_some(),
            committed
        );
        h.db.restart_in_memory();
        let retried = consume(&mut h, &record, &fence, dispatcher).unwrap();
        assert_eq!(
            retried.disposition == PrivateEffectDisposition::Execute,
            !committed
        );
        let effect = retried.record.clone();
        assert_eq!(effect.stage, PrivateEffectStage::Prepared);
        assert_eq!(effect.decision, Some(record.receipt.decision));
        assert_eq!(effect.binding_digest, Some(record.receipt.binding_digest));
        assert_eq!(effect.dispatcher, dispatcher);
        assert_eq!(effect.operation, proposal);
        assert_eq!(
            (effect.key.attempt, effect.key.generation, effect.key.turn),
            (fence.attempt, fence.generation, fence.turn.unwrap())
        );
        assert!(effect.routine.is_none());
        // Every replay is reconcile-only and records no projection.
        let projections = count(&h, "bot_decision_publications");
        for _ in 0..2 {
            let replay = consume(&mut h, &record, &fence, dispatcher).unwrap();
            assert_eq!(replay.disposition, PrivateEffectDisposition::ReconcileOnly);
            assert_eq!(replay.record, effect);
        }
        assert_eq!(count(&h, "bot_decision_publications"), projections);
        assert_eq!(count(&h, "bot_private_effects"), 1);
        let consumed = h.read(record.receipt.decision).unwrap();
        assert_eq!(
            (
                consumed.receipt.state.authorization,
                consumed.receipt.state.phase
            ),
            (DecisionAuthorization::Consumed, DecisionPhase::Effect)
        );
        assert_eq!(count(&h, "native_result_intents"), 0);
        assert_eq!(count(&h, "native_decision_leases"), 0);
    }
}

#[test]
fn m5_private_ledger_consume_replays_after_revocation_and_refuses_a_changed_proposal() {
    let (mut h, record, fence, dispatcher, _key) = consumed();
    // The decision row moved on; the caller still holds its consume ticket.
    let ticket = {
        let mut ticket = expected(&record);
        ticket.revision = rev(ticket.revision.get() - 1);
        ticket
    };
    h.revoke();
    let proposal = h.proposal();
    let mut later = fence.clone();
    later.attempt = RunAttemptId::mint();
    let replay =
        h.db.consume_private_decision(
            &ticket,
            DecisionTransitionId::mint(),
            dispatcher,
            &proposal,
            &later,
            &h.proof,
            DelegateBootId::mint(),
        )
        .unwrap()
        .outcome
        .unwrap();
    assert_eq!(replay.disposition, PrivateEffectDisposition::ReconcileOnly);
    let mut changed = proposal.clone();
    changed.connection_revision = rev(9);
    let mut other_item = crate::private_effects::tests::proposal(proposal.operation_id);
    other_item.item = ConnectionItemId::mint();
    other_item.request_fingerprint = other_item.fingerprint();
    for wrong in [changed, other_item] {
        assert_eq!(
            h.db.consume_private_decision(
                &ticket,
                DecisionTransitionId::mint(),
                dispatcher,
                &wrong,
                &fence,
                &h.proof,
                h.boot,
            )
            .unwrap()
            .outcome
            .unwrap_err(),
            PrivateLedgerError::Ledger(BotLedgerError::Conflict)
        );
    }
    let mut foreign = fence.clone();
    foreign.bot = BotId::mint();
    assert_eq!(
        h.db.consume_private_decision(
            &ticket,
            DecisionTransitionId::mint(),
            dispatcher,
            &proposal,
            &foreign,
            &h.proof,
            h.boot,
        )
        .unwrap()
        .outcome
        .unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Fenced)
    );
}

#[test]
fn m5_private_ledger_consume_refuses_wrong_consumer_moved_policy_and_deadline() {
    // The origin, another turn and another session are not the acknowledged
    // consumer.
    let (mut h, record, fence, _clock) = allowed();
    let dispatcher = PrivateDispatcherId::mint();
    let mut wrong_turn = fence.clone();
    wrong_turn.turn = Some(AgentTurnId::mint());
    let mut wrong_session = fence.clone();
    wrong_session.session = SessionId::mint();
    let origin = completion::execution_fence(&h.origin());
    for wrong in [wrong_turn, wrong_session, origin] {
        assert!(matches!(
            consume(&mut h, &record, &wrong, dispatcher),
            Err(PrivateLedgerError::Ledger(_))
        ));
    }
    // A stale ticket, a foreign boot.
    let mut stale = expected(&record);
    stale.revision = rev(stale.revision.get() - 1);
    let proposal = h.proposal();
    assert!(
        h.db.consume_private_decision(
            &stale,
            DecisionTransitionId::mint(),
            dispatcher,
            &proposal,
            &fence,
            &h.proof,
            h.boot,
        )
        .unwrap()
        .outcome
        .is_err()
    );
    assert_eq!(
        h.db.consume_private_decision(
            &expected(&record),
            DecisionTransitionId::mint(),
            dispatcher,
            &proposal,
            &fence,
            &h.proof,
            DelegateBootId::mint(),
        )
        .unwrap()
        .outcome
        .unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Fenced)
    );
    assert_eq!(count(&h, "bot_private_effects"), 0);
    assert_eq!(
        h.read(record.receipt.decision)
            .unwrap()
            .receipt
            .state
            .authorization,
        DecisionAuthorization::Granted
    );
    // The right consumer still can.
    assert_eq!(
        consume(&mut h, &record, &fence, dispatcher)
            .unwrap()
            .disposition,
        PrivateEffectDisposition::Execute
    );

    // A policy that moved since the question was asked makes the answer stale.
    let (mut h, record, fence, _clock) = allowed();
    h.revoke();
    assert_eq!(
        consume(&mut h, &record, &fence, dispatcher).unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Fenced)
    );
    // After the deadline nothing is consumed.
    let (mut h, record, fence, clock) = allowed();
    clock.store(1000 + DECISION_WAIT_MILLIS, Ordering::SeqCst);
    assert_eq!(
        consume(&mut h, &record, &fence, dispatcher).unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Fenced)
    );
    assert_eq!(count(&h, "bot_private_effects"), 0);
}

#[test]
fn m5_private_ledger_settles_mid_turn_on_confirmed_and_refused_never_on_unknown() {
    for (steps, reason) in [
        (
            vec![DISPATCHED, CONFIRMED],
            DecisionClosureReason::Completed,
        ),
        (
            vec![
                DISPATCHED,
                PrivateEffectTransition::Confirmed {
                    outcome_bytes: 40,
                    withheld: true,
                },
            ],
            DecisionClosureReason::Completed,
        ),
        (vec![REFUSED], DecisionClosureReason::EffectNotApplied),
        (
            vec![DISPATCHED, REFUSED],
            DecisionClosureReason::EffectNotApplied,
        ),
    ] {
        let (mut h, record, fence, dispatcher, key) = consumed();
        let f = h.origin();
        // While the row is open its dispatcher owns it: no outcome exists.
        assert_eq!(
            h.db.decision_retained_effect(f.owner, f.run, record.receipt.decision)
                .unwrap_err(),
            BotLedgerError::Fenced
        );
        for step in steps {
            advance(&mut h, &key, dispatcher, step);
        }
        let outcome =
            h.db.decision_retained_effect(f.owner, f.run, record.receipt.decision)
                .unwrap()
                .unwrap();
        assert_eq!(
            Some(&outcome),
            h.db.private_effect(key.run, key.operation_id)
                .unwrap()
                .unwrap()
                .retained_effect()
                .as_ref()
        );
        // A fabricated outcome, or another effect's, is refused.
        let fabricated = DecisionRetainedEffect::PrivateConfirmed {
            key: key.clone(),
            outcome_bytes: 99,
            withheld: false,
        };
        assert!(
            h.db.settle_decision_effect(
                &expected(&record),
                DecisionTransitionId::mint(),
                &fabricated
            )
            .is_err()
        );
        let transition = DecisionTransitionId::mint();
        let settled =
            h.db.settle_decision_effect(&expected(&record), transition, &outcome)
                .unwrap();
        assert_eq!(settled.receipt.state.phase, DecisionPhase::Settled);
        assert_eq!(settled.receipt.closure.as_ref().unwrap().reason, reason);
        // A lost reply replays under a new transition and records nothing.
        let projections = count(&h, "bot_decision_publications");
        assert_eq!(
            h.db.settle_decision_effect(&expected(&record), DecisionTransitionId::mint(), &outcome)
                .unwrap()
                .receipt,
            settled.receipt
        );
        assert_eq!(count(&h, "bot_decision_publications"), projections);

        // The fence lifts once the settlement is published: the wait settles
        // with the successor still live, and the Bot can ask or read again.
        let second = proposal(uuid::Uuid::new_v4());
        assert_eq!(
            h.db.prepare_private_effect(dispatcher, &second, &fence, &h.proof)
                .unwrap()
                .outcome
                .unwrap_err(),
            PrivateLedgerError::DecisionOwned
        );
        let pending =
            h.db.decision_projection(f.owner, f.run, transition)
                .unwrap()
                .unwrap();
        let wait = completion::ticket(&h);
        let prepared = projection::prepared(&pending);
        let ack = DecisionProjectionAcknowledgment {
            identity: pending.identity.clone(),
            delta_digest: prepared.delta_digest,
            observed_version: DecisionVersionVector::new(vec![3]).unwrap(),
        };
        projection::apply(&mut h, &pending);
        h.db.settle_decision_wait(&wait, &ack).unwrap();
        assert!(
            h.db.decision_wait_execution(f.owner, f.run)
                .unwrap()
                .is_none()
        );
        assert!(!h.db.current_attempt(f.run).unwrap().unwrap().retired);
        // No grant: the next read is a new question, not a granted read.
        assert_eq!(
            h.db.prepare_private_effect(dispatcher, &second, &fence, &h.proof)
                .unwrap()
                .outcome
                .unwrap_err(),
            PrivateLedgerError::ConnectionNotGranted
        );
    }

    // `unknown` is not a mid-turn settlement.
    let (mut h, record, _fence, dispatcher, key) = consumed();
    advance(&mut h, &key, dispatcher, DISPATCHED);
    advance(&mut h, &key, dispatcher, UNKNOWN);
    let f = h.origin();
    let outcome =
        h.db.decision_retained_effect(f.owner, f.run, record.receipt.decision)
            .unwrap()
            .unwrap();
    assert_eq!(outcome, DecisionRetainedEffect::PrivateUnknown { key });
    assert_eq!(
        h.db.settle_decision_effect(&expected(&record), DecisionTransitionId::mint(), &outcome)
            .unwrap_err(),
        BotLedgerError::Fenced
    );
    assert_eq!(
        h.read(record.receipt.decision).unwrap().receipt.state.phase,
        DecisionPhase::Effect
    );
}

/// Request a person's Stop and record the final cleanup with `outcome`.
fn stop_and_clean(
    h: &mut Harness,
    outcome: Option<DecisionRetainedEffect>,
) -> Result<DecisionRecord, BotLedgerError> {
    let f = h.origin();
    let requested =
        h.db.request_decision_cleanup(
            &completion::ticket(h),
            DecisionTransitionId::mint(),
            DecisionClosureReason::Canceled,
        )
        .unwrap();
    projection::apply(h, &requested);
    let owner =
        h.db.decision_cleanup_ownership(f.owner, f.run)
            .unwrap()
            .unwrap();
    let (ticket, without) = lifecycle::confirmed_cleanup(h, 1000);
    let ConsumerCleanup::Confirmed {
        cleanup_revision,
        target,
        creation_retirement,
        attempt_retirement,
        native_drain,
        ..
    } = without
    else {
        unreachable!()
    };
    let cleanup = ConsumerCleanup::Confirmed {
        cleanup_revision,
        target,
        creation_retirement,
        attempt_retirement,
        native_drain,
        retained_effect_outcome: outcome,
    };
    h.db.record_decision_consumer_cleanup(
        &ticket,
        owner.projection,
        DecisionTransitionId::mint(),
        &cleanup,
    )
}

#[test]
fn m5_private_ledger_cancellation_names_the_final_effect_and_waits_for_an_open_one() {
    // A Stop while the effect is open: cleanup has no outcome to name and is
    // refused with or without one, until the dispatcher closes the row.
    let (mut h, record, _fence, dispatcher, key) = consumed();
    advance(&mut h, &key, dispatcher, DISPATCHED);
    let f = h.origin();
    let requested =
        h.db.request_decision_cleanup(
            &completion::ticket(&h),
            DecisionTransitionId::mint(),
            DecisionClosureReason::Canceled,
        )
        .unwrap();
    projection::apply(&mut h, &requested);
    let owner =
        h.db.decision_cleanup_ownership(f.owner, f.run)
            .unwrap()
            .unwrap();
    assert_eq!(
        h.db.decision_retained_effect(f.owner, f.run, record.receipt.decision)
            .unwrap_err(),
        BotLedgerError::Fenced
    );
    let (ticket, without) = lifecycle::confirmed_cleanup(&mut h, 1000);
    assert!(
        h.db.record_decision_consumer_cleanup(
            &ticket,
            owner.projection,
            DecisionTransitionId::mint(),
            &without
        )
        .is_err()
    );
    // The Stop did not abort the call: the dispatcher records the truth, and
    // the content it got is dropped because the consumer is gone.
    let unknown = advance(&mut h, &key, dispatcher, UNKNOWN);
    assert!(!unknown.deliver);
    let outcome =
        h.db.decision_retained_effect(f.owner, f.run, record.receipt.decision)
            .unwrap()
            .unwrap();
    let ConsumerCleanup::Confirmed {
        cleanup_revision,
        target,
        creation_retirement,
        attempt_retirement,
        native_drain,
        ..
    } = without
    else {
        unreachable!()
    };
    let with = ConsumerCleanup::Confirmed {
        cleanup_revision,
        target,
        creation_retirement,
        attempt_retirement,
        native_drain,
        retained_effect_outcome: Some(outcome),
    };
    let settled =
        h.db.record_decision_consumer_cleanup(
            &ticket,
            owner.projection,
            DecisionTransitionId::mint(),
            &with,
        )
        .unwrap();
    assert_eq!(
        settled.receipt.closure.unwrap().reason,
        DecisionClosureReason::EffectUnknown
    );
    assert_eq!(
        h.db.private_effect(key.run, key.operation_id)
            .unwrap()
            .unwrap()
            .stage,
        PrivateEffectStage::Unknown
    );

    // A final effect is named exactly; a wrong outcome is refused.
    for (step, reason) in [
        (REFUSED, DecisionClosureReason::EffectNotApplied),
        (DISPATCHED, DecisionClosureReason::Completed),
    ] {
        let (mut h, record, _fence, dispatcher, key) = consumed();
        advance(&mut h, &key, dispatcher, step);
        if step == DISPATCHED {
            advance(&mut h, &key, dispatcher, CONFIRMED);
        }
        let f = h.origin();
        let outcome =
            h.db.decision_retained_effect(f.owner, f.run, record.receipt.decision)
                .unwrap()
                .unwrap();
        let settled = stop_and_clean(&mut h, Some(outcome)).unwrap();
        assert_eq!(settled.receipt.closure.unwrap().reason, reason);
    }
    let (mut h, _record, _fence, dispatcher, key) = consumed();
    advance(&mut h, &key, dispatcher, REFUSED);
    assert!(
        stop_and_clean(
            &mut h,
            Some(DecisionRetainedEffect::PrivateUnknown { key: key.clone() })
        )
        .is_err()
    );
}

#[test]
fn m5_private_ledger_an_approval_never_used_closes_without_an_effect() {
    // Allowed, resumed, and the successor never made the call. A person's
    // Stop closes it with the cleanup's own cause and no effect row.
    let (mut h, record, _fence, _clock) = allowed();
    let f = h.origin();
    assert_eq!(
        h.db.decision_retained_effect(f.owner, f.run, record.receipt.decision)
            .unwrap(),
        None
    );
    // An outcome cannot be invented for a decision that was not consumed.
    let (mut probe, _record, _fence, _clock) = allowed();
    assert!(
        stop_and_clean(
            &mut probe,
            Some(DecisionRetainedEffect::PrivateUnknown {
                key: PrivateEffectKey {
                    run: f.run,
                    operation_id: uuid::Uuid::new_v4(),
                    attempt: RunAttemptId::mint(),
                    generation: AttemptGeneration::FIRST,
                    turn: AgentTurnId::mint(),
                }
            })
        )
        .is_err()
    );
    let settled = stop_and_clean(&mut h, None).unwrap();
    assert_eq!(
        settled.receipt.closure.as_ref().unwrap().reason,
        DecisionClosureReason::Canceled
    );
    // A Stop moves the authorization itself to `canceled` (the table admits
    // `granted` in no closing phase). What stays readable, whatever the
    // reason, is "allowed and never consumed": the retained Allow answer and
    // no consume time. That is what a card must read.
    assert_eq!(
        settled.receipt.state.authorization,
        DecisionAuthorization::Canceled
    );
    assert_eq!(
        settled.receipt.answer.as_ref().unwrap().choice,
        DecisionAnswerChoice::AllowOnce {}
    );
    assert_eq!(settled.receipt.consumed_at_ms, None);
    assert_eq!(count(&h, "bot_private_effects"), 0);
}

#[test]
fn m5_private_restart_before_dispatch_closes_not_dispatched_and_settles_at_once() {
    // After consume, before the dispatch mark; the dispatcher's process is
    // gone and the next one announces.
    let (mut h, record, fence, dead, key) = consumed();
    h.db.restart_in_memory();
    let next = PrivateDispatcherId::mint();
    let resolved = h.db.announce_private_dispatcher(fence.owner, next).unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].decision, Some(record.receipt.decision));
    assert_eq!(
        (resolved[0].stage, resolved[0].refusal),
        (
            PrivateEffectStage::Refused,
            Some(PrivateEffectRefusal::NotDispatched)
        )
    );
    assert!(
        resolved[0].dispatched_at_ms.is_none(),
        "the vendor was never called"
    );
    // The row resolution closed settles its decision exactly as an advance.
    let outcome = resolved[0].retained_effect().unwrap();
    let f = h.origin();
    assert_eq!(
        h.db.decision_retained_effect(f.owner, f.run, record.receipt.decision)
            .unwrap(),
        Some(outcome.clone())
    );
    let settled =
        h.db.settle_decision_effect(&expected(&record), DecisionTransitionId::mint(), &outcome)
            .unwrap();
    assert_eq!(
        settled.receipt.closure.unwrap().reason,
        DecisionClosureReason::EffectNotApplied
    );
    // The approval is spent: the exact retry reconciles, the dead instance's
    // advance is stale, and nothing reopens the row.
    let replay = consume(&mut h, &record, &fence, next).unwrap();
    assert_eq!(replay.disposition, PrivateEffectDisposition::ReconcileOnly);
    assert_eq!(replay.record.stage, PrivateEffectStage::Refused);
    assert_eq!(
        h.db.advance_private_effect(&key, dead, &DISPATCHED)
            .unwrap()
            .outcome
            .unwrap_err(),
        PrivateLedgerError::EffectStale
    );
    assert_eq!(
        h.db.advance_private_effect(&key, next, &DISPATCHED)
            .unwrap()
            .outcome
            .unwrap_err(),
        PrivateLedgerError::EffectStale
    );
    assert_eq!(count(&h, "bot_private_effects"), 1);
}

#[test]
fn m5_private_restart_after_dispatch_closes_unknown_and_settles_only_at_cleanup() {
    let (mut h, record, fence, dead, key) = consumed();
    advance(&mut h, &key, dead, DISPATCHED);
    h.db.restart_in_memory();
    // The replacement's own exact retry carries the new instance id, so the
    // consume command itself resolves first and then reconciles.
    let next = PrivateDispatcherId::mint();
    let proposal = h.proposal();
    let reply =
        h.db.consume_private_decision(
            &expected(&record),
            DecisionTransitionId::mint(),
            next,
            &proposal,
            &fence,
            &h.proof,
            h.boot,
        )
        .unwrap();
    assert_eq!(reply.resolved.len(), 1);
    assert_eq!(reply.resolved[0].stage, PrivateEffectStage::Unknown);
    let replay = reply.outcome.unwrap();
    assert_eq!(replay.disposition, PrivateEffectDisposition::ReconcileOnly);
    assert_eq!(replay.record.stage, PrivateEffectStage::Unknown);
    assert_eq!(replay.record.dispatcher, dead);
    // Never resent: no license, no path back, and a late answer is dropped.
    for transition in [DISPATCHED, CONFIRMED, REFUSED] {
        for instance in [dead, next] {
            assert_eq!(
                h.db.advance_private_effect(&key, instance, &transition)
                    .unwrap()
                    .outcome
                    .unwrap_err(),
                PrivateLedgerError::EffectStale
            );
        }
    }
    // `unknown` waits for cleanup, which names it.
    let outcome = replay.record.retained_effect().unwrap();
    assert_eq!(
        h.db.settle_decision_effect(&expected(&record), DecisionTransitionId::mint(), &outcome)
            .unwrap_err(),
        BotLedgerError::Fenced
    );
    let settled = stop_and_clean(&mut h, Some(outcome)).unwrap();
    assert_eq!(
        settled.receipt.closure.unwrap().reason,
        DecisionClosureReason::EffectUnknown
    );
}

#[test]
fn m5_private_restart_after_confirmed_and_at_each_earlier_row_never_reaches_prepared_again() {
    // Before the prepare committed: the retry prepares.
    let mut h = Harness::new();
    h.private();
    h.db.restart_in_memory();
    h.db.prepare_private_decision(&h.binding, &h.proof, h.boot, &projection::base(&h))
        .unwrap();
    // After the answer, before the consume: the consumer's retry consumes,
    // under whatever instance is alive.
    let (mut h, record, fence, _clock) = allowed();
    h.db.restart_in_memory();
    assert!(
        h.db.announce_private_dispatcher(fence.owner, PrivateDispatcherId::mint())
            .unwrap()
            .is_empty()
    );
    let dispatcher = PrivateDispatcherId::mint();
    assert_eq!(
        consume(&mut h, &record, &fence, dispatcher)
            .unwrap()
            .disposition,
        PrivateEffectDisposition::Execute
    );
    // After `confirmed`, before the tool reply left: the receipt is retained
    // and the read is not repeated.
    let (mut h, record, fence, dispatcher, key) = consumed();
    advance(&mut h, &key, dispatcher, DISPATCHED);
    assert!(advance(&mut h, &key, dispatcher, CONFIRMED).deliver);
    h.db.restart_in_memory();
    let next = PrivateDispatcherId::mint();
    assert!(
        h.db.announce_private_dispatcher(fence.owner, next)
            .unwrap()
            .is_empty(),
        "a final row is not swept"
    );
    let replay = consume(&mut h, &record, &fence, next).unwrap();
    assert_eq!(replay.disposition, PrivateEffectDisposition::ReconcileOnly);
    assert_eq!(replay.record.stage, PrivateEffectStage::Confirmed);
    assert!(!replay.deliver);
    // A delegate restart during the vendor call: the same live dispatcher
    // retries its advance until the ledger answers.
    let (mut h, _record, fence, dispatcher, key) = consumed();
    advance(&mut h, &key, dispatcher, DISPATCHED);
    h.db.restart_in_memory();
    assert!(
        h.db.announce_private_dispatcher(fence.owner, dispatcher)
            .unwrap()
            .is_empty(),
        "a live dispatcher's row is never swept"
    );
    assert!(advance(&mut h, &key, dispatcher, CONFIRMED).deliver);
}

#[test]
fn m5_private_operation_ids_decisions_and_effects_cannot_share_an_id_across_kinds() {
    // An edit decision holds the id: a private question under it conflicts.
    let mut h = Harness::new();
    let base = projection::base(&h);
    let edit_binding = h.binding.clone();
    let operation = h.edit().operation_id;
    h.db.prepare_bot_decision(&edit_binding, &h.proof, h.boot, &base)
        .unwrap();
    h.private();
    let DecisionBindingV1::PrivateOperation(v) = &mut h.binding else {
        unreachable!()
    };
    v.operation_id = operation;
    assert_eq!(
        h.db.prepare_private_decision(&h.binding, &h.proof, h.boot, &base)
            .unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Conflict)
    );
    assert_eq!(count(&h, "bot_decisions"), 1);

    // A private question holds the id: an edit question under it conflicts.
    let mut h = Harness::new();
    let base = projection::base(&h);
    let mut edit_binding = h.binding.clone();
    h.private();
    let operation = h.proposal().operation_id;
    h.db.prepare_private_decision(&h.binding, &h.proof, h.boot, &base)
        .unwrap();
    let DecisionBindingV1::KoruEdit(v) = &mut edit_binding else {
        unreachable!()
    };
    v.operation_id = operation;
    assert_eq!(
        h.db.prepare_bot_decision(&edit_binding, &h.proof, h.boot, &base)
            .unwrap_err(),
        BotLedgerError::Conflict
    );
    assert_eq!(count(&h, "bot_decisions"), 1);

    // A page intent holds the id: a private question under it conflicts, and
    // the other way round the page prepare is fenced by the open wait.
    let mut h = Harness::new();
    let page = completion::result_request(&h);
    h.db.prepare_native_result(&page).unwrap();
    h.private();
    let DecisionBindingV1::PrivateOperation(v) = &mut h.binding else {
        unreachable!()
    };
    v.operation_id = page.key.operation_id;
    assert_eq!(
        h.db.prepare_private_decision(&h.binding, &h.proof, h.boot, &projection::base(&h))
            .unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Conflict)
    );
    assert_eq!(count(&h, "bot_decisions"), 0);

    // A consumed private decision's id names its effect: an edit consume or a
    // granted read under it is refused, and the granted path cannot adopt it.
    let (mut h, record, fence, dispatcher, key) = consumed();
    let proposal = h.proposal();
    assert_eq!(
        h.db.prepare_private_effect(dispatcher, &proposal, &fence, &h.proof)
            .unwrap()
            .outcome
            .unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Conflict)
    );
    assert_eq!(
        h.db.private_effect(key.run, key.operation_id)
            .unwrap()
            .unwrap()
            .decision,
        Some(record.receipt.decision)
    );
}

#[test]
fn m5_private_occupancy_discounts_a_retired_private_wait_exactly_as_an_edit_wait() {
    let retire = |private: bool| {
        let mut h = Harness::new();
        if private {
            h.private();
        }
        let f = h.origin();
        h.db.set_bot_active(f.run, f.attempt, f.generation, false, None)
            .unwrap();
        let record = enrollment::retired(&mut h);
        let inventory = h.db.bot_execution_inventory(f.owner.device).unwrap();
        assert!(record.receipt.state.kind.resumes_by_successor());
        assert!(
            matches!(inventory.owned[0].capacity,crate::BotExecutionCapacity::QualifiedDecisionRelease{wait,..} if wait==record.wait.wait)
        );
        (
            inventory.occupied,
            inventory.owned.len(),
            h.db.active_bot_executions(f.owner.device).unwrap().len(),
            h.db.bot_has_unsettled(f.owner, f.bot).unwrap(),
        )
    };
    assert_eq!(retire(true), retire(false));
    assert_eq!(retire(true), (0, 1, 1, true));
    // A native wait is not discounted: nothing else changed.
    let mut h = Harness::new();
    h.native();
    let f = h.origin();
    let record = h.prepare();
    h.published(record.receipt.decision, true);
    assert_eq!(
        h.db.bot_execution_inventory(f.owner.device)
            .unwrap()
            .occupied,
        1
    );
}

#[test]
fn m5_decision_kind_sites_a_private_decision_takes_every_koru_path_and_no_native_one() {
    // Enrollment, the origin stop, the answer, the successor reservation, the
    // session binding and the input acknowledgment each accepted a private
    // decision on the way to `allowed()`; the wait and resume decoders read
    // it back at every step. Here: the sites that distinguish kinds.
    let (mut h, record, fence, _clock) = allowed();
    let f = h.origin();
    // The wait observation and the cleanup owner read a Koru wait.
    let execution =
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap();
    assert_eq!(
        execution.decision_state.as_ref().unwrap().kind,
        DecisionKind::PrivateOperation
    );
    assert!(execution.resume.is_some());
    assert!(execution.wait.enrollment_request.is_some());
    // No native companion was written for it.
    assert_eq!(count(&h, "bot_decision_native_answers"), 0);
    assert_eq!(count(&h, "native_decision_leases"), 0);
    assert!(record.native_response.is_none());
    // A native answer for it is refused.
    let native_choice = DecisionNativeChoice {
        id: ApprovalChoiceId::mint(),
        intent: serde_json::from_str("\"allow\"").unwrap(),
    };
    assert!(
        !DecisionAnswerChoice::Native {
            offered: native_choice
        }
        .matches_binding(&record.receipt.binding)
    );
    // The edit consume refuses a private decision, and the private consume
    // refuses an edit decision.
    let (mut edit, edit_record, request, edit_fence, payload) = consume::ready();
    let proposal = h.proposal();
    assert_eq!(
        edit.db
            .consume_private_decision(
                &expected(&edit_record),
                DecisionTransitionId::mint(),
                PrivateDispatcherId::mint(),
                &proposal,
                &edit_fence,
                &edit.proof,
                edit.boot,
            )
            .unwrap()
            .outcome
            .unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Invalid)
    );
    let mut private_request = request.clone();
    private_request.key.run = f.run;
    assert!(
        h.db.consume_decision_edit(
            &expected(&record),
            DecisionTransitionId::mint(),
            &private_request,
            &payload,
            &fence,
            &h.proof,
            h.boot,
        )
        .is_err()
    );
    assert_eq!(count(&h, "native_result_intents"), 0);
    // The edit path itself is unchanged: its consume still takes the one
    // native lease and writes no private effect.
    let lease = edit
        .db
        .consume_decision_edit(
            &expected(&edit_record),
            DecisionTransitionId::mint(),
            &request,
            &payload,
            &edit_fence,
            &edit.proof,
            edit.boot,
        )
        .unwrap();
    assert!(lease.execute.is_some());
    assert_eq!(count(&edit, "bot_private_effects"), 0);
    assert_eq!(count(&edit, "native_decision_leases"), 1);
    // A private outcome cannot settle an edit, nor a page outcome a read.
    let edit_consumed = edit.read(edit_record.receipt.decision).unwrap();
    assert!(
        edit.db
            .settle_decision_effect(
                &expected(&edit_consumed),
                DecisionTransitionId::mint(),
                &DecisionRetainedEffect::PrivateUnknown {
                    key: PrivateEffectKey {
                        run: request.key.run,
                        operation_id: request.key.operation_id,
                        attempt: request.key.attempt,
                        generation: request.key.generation,
                        turn: request.key.turn,
                    }
                }
            )
            .is_err()
    );
}

#[test]
fn m5_decision_kind_sites_both_sql_literals_read_the_private_kind() {
    // `rows.rs`: the lookup by operation has a private sibling.
    let (mut h, record, fence, dispatcher, key) = consumed();
    let f = h.origin();
    assert_eq!(
        h.db.bot_private_decision_for_operation(f.owner, f.run, key.operation_id)
            .unwrap()
            .unwrap()
            .receipt
            .decision,
        record.receipt.decision
    );
    // `rows/resume.rs`: a settled private decision's acknowledged consumer is
    // a settled continuation, so the successor's own turn may act again.
    advance(&mut h, &key, dispatcher, REFUSED);
    let outcome =
        h.db.decision_retained_effect(f.owner, f.run, record.receipt.decision)
            .unwrap()
            .unwrap();
    let transition = DecisionTransitionId::mint();
    h.db.settle_decision_effect(&expected(&record), transition, &outcome)
        .unwrap();
    let pending =
        h.db.decision_projection(f.owner, f.run, transition)
            .unwrap()
            .unwrap();
    let wait = completion::ticket(&h);
    let prepared = projection::prepared(&pending);
    let ack = DecisionProjectionAcknowledgment {
        identity: pending.identity.clone(),
        delta_digest: prepared.delta_digest,
        observed_version: DecisionVersionVector::new(vec![3]).unwrap(),
    };
    projection::apply(&mut h, &pending);
    h.db.settle_decision_wait(&wait, &ack).unwrap();
    // With the settled-continuation literal reading the private kind, a page
    // result by the successor's acknowledged turn is admitted again.
    let page = PrepareNativeResult {
        key: NativeResultKey {
            run: f.run,
            attempt: fence.attempt,
            generation: fence.generation,
            turn: fence.turn.unwrap(),
            operation_id: uuid::Uuid::new_v4(),
        },
        kind: NativeResultOperation::CreatePage,
        destination: h.proof.global.policy.workspace,
        target: None,
        request_fingerprint: [5; 32],
        expected_revision: None,
        config_revision: h.proof.global.config_revision,
        policy_revision: h.proof.global.policy_revision,
        title: "After the read".into(),
    };
    h.db.prepare_native_result(&page).unwrap();
}
