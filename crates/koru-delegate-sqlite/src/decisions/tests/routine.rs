// SPDX-License-Identifier: MPL-2.0

//! A Routine Run's page-edit decision (P4.3 section 4.10): bound to the
//! Routine's own grant, answered, consumed once under that grant, and closed
//! by a decline or by its deadline. Real in-memory ledger, real admission,
//! claim, link and policy owners; a fake clock.
use super::*;
use lifecycle::expected;
use std::sync::{Arc, atomic::AtomicI64, atomic::Ordering};

/// A Linked Routine Run whose Routine grant (revision 1) does not include the
/// binding's target page.
fn harness() -> (Harness, Arc<AtomicI64>) {
    let clock = Arc::new(AtomicI64::new(1000));
    let mut h = Harness::with_db(RunsDb::in_memory(Box::new(fixtures::FakeClock(
        clock.clone(),
    ))));
    h.routine(true);
    (h, clock)
}

/// The first projection base of a Routine Run (BotRun5).
fn base(h: &Harness) -> DecisionPublicationBase {
    let mut base = projection::base(h);
    base.source_schema = 5u8.try_into().unwrap();
    base
}

/// Prepared through the public owner, its initial projection applied and its
/// enrollment requested: Pending/Holding, wait HoldRequested.
fn enrollment_ready() -> (Harness, DecisionRecord, Arc<AtomicI64>) {
    let (mut h, clock) = harness();
    let prepared =
        h.db.prepare_bot_decision(&h.binding, &h.proof, h.boot, &base(&h))
            .unwrap();
    let f = h.origin();
    let initial =
        h.db.next_decision_projection(f.owner, f.run)
            .unwrap()
            .unwrap();
    projection::apply(&mut h, &initial);
    let record = crate::test_support::request_enrollment(&mut h.db, &prepared).unwrap();
    (h, record, clock)
}

/// Pending/Held with the origin stopped: answerable.
fn held() -> (Harness, DecisionRecord, Arc<AtomicI64>) {
    let (mut h, record, clock) = enrollment_ready();
    let record = crate::test_support::record_enrollment(&mut h.db, &record).unwrap();
    let record = crate::test_support::stop_origin(&mut h.db, &record, 1000).unwrap();
    (h, record, clock)
}

/// Granted, its successor bound and its continuation input acknowledged: the
/// one-use consume is the next step.
fn ready() -> (
    Harness,
    DecisionRecord,
    PrepareNativeResult,
    BotExecutionFence,
    Vec<u8>,
    Arc<AtomicI64>,
) {
    let (mut h, record, clock) = held();
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
    let request = crate::test_support::consume_request(&record, &fence).unwrap();
    let DecisionBindingV1::KoruEdit(edit) = &record.receipt.binding else {
        panic!("edit")
    };
    let payload = edit.payload.as_slice().to_vec();
    (h, record, request, fence, payload, clock)
}

fn durable(request: &PrepareNativeResult, record: &DecisionRecord) -> ConfirmNativeResult {
    let DecisionBindingV1::KoruEdit(edit) = &record.receipt.binding else {
        panic!("edit")
    };
    ConfirmNativeResult {
        key: request.key.clone(),
        request_fingerprint: request.request_fingerprint,
        observation: NativeResultObservation::Durable {
            evidence: NativeResultReference {
                version_id: None,
                operation_id: edit.operation_id,
                workspace: edit.workspace,
                document: edit.target,
                version: vec![7, 7, 7],
                title: request.title.clone(),
                confirmed_at_ms: 1000,
                kind: NativeResultKind::EditedPage,
            },
        },
    }
}

/// A newer grant of the same Routine, installed as its current policy. The
/// grant selects `page` when one is given.
fn regrant(h: &mut Harness, revision: u64, page: Option<DocumentId>) -> RoutinePolicyBarrier {
    let mut next = h.proof.routine.clone().unwrap();
    next.request = BotRequestId::mint().into();
    next.grant_revision = revision;
    if let Some(page) = page {
        next.policy.selected_pages.insert(page);
    }
    h.db.install_routine_policy(&next).unwrap();
    next
}

/// Rebind the harness's binding and proof to `grant`.
fn bind(h: &mut Harness, grant: &RoutinePolicyBarrier) {
    let policy = DecisionRoutinePolicy {
        owner: grant.owner,
        bot: grant.bot,
        schedule: grant.schedule,
        request: grant.request,
        grant_revision: rev(grant.grant_revision),
        digest: grant.digest().unwrap(),
    };
    h.edit().routine_policy = Some(policy);
    h.proof.routine = Some(grant.clone());
}

#[test]
fn routine_decision_denied_by_the_routines_own_grant_asks_bound_to_that_grant() {
    let (mut h, _clock) = harness();
    let grant = h.proof.routine.clone().unwrap();
    let saved = h.prepare();
    assert_eq!(saved.receipt.state.kind, DecisionKind::KoruEdit);
    assert_eq!(
        saved.receipt.state.authorization,
        DecisionAuthorization::Pending
    );
    let DecisionBindingV1::KoruEdit(bound) = &saved.receipt.binding else {
        panic!("edit")
    };
    // The Routine itself and the grant it runs under now, not the Bot's.
    let subject = bound.routine.as_ref().expect("the Routine subject");
    assert_eq!(subject.provenance.schedule, grant.schedule);
    let policy = bound.routine_policy.as_ref().expect("the Routine policy");
    assert_eq!(
        (
            policy.schedule,
            policy.request,
            policy.grant_revision.get(),
            policy.digest
        ),
        (
            grant.schedule,
            grant.request,
            grant.grant_revision,
            grant.digest().unwrap()
        )
    );
    assert_eq!(h.count("bot_decisions"), 1);
    assert_eq!(h.count("bot_wait_runtime"), 1);
    assert_eq!(h.count("native_result_intents"), 0);
    // The subject the runtime binds is exactly the retained one.
    let f = h.origin();
    assert_eq!(
        h.db.decision_routine_subject(f.owner, f.run).unwrap(),
        bound.routine.clone()
    );
}

#[test]
fn routine_decision_allowed_by_the_grant_does_not_ask() {
    // The Routine's current grant selects the page: nothing to ask, whatever
    // the Bot's ordinary-chat access says.
    let (mut h, _clock) = harness();
    let DecisionBindingV1::KoruEdit(edit) = &h.binding else {
        panic!("edit")
    };
    let target = edit.target;
    let grant = regrant(&mut h, 2, Some(target));
    bind(&mut h, &grant);
    assert!(matches!(h.prepare_at(1000), Err(BotLedgerError::Fenced)));
    assert_eq!(h.count("bot_decisions"), 0);
    assert_eq!(h.count("bot_wait_runtime"), 0);
}

#[test]
fn routine_decision_never_asks_through_the_bots_ordinary_access() {
    // Binding, Routine policy and proof name the Routine together or not at
    // all. A Routine Run offered with an interactive proof, a Routine proof
    // offered for a binding without its Routine policy, and an interactive
    // Run offered with a Routine proof are all refused.
    let (mut h, _clock) = harness();
    let routine = h.proof.routine.take();
    assert!(h.prepare_at(1000).is_err());
    h.proof.routine = routine.clone();
    let policy = h.edit().routine_policy.take();
    assert!(h.prepare_at(1000).is_err());
    h.edit().routine_policy = policy;
    assert_eq!(h.count("bot_decisions"), 0);

    let mut manual = Harness::new();
    manual.proof.routine = routine;
    assert!(manual.prepare_at(1000).is_err());
    assert_eq!(manual.count("bot_decisions"), 0);
    // And the interactive Run itself is unchanged.
    manual.proof.routine = None;
    let saved = manual.prepare();
    assert!(
        matches!(&saved.receipt.binding, DecisionBindingV1::KoruEdit(v) if v.routine.is_none() && v.routine_policy.is_none())
    );
}

#[test]
fn routine_decision_page_this_routine_created_does_not_ask() {
    let (mut h, _clock) = harness();
    let f = h.origin();
    let fence = BotExecutionFence {
        owner: f.owner,
        bot: f.bot,
        run: f.run,
        attempt: f.attempt,
        generation: f.generation,
        session: f.session,
        turn: Some(f.turn),
    };
    let workspace = h.edit().workspace;
    // This occurrence creates a page through the ordinary native path.
    let create = PrepareNativeResult {
        key: NativeResultKey {
            run: f.run,
            attempt: f.attempt,
            generation: f.generation,
            turn: f.turn,
            operation_id: uuid::Uuid::new_v4(),
        },
        kind: NativeResultOperation::CreatePage,
        destination: workspace,
        target: None,
        request_fingerprint: [8; 32],
        expected_revision: None,
        config_revision: 1,
        policy_revision: 1,
        title: "Created".into(),
    };
    let prepared = h.db.prepare_native_result(&create).unwrap();
    assert!(
        h.db.lease_native_result(&create.key, create.request_fingerprint, &fence, &h.proof)
            .unwrap()
            .execute
            .is_some()
    );
    h.db.confirm_native_result(&ConfirmNativeResult {
        key: create.key.clone(),
        request_fingerprint: create.request_fingerprint,
        observation: NativeResultObservation::Durable {
            evidence: NativeResultReference {
                version_id: None,
                operation_id: create.key.operation_id,
                workspace,
                document: prepared.target,
                version: vec![1],
                title: "Created".into(),
                confirmed_at_ms: 1000,
                kind: NativeResultKind::CreatedPage,
            },
        },
    })
    .unwrap();
    // The page is not in the grant, but this Routine created it (O83).
    h.edit().target = prepared.target;
    assert!(matches!(h.prepare_at(1000), Err(BotLedgerError::Fenced)));
    assert_eq!(h.count("bot_decisions"), 0);
}

/// The test that proves an approved Routine edit actually runs.
#[test]
fn routine_decision_granted_edit_is_consumed_and_executes_once() {
    let (mut h, record, request, fence, payload, _clock) = ready();
    assert_eq!(
        record.receipt.state.authorization,
        DecisionAuthorization::Granted
    );
    let grant = h.proof.routine.clone().unwrap();
    let first =
        h.db.consume_decision_edit(
            &expected(&record),
            DecisionTransitionId::mint(),
            &request,
            &payload,
            &fence,
            &h.proof,
            h.boot,
        )
        .unwrap();
    // The one license, under the Routine's own grant.
    let lease = first.execute.clone().expect("the one execution license");
    let leased = lease.routine.as_ref().expect("the Routine lease");
    assert_eq!(
        (leased.schedule, leased.request, leased.grant_revision),
        (grant.schedule, grant.request, grant.grant_revision)
    );
    let consumed = h.read(record.receipt.decision).unwrap();
    assert_eq!(
        (
            consumed.receipt.state.authorization,
            consumed.receipt.state.phase
        ),
        (DecisionAuthorization::Consumed, DecisionPhase::Effect)
    );
    assert_eq!(h.count("native_decision_leases"), 1);
    assert_eq!(h.count("native_routine_leases"), 1);
    assert_eq!(h.count("native_result_intents"), 1);

    // The edit's native result goes through the effect validator (every
    // retained read of this intent does) to a confirmed result, once.
    let confirm = durable(&request, &record);
    let confirmed = h.db.confirm_native_result(&confirm).unwrap();
    assert_eq!(confirmed.stage, NativeResultStage::Verified);
    let retained = h.db.native_result(&request.key).unwrap().unwrap();
    assert_eq!(retained.stage, NativeResultStage::Verified);
    assert!(
        retained
            .lease
            .as_ref()
            .is_some_and(|lease| lease.routine.is_some())
    );
    let outcome =
        h.db.decision_retained_effect(fence.owner, fence.run, record.receipt.decision)
            .unwrap();
    assert!(matches!(
        outcome,
        Some(DecisionRetainedEffect::Durable { .. })
    ));

    // A second consume and a second effect with the same key return the
    // retained outcome and write nothing.
    let projections = h.count("bot_decision_publications");
    let replay =
        h.db.consume_decision_edit(
            &expected(&record),
            DecisionTransitionId::mint(),
            &request,
            &payload,
            &fence,
            &h.proof,
            h.boot,
        )
        .unwrap();
    assert!(replay.execute.is_none(), "never a second license");
    assert_eq!(replay.intent.key, request.key);
    assert_eq!(replay.intent.stage, NativeResultStage::Verified);
    let again = h.db.confirm_native_result(&confirm).unwrap();
    assert_eq!(again.stage, NativeResultStage::Verified);
    assert_eq!(again.verified, confirmed.verified);
    assert_eq!(h.count("native_decision_leases"), 1);
    assert_eq!(h.count("native_routine_leases"), 1);
    assert_eq!(h.count("native_result_intents"), 1);
    assert_eq!(h.count("bot_decision_publications"), projections);
    assert_eq!(
        h.read(record.receipt.decision)
            .unwrap()
            .receipt
            .state
            .authorization,
        DecisionAuthorization::Consumed
    );

    // The positively recorded save settles the decision as Completed.
    let settled =
        h.db.settle_decision_effect(
            &expected(&h.read(record.receipt.decision).unwrap()),
            DecisionTransitionId::mint(),
            &outcome.unwrap(),
        )
        .unwrap();
    assert_eq!(settled.receipt.state.phase, DecisionPhase::Settled);
    assert_eq!(
        settled.receipt.closure.as_ref().map(|c| c.reason),
        Some(DecisionClosureReason::Completed)
    );
}

#[test]
fn routine_decision_consume_requires_the_routines_own_current_grant() {
    // Without the Routine in the proof the approved edit is not consumed.
    let (mut h, record, request, fence, payload, _clock) = ready();
    let interactive = FreshBotPolicyProof {
        global: h.proof.global.clone(),
        routine: None,
    };
    assert!(
        h.db.consume_decision_edit(
            &expected(&record),
            DecisionTransitionId::mint(),
            &request,
            &payload,
            &fence,
            &interactive,
            h.boot,
        )
        .is_err()
    );
    // 5b: a grant revision change makes the answer stale, offered with the
    // old proof or with the new one. Nothing is consumed or leased.
    let newer = regrant(&mut h, 2, None);
    let stale = h.proof.clone();
    let fresh = FreshBotPolicyProof {
        global: h.proof.global.clone(),
        routine: Some(newer),
    };
    for proof in [&stale, &fresh] {
        assert!(
            h.db.consume_decision_edit(
                &expected(&record),
                DecisionTransitionId::mint(),
                &request,
                &payload,
                &fence,
                proof,
                h.boot,
            )
            .is_err()
        );
    }
    assert_eq!(
        h.read(record.receipt.decision)
            .unwrap()
            .receipt
            .state
            .authorization,
        DecisionAuthorization::Granted
    );
    assert_eq!(h.count("native_decision_leases"), 0);
    assert_eq!(h.count("native_routine_leases"), 0);
    assert_eq!(h.count("native_result_intents"), 0);
}

#[test]
fn routine_decision_stale_grant_revision_refuses_the_answer() {
    let (mut h, record, _clock) = held();
    let input = h.answer_input(&record, true);
    let newer = regrant(&mut h, 2, None);
    let stale = h.proof.clone();
    let fresh = FreshBotPolicyProof {
        global: h.proof.global.clone(),
        routine: Some(newer),
    };
    for proof in [&stale, &fresh] {
        assert!(
            h.db.answer_bot_decision(&input, DecisionTransitionId::mint(), proof, h.boot)
                .is_err()
        );
    }
    let unchanged = h.read(record.receipt.decision).unwrap();
    assert_eq!(
        unchanged.receipt.state.authorization,
        DecisionAuthorization::Pending
    );
    assert!(unchanged.receipt.answer.is_none());
}

#[test]
fn routine_decision_declined_closes_and_is_never_consumed() {
    let (mut h, record, _clock) = held();
    let f = h.origin();
    let input = h.answer_input(&record, false);
    let denied =
        h.db.answer_bot_decision(&input, DecisionTransitionId::mint(), &h.proof, h.boot)
            .unwrap();
    assert_eq!(
        (
            denied.receipt.state.authorization,
            denied.receipt.state.phase,
            denied.wait.phase
        ),
        (
            DecisionAuthorization::Denied,
            DecisionPhase::StopPending,
            BotWaitPhase::CleanupPending
        )
    );
    // The decline owns the Run's cleanup; no successor can be reserved.
    assert!(
        h.db.decision_cleanup_ownership(f.owner, f.run)
            .unwrap()
            .is_some()
    );
    assert!(
        h.db.reserve_decision_successor(
            &expected(&denied),
            DecisionTransitionId::mint(),
            HostInstanceId::from_uuid(uuid::Uuid::new_v4()),
            [42; 32],
            &h.proof,
            h.boot,
        )
        .is_err()
    );
    assert_eq!(h.count("native_result_intents"), 0);
    assert_eq!(h.count("native_routine_leases"), 0);
}

#[test]
fn routine_decision_unanswered_expires_and_a_late_or_duplicate_answer_changes_nothing() {
    // A duplicate of an accepted answer replays it and grants nothing more.
    let (mut h, record, _clock) = held();
    let input = h.answer_input(&record, true);
    let transition = DecisionTransitionId::mint();
    let first =
        h.db.answer_bot_decision(&input, transition, &h.proof, h.boot)
            .unwrap();
    let projections = h.count("bot_decision_publications");
    let again =
        h.db.answer_bot_decision(&input, transition, &h.proof, h.boot)
            .unwrap();
    assert_eq!(again.receipt, first.receipt);
    assert_eq!(h.count("bot_decision_publications"), projections);
    // The same answer under another transition, or another answer, is refused.
    assert!(
        h.db.answer_bot_decision(&input, DecisionTransitionId::mint(), &h.proof, h.boot)
            .is_err()
    );
    let other = h.answer_input(&first, false);
    assert!(
        h.db.answer_bot_decision(&other, DecisionTransitionId::mint(), &h.proof, h.boot)
            .is_err()
    );
    assert_eq!(
        h.read(record.receipt.decision).unwrap().receipt,
        first.receipt
    );
    assert_eq!(
        first.receipt.state.authorization,
        DecisionAuthorization::Granted
    );

    // Nobody answers: before the deadline expiry is refused, at it accepted.
    let (mut h, record, clock) = held();
    let f = h.origin();
    // Every owed projection is applied, as the publication worker would.
    while let Some(owed) = h.db.next_decision_projection(f.owner, f.run).unwrap() {
        projection::apply(&mut h, &owed);
    }
    let record = h.read(record.receipt.decision).unwrap();
    let deadline = record.receipt.deadline.deadline_at_ms;
    let ticket = |h: &Harness| {
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap()
            .expected
    };
    clock.store(deadline - 1, Ordering::SeqCst);
    assert!(
        h.db.request_decision_cleanup(
            &ticket(&h),
            DecisionTransitionId::mint(),
            DecisionClosureReason::Expired
        )
        .is_err()
    );
    clock.store(deadline, Ordering::SeqCst);
    // A late answer is refused by the ledger's own clock.
    let late = h.answer_input(&record, true);
    assert!(
        h.db.answer_bot_decision(&late, DecisionTransitionId::mint(), &h.proof, h.boot)
            .is_err()
    );
    h.db.request_decision_cleanup(
        &ticket(&h),
        DecisionTransitionId::mint(),
        DecisionClosureReason::Expired,
    )
    .unwrap();
    let expired = h.read(record.receipt.decision).unwrap();
    assert_eq!(
        (
            expired.receipt.state.authorization,
            expired.receipt.state.phase,
            expired.wait.phase
        ),
        (
            DecisionAuthorization::Expired,
            DecisionPhase::StopPending,
            BotWaitPhase::CleanupPending
        )
    );
    // An answer after the expiry never reopens it.
    let after = h.answer_input(&expired, true);
    assert!(
        h.db.answer_bot_decision(&after, DecisionTransitionId::mint(), &h.proof, h.boot)
            .is_err()
    );
    assert_eq!(
        h.read(record.receipt.decision).unwrap().receipt,
        expired.receipt
    );
}
