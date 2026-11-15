// SPDX-License-Identifier: MPL-2.0

//! The decision resume chain through the runtime's per-turn driver, over the
//! real in-memory ledger and the scripted session host. Scripted runtime
//! composition, not provider qualification: nothing here proves a provider
//! stops, launches or accepts input as the scripted host does.

use super::*;
use crate::DecisionResumeStep as Step;

/// An applied Koru edit decision prepared through the runtime entry point.
struct Chain {
    h: Harness,
    s: BotSubmission,
    fence: BotExecutionFence,
    proof: FreshBotPolicyProof,
    boot: DelegateBootId,
    decision: DecisionId,
}

fn attach_projection(h: &mut Harness, run: RunId, exclusive: bool) {
    use crate::orchestrator::publication::RunDocumentOwner;
    let state = h.orchestrator.runs.get_mut(&run).unwrap();
    let RunDocumentOwner::Attached { handle, .. } =
        std::mem::replace(&mut state.doc, RunDocumentOwner::Unopened)
    else {
        panic!("attached")
    };
    state.doc = RunDocumentOwner::Attached {
        exclusive,
        handle: Box::new(ProjectionHandle {
            inner: handle,
            staging: koru_delegate_document::RunProjectionStaging::default(),
        }),
    };
}

/// The dev-only builder's edit, so the fixture edit has one copy.
fn edit_request() -> crate::DecisionEditRequest {
    crate::test_support::edit_request()
}

fn edit_decision() -> Chain {
    let (mut h, s, fence, policy, _) = native_results::active();
    h.orchestrator
        .synchronize_native_policy(policy.clone(), fence.owner.device)
        .unwrap();
    let proof = FreshBotPolicyProof {
        global: policy,
        routine: None,
    };
    let boot = h.orchestrator.delegate_boot();
    let prepared = h
        .orchestrator
        .bot_prepare_edit_decision(&fence, fence.owner.device, &edit_request(), &proof, boot)
        .unwrap();
    attach_projection(&mut h, fence.run, false);
    publish_owed(&mut h, fence.run);
    Chain {
        h,
        s,
        fence,
        proof,
        boot,
        decision: prepared.receipt.decision,
    }
}

fn advance(c: &mut Chain) -> Step {
    let ai = c.s.provenance.ai_attempt_id.unwrap();
    c.h.orchestrator
        .bot_advance_decision_resume(c.fence.owner, c.fence.run, &mut Enroller(ai))
        .unwrap()
}

fn execution(c: &Chain) -> koru_delegate_sqlite::DecisionWaitExecution {
    c.h.schedule_store
        .inner
        .lock()
        .unwrap()
        .decision_wait_execution(c.fence.owner, c.fence.run)
        .unwrap()
        .unwrap()
}

fn profile(c: &Chain) -> ProfileId {
    match &c.s.request.requested_by {
        ActorRef::Person { profile } => *profile,
        _ => panic!("person"),
    }
}

/// AllowOnce at the decision's current revision, as the human route would.
fn allow(c: &mut Chain) {
    let record =
        c.h.schedule_store
            .inner
            .lock()
            .unwrap()
            .bot_decision(c.fence.owner, c.fence.run, c.decision)
            .unwrap()
            .unwrap();
    let human = profile(c);
    c.h.orchestrator
        .bot_answer_decision(
            &AnswerDecision {
                owner: c.fence.owner,
                run: c.fence.run,
                decision: c.decision,
                expected_revision: record.receipt.state.revision,
                binding_digest: record.receipt.binding_digest,
                request: DecisionAnswerId::mint(),
                human,
                choice: DecisionAnswerChoice::AllowOnce {},
                authority_digest: DecisionDigest::from_bytes([9; 32]),
            },
            c.fence.owner.device,
            &c.proof,
            c.boot,
        )
        .unwrap();
}

/// Enrollment, origin stop and AllowOnce; the next turn reserves.
fn held_and_allowed(c: &mut Chain) {
    assert_eq!(advance(c), Step::EnrollmentRequested);
    assert_eq!(advance(c), Step::Enrolled);
    assert_eq!(advance(c), Step::OriginStopped);
    assert!(
        c.h.attempt(c.fence.run).retired,
        "the origin row is retired"
    );
    assert_eq!(advance(c), Step::Waiting("bot_answer_pending"));
    allow(c);
}

fn script_creation(c: &mut Chain, session: SessionId) {
    c.h.sessions.lock().unwrap().create = Some(CreatedSession {
        session,
        provider: "codex".parse().unwrap(),
        requested_model: None,
    });
}

fn creates(c: &Chain, creation: IdempotencyKey) -> usize {
    c.h.calls()
        .iter()
        .filter(|call| matches!(call, Call::Create { key, .. } if *key == creation))
        .count()
}

fn submits_to(c: &Chain, session: SessionId) -> usize {
    c.h.calls()
        .iter()
        .filter(|call| matches!(call, Call::Submit { session: s, .. } if *s == session))
        .count()
}

fn creation_key(c: &Chain) -> IdempotencyKey {
    execution(c).resume.unwrap().consumer_identity().3
}

#[test]
fn decision_resume_driver_takes_one_step_per_turn_to_the_acknowledged_successor() {
    let mut c = edit_decision();
    let (run, owner) = (c.fence.run, c.fence.owner);
    held_and_allowed(&mut c);
    let successor = session(2);
    script_creation(&mut c, successor);
    assert_eq!(advance(&mut c), Step::SuccessorReserved);
    let resume = execution(&c).resume.unwrap();
    let (attempt, generation, _, creation) = resume.consumer_identity();
    assert_eq!(generation.get(), c.fence.generation.get() + 1);
    assert_eq!(advance(&mut c), Step::SuccessorBound);
    assert_eq!(advance(&mut c), Step::SuccessorInstalled);
    assert_eq!(advance(&mut c), Step::InputAcknowledged);
    assert_eq!(advance(&mut c), Step::Waiting("bot_consume_owned"));
    // Exactly one creation under the reserved key, the controller taken from
    // the host, and one continuation input on the successor's session.
    assert_eq!(creates(&c, creation), 1);
    assert!(c.h.calls().contains(&Call::Attach(successor)));
    assert_eq!(submits_to(&c, successor), 1);
    let text =
        c.h.calls()
            .into_iter()
            .find_map(|call| match call {
                Call::Submit { session, text } if session == successor => Some(text),
                _ => None,
            })
            .unwrap();
    assert!(text.contains(&c.decision.to_string()));
    assert!(
        text.contains(&c.s.input.text),
        "original captured task input"
    );
    // The acknowledgment is recorded and mirrored for this exact consumer.
    let DecisionResumeState::InputAcknowledged { acknowledgment } =
        execution(&c).resume.unwrap().state
    else {
        panic!("acknowledged")
    };
    let memory = c.h.orchestrator.runs[&run].attempt.as_ref().unwrap();
    assert_eq!(
        (
            memory.id,
            memory.generation,
            memory.session,
            memory.resume_turn,
            memory.orchestrator_turn,
            memory.holds_lease
        ),
        (
            attempt,
            generation,
            Some(successor),
            Some(acknowledgment.turn),
            Some(acknowledgment.turn),
            true
        )
    );
    assert_eq!(c.h.attempt(run).phase, AttemptPhase::Running);
    // Original Fired after successor: the InitialSend still names the
    // original attempt and turn, and BeginTurn is not a second initial send.
    let original =
        c.h.schedule_store
            .inner
            .lock()
            .unwrap()
            .bot_admission(run)
            .unwrap()
            .unwrap()
            .initial_send
            .unwrap();
    assert_eq!(
        (original.attempt, original.generation, original.turn),
        (c.fence.attempt, c.fence.generation, c.fence.turn)
    );
    let consumer = BotExecutionFence {
        attempt,
        generation,
        session: successor,
        turn: None,
        ..c.fence
    };
    assert!(
        c.h.orchestrator
            .bot_begin_turn(&consumer, owner.device, c.s.input.clone())
            .is_err()
    );
    assert!(
        c.h.orchestrator
            .bot_begin_turn(&c.fence, owner.device, c.s.input.clone())
            .is_err()
    );
    let unchanged =
        c.h.schedule_store
            .inner
            .lock()
            .unwrap()
            .bot_admission(run)
            .unwrap()
            .unwrap()
            .initial_send
            .unwrap();
    assert_eq!(unchanged, original);
    assert_eq!(submits_to(&c, successor), 1, "no second input was sent");
}

#[test]
fn decision_successor_lost_creation_is_recovered_by_its_key_never_created_twice() {
    let mut c = edit_decision();
    held_and_allowed(&mut c);
    c.h.sessions.lock().unwrap().create_error = Some("unavailable");
    assert_eq!(advance(&mut c), Step::SuccessorReserved);
    let creation = creation_key(&c);
    assert_eq!(
        advance(&mut c),
        Step::Waiting("bot_successor_creation_unknown")
    );
    // The host recovers; the lost answer is never retried as a creation.
    c.h.sessions.lock().unwrap().create_error = None;
    assert_eq!(
        advance(&mut c),
        Step::Waiting("bot_successor_creation_unknown")
    );
    assert_eq!(creates(&c, creation), 1);
    assert!(c.h.calls().contains(&Call::Lookup(creation)));
    assert_eq!(execution(&c).resume.unwrap().consumer_identity().2, None);
    // A positive answer for the exact key binds without creating again.
    {
        let mut script = c.h.sessions.lock().unwrap();
        let mut known = summary(ManagedState::Running, None);
        known.id = session(3);
        let host = script.host;
        script
            .lookups
            .insert(creation, (host, CreationLookup::Known { session: known }));
    }
    assert_eq!(advance(&mut c), Step::SuccessorBound);
    assert_eq!(creates(&c, creation), 1);
    assert_eq!(
        execution(&c).resume.unwrap().consumer_identity().2,
        Some(session(3))
    );
}

#[test]
fn decision_successor_reserved_by_an_earlier_process_is_only_looked_up() {
    let mut c = edit_decision();
    held_and_allowed(&mut c);
    script_creation(&mut c, session(2));
    assert_eq!(advance(&mut c), Step::SuccessorReserved);
    let creation = creation_key(&c);
    // Working memory lost the reservation (a restart): the creation may have
    // been asked for, so unknown creation is looked up and never repeated.
    c.h.orchestrator
        .runs
        .get_mut(&c.fence.run)
        .unwrap()
        .decision_launch = None;
    assert_eq!(
        advance(&mut c),
        Step::Waiting("bot_successor_creation_unknown")
    );
    assert_eq!(creates(&c, creation), 0);
    assert!(c.h.calls().contains(&Call::Lookup(creation)));
}

#[test]
fn decision_continuation_lost_reply_stays_unconfirmed_and_resends_the_same_keyed_input() {
    let mut c = edit_decision();
    let run = c.fence.run;
    held_and_allowed(&mut c);
    let successor = session(2);
    script_creation(&mut c, successor);
    assert_eq!(advance(&mut c), Step::SuccessorReserved);
    assert_eq!(advance(&mut c), Step::SuccessorBound);
    assert_eq!(advance(&mut c), Step::SuccessorInstalled);
    c.h.sessions.lock().unwrap().bot_reply_lost = true;
    assert_eq!(advance(&mut c), Step::InputUnconfirmed);
    let resume = execution(&c).resume.unwrap();
    assert!(matches!(
        resume.state,
        DecisionResumeState::InputUnconfirmed {}
    ));
    let memory = c.h.orchestrator.runs[&run].attempt.as_ref().unwrap();
    assert_eq!(memory.resume_turn, None, "no turn before acknowledgment");
    // The resend carries the same key and bytes: the host answers with the
    // turn pinned to that key (a changed input would be refused).
    assert_eq!(advance(&mut c), Step::InputAcknowledged);
    assert_eq!(submits_to(&c, successor), 1);
    let DecisionResumeState::InputAcknowledged { acknowledgment } =
        execution(&c).resume.unwrap().state
    else {
        panic!("acknowledged")
    };
    let pinned = c.h.sessions.lock().unwrap().bot_inputs[&resume.input_key].1;
    assert_eq!(acknowledgment.turn, pinned);
}

#[test]
fn ordinary_stop_supersedes_the_resume_and_no_continuation_is_sent() {
    let mut c = edit_decision();
    let (run, owner) = (c.fence.run, c.fence.owner);
    held_and_allowed(&mut c);
    let successor = session(2);
    script_creation(&mut c, successor);
    assert_eq!(advance(&mut c), Step::SuccessorReserved);
    assert_eq!(advance(&mut c), Step::SuccessorBound);
    assert_eq!(advance(&mut c), Step::SuccessorInstalled);
    c.h.orchestrator
        .request_publication_cleanup(run, DecisionClosureReason::Canceled)
        .unwrap();
    assert_eq!(advance(&mut c), Step::Waiting("bot_cleanup_owned"));
    assert!(
        c.h.orchestrator
            .bot_submit_decision_continuation(owner, run)
            .unwrap()
            .is_none()
    );
    assert_eq!(submits_to(&c, successor), 0, "Stop sends no input");
}

/// The host bridge answers as scripted; a transport failure is unknown.
struct Bridge(Option<BotWaitEnrollmentReply>);
impl crate::DecisionEnrollmentBridge for Bridge {
    fn enroll(
        &mut self,
        _request: &EnrollBotWaitCleanup,
    ) -> crate::PortResult<BotWaitEnrollmentReply> {
        self.0
            .clone()
            .ok_or_else(|| PortError::new(Port::Sessions, "unavailable", "reply lost"))
    }
}

#[test]
fn enrollment_missing_versus_unavailable_never_stops_the_origin() {
    let mut c = edit_decision();
    let (run, owner) = (c.fence.run, c.fence.owner);
    assert_eq!(advance(&mut c), Step::EnrollmentRequested);
    // Unavailable: unknown, nothing recorded, the origin keeps running.
    assert!(
        c.h.orchestrator
            .bot_advance_decision_resume(owner, run, &mut Bridge(None))
            .is_err()
    );
    assert!(execution(&c).wait.enrollment.is_none());
    // Missing: an authenticated "no such wait" is never a stop.
    let key = execution(&c).wait.enrollment_request.unwrap().key;
    let _ = c.h.orchestrator.bot_advance_decision_resume(
        owner,
        run,
        &mut Bridge(Some(BotWaitEnrollmentReply::Missing { key })),
    );
    let step =
        c.h.orchestrator
            .bot_advance_decision_resume(owner, run, &mut Bridge(None));
    assert!(!matches!(step, Ok(Step::OriginStopped)));
    assert!(!c.h.attempt(run).retired, "the origin was not stopped");
    assert!(
        !c.h.calls()
            .iter()
            .any(|call| matches!(call, Call::Retire(_) | Call::Terminate(_)))
    );
}

#[test]
fn a_successor_reaches_the_low_cursor_of_its_own_stream() {
    let mut c = edit_decision();
    let run = c.fence.run;
    held_and_allowed(&mut c);
    let successor = session(2);
    script_creation(&mut c, successor);
    for _ in 0..4 {
        advance(&mut c);
    }
    assert_eq!(advance(&mut c), Step::Waiting("bot_consume_owned"));
    // The origin's accepted frontier was high; the successor's stream starts
    // with no cursor, never the old session's sequence.
    {
        let state = c.h.orchestrator.runs.get_mut(&run).unwrap();
        state.source_identity = Some((c.fence.attempt, c.fence.generation, c.fence.session));
        state.source_frontiers.agent_after = Some(StreamSequence::new(500).unwrap());
        state.source_frontiers.session_after = Some(StreamSequence::new(40).unwrap());
    }
    c.h.orchestrator
        .register_source_producer(run, uuid::Uuid::new_v4());
    let state = &c.h.orchestrator.runs[&run];
    assert_eq!(state.source_frontiers.agent_after, None);
    assert_eq!(state.source_frontiers.session_after, None);
    assert_eq!(
        state.source_identity.map(|identity| identity.2),
        Some(successor)
    );
}

// ---------------------------------------------------- boot and keyed answers

fn stale_boot_fixture() -> (
    Harness,
    BotSubmission,
    BotExecutionFence,
    FreshBotPolicyProof,
    PrepareNativeResult,
) {
    let (mut h, s, fence, policy, request) = native_results::active();
    h.orchestrator
        .synchronize_native_policy(policy.clone(), fence.owner.device)
        .unwrap();
    (
        h,
        s,
        fence,
        FreshBotPolicyProof {
            global: policy,
            routine: None,
        },
        request,
    )
}

fn assert_stale_boot<T>(h: &Harness, fence: &BotExecutionFence, result: Result<T, RunError>) {
    let error = result.err();
    assert!(
        matches!(&error, Some(RunError::Port(error)) if error.code == "bot_boot_stale"),
        "{error:?}"
    );
    assert!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .decision_wait_execution(fence.owner, fence.run)
            .unwrap()
            .is_none(),
        "nothing was written"
    );
}

#[test]
fn native_decision_preparation_refuses_a_stale_boot() {
    let (mut h, _, fence, proof, _) = stale_boot_fixture();
    let approval = crate::tests::bots::decisions::prompt(
        &mut h,
        &fence,
        vec![
            crate::tests::bots::decisions::choice("allow"),
            crate::tests::bots::decisions::choice("reject"),
        ],
    );
    let result = h.orchestrator.bot_prepare_native_decision(
        &fence,
        fence.owner.device,
        approval,
        &proof,
        DelegateBootId::mint(),
    );
    assert_stale_boot(&h, &fence, result);
}

#[test]
fn edit_decision_preparation_refuses_a_stale_boot() {
    let (mut h, _, fence, proof, _) = stale_boot_fixture();
    let result = h.orchestrator.bot_prepare_edit_decision(
        &fence,
        fence.owner.device,
        &edit_request(),
        &proof,
        DelegateBootId::mint(),
    );
    assert_stale_boot(&h, &fence, result);
}

#[test]
fn decision_answer_refuses_a_stale_boot() {
    let (mut h, s, fence, proof, _) = stale_boot_fixture();
    let ActorRef::Person { profile } = s.request.requested_by else {
        panic!("person")
    };
    let result = h.orchestrator.bot_answer_decision(
        &AnswerDecision {
            owner: fence.owner,
            run: fence.run,
            decision: DecisionId::mint(),
            expected_revision: DecisionRevision::new(1).unwrap(),
            binding_digest: DecisionDigest::from_bytes([1; 32]),
            request: DecisionAnswerId::mint(),
            human: profile,
            choice: DecisionAnswerChoice::AllowOnce {},
            authority_digest: DecisionDigest::from_bytes([9; 32]),
        },
        fence.owner.device,
        &proof,
        DelegateBootId::mint(),
    );
    assert_stale_boot(&h, &fence, result);
}

#[test]
fn decision_edit_consume_refuses_a_stale_boot() {
    let (mut h, _, fence, proof, request) = stale_boot_fixture();
    let result = h.orchestrator.bot_consume_decision_edit(
        &fence,
        fence.owner.device,
        &request,
        b"payload",
        &proof,
        DelegateBootId::mint(),
    );
    assert_stale_boot(&h, &fence, result);
}

#[test]
fn successor_reservation_refuses_a_stale_boot() {
    let (mut h, _, fence, proof, _) = stale_boot_fixture();
    let host = h.sessions.lock().unwrap().host;
    let result = h.orchestrator.bot_reserve_decision_successor(
        fence.owner,
        fence.run,
        host,
        [42; 32],
        &proof,
        DelegateBootId::mint(),
    );
    assert_stale_boot(&h, &fence, result);
}

#[test]
fn keyed_native_answer_forwards_the_callers_key_and_refuses_while_a_decision_owns_the_run() {
    let (mut h, _, fence, proof, _) = stale_boot_fixture();
    let device = fence.owner.device;
    let offered = vec![
        crate::tests::bots::decisions::choice("allow"),
        crate::tests::bots::decisions::choice("reject"),
    ];
    let approval = crate::tests::bots::decisions::prompt(&mut h, &fence, offered.clone());
    let key = IdempotencyKey::mint();
    for _ in 0..2 {
        h.orchestrator
            .bot_answer_approval_keyed(
                &fence,
                device,
                key,
                approval,
                ApprovalDecision::Approve,
                None,
            )
            .unwrap();
    }
    let forwarded: Vec<_> = h
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            Call::AnswerApproval {
                key, approval: a, ..
            } if a == approval => Some(key),
            _ => None,
        })
        .collect();
    assert_eq!(forwarded, vec![key, key], "the caller's key, unchanged");
    // Once a decision owns the Run's publication the ordinary route refuses.
    let owned = crate::tests::bots::decisions::prompt(&mut h, &fence, offered);
    let boot = h.orchestrator.delegate_boot();
    h.orchestrator
        .bot_prepare_native_decision(&fence, device, owned, &proof, boot)
        .unwrap();
    attach_projection(&mut h, fence.run, true);
    let refused = h.orchestrator.bot_answer_approval_keyed(
        &fence,
        device,
        IdempotencyKey::mint(),
        owned,
        ApprovalDecision::Approve,
        None,
    );
    assert!(
        matches!(&refused, Err(RunError::Port(error)) if error.code == "bot_decision_owned"),
        "{refused:?}"
    );
}

/// Drive the chain to an acknowledged successor; returns its consumer fence.
fn acknowledged(c: &mut Chain) -> BotExecutionFence {
    held_and_allowed(c);
    script_creation(c, session(2));
    for step in [
        Step::SuccessorReserved,
        Step::SuccessorBound,
        Step::SuccessorInstalled,
        Step::InputAcknowledged,
    ] {
        assert_eq!(advance(c), step);
    }
    let resume = execution(c).resume.unwrap();
    let (attempt, generation, session, _) = resume.consumer_identity();
    let DecisionResumeState::InputAcknowledged { acknowledgment } = resume.state else {
        panic!("acknowledged")
    };
    BotExecutionFence {
        attempt,
        generation,
        session: session.unwrap(),
        turn: Some(acknowledgment.turn),
        ..c.fence
    }
}

#[test]
fn the_successor_repeats_the_exact_approved_edit_and_consume_accepts_it() {
    let mut c = edit_decision();
    let consumer = acknowledged(&mut c);
    let record =
        c.h.schedule_store
            .inner
            .lock()
            .unwrap()
            .bot_decision(c.fence.owner, c.fence.run, c.decision)
            .unwrap()
            .unwrap();
    let DecisionBindingV1::KoruEdit(edit) = &record.receipt.binding else {
        panic!("edit")
    };
    // The continuation names the retained target, operation and arguments.
    let text =
        c.h.calls()
            .into_iter()
            .find_map(|call| match call {
                Call::Submit { session, text } if session == consumer.session => Some(text),
                _ => None,
            })
            .unwrap();
    assert!(text.contains(&edit.target.to_string()));
    assert!(text.contains(&edit.operation_id.to_string()));
    assert!(text.contains("PRIVATE-EDIT"));
    // The successor repeats exactly that edit: the one-use consume accepts it
    // under the acknowledged consumer fence and licenses it once.
    let request = koru_delegate_sqlite::test_support::consume_request(&record, &consumer).unwrap();
    let outcome =
        c.h.orchestrator
            .bot_consume_decision_edit(
                &consumer,
                c.fence.owner.device,
                &request,
                edit.payload.as_slice(),
                &c.proof,
                c.boot,
            )
            .unwrap();
    assert!(outcome.execute.is_some(), "licensed exactly once");
}

#[test]
fn after_memory_loss_the_successor_is_recognized_only_from_its_durable_acknowledgment() {
    let mut c = edit_decision();
    let consumer = acknowledged(&mut c);
    let (run, device) = (c.fence.run, c.fence.owner.device);
    // After the wait settled the handle is no longer exclusive (modelled
    // directly: settlement itself is covered by the D2 tests).
    {
        use crate::orchestrator::publication::RunDocumentOwner;
        let state = c.h.orchestrator.runs.get_mut(&run).unwrap();
        if let RunDocumentOwner::Attached { exclusive, .. } = &mut state.doc {
            *exclusive = false;
        }
    }
    assert!(!c.h.orchestrator.run_publication_pending(run));
    // A restart loses working memory of the successor's turn.
    {
        let memory =
            c.h.orchestrator
                .runs
                .get_mut(&run)
                .unwrap()
                .attempt
                .as_mut()
                .unwrap();
        memory.resume_turn = None;
        memory.orchestrator_turn = None;
    }
    // Native positive absence: the successor's acknowledged turn qualifies,
    // the original's turn under the successor attempt never does.
    let key = |turn| NativeResultKey {
        run,
        attempt: consumer.attempt,
        generation: consumer.generation,
        turn,
        operation_id: Uuid::new_v4(),
    };
    assert!(
        c.h.orchestrator
            .reconcile_native_result_if_present(&key(consumer.turn.unwrap()), device)
            .unwrap()
            .is_none()
    );
    let foreign =
        c.h.orchestrator
            .reconcile_native_result_if_present(&key(c.fence.turn.unwrap()), device);
    assert!(matches!(&foreign, Err(RunError::Port(error)) if error.code == "bot_fenced"));
    // Delivery: the original attempt's receipt is refused; the successor's is
    // accepted from the durable acknowledgment and recorded.
    let original = successor_receipt(&c.s, run, c.fence.attempt, c.fence.generation);
    let refused =
        c.h.orchestrator
            .bot_acknowledge_delivery(&key_of(&c.s), device, &original);
    assert!(matches!(&refused, Err(RunError::Port(error)) if error.code == "bot_fenced"));
    let delivered = successor_receipt(&c.s, run, consumer.attempt, consumer.generation);
    let accepted =
        c.h.orchestrator
            .bot_acknowledge_delivery(&key_of(&c.s), device, &delivered);
    assert!(
        !matches!(&accepted, Err(RunError::Port(error)) if error.code == "bot_fenced"),
        "{accepted:?}"
    );
    assert_eq!(
        c.h.schedule_store
            .inner
            .lock()
            .unwrap()
            .bot_admission(run)
            .unwrap()
            .unwrap()
            .delivery,
        Some(delivered)
    );
}

fn key_of(s: &BotSubmission) -> BotRunKey {
    BotRunKey {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        run: s.request.run,
    }
}

#[test]
fn decision_resume_runs_offer_the_open_decision_and_wrap_after_the_cursor() {
    let mut c = edit_decision();
    let (run, owner) = (c.fence.run, c.fence.owner);
    assert_eq!(
        c.h.orchestrator.bot_decision_resume_runs(None).unwrap(),
        vec![(owner, run)]
    );
    // Rotating past the only Run wraps to it once, never twice.
    assert_eq!(
        c.h.orchestrator
            .bot_decision_resume_runs(Some(run))
            .unwrap(),
        vec![(owner, run)]
    );
}
