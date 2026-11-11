// SPDX-License-Identifier: MPL-2.0

mod native_results;

use super::*;
use koru_delegate_core::*;
use koru_delegate_sqlite::*;
impl BotLedger for RecordingStore {
    fn active_bot_executions(
        &self,
        device: koru_delegate_core::DeviceId,
    ) -> Result<Vec<BotExecutionRecord>, BotLedgerError> {
        self.inner.lock().unwrap().active_bot_executions(device)
    }
    fn admit_bot(
        &mut self,
        submission: &BotSubmission,
    ) -> Result<BotAdmissionRecord, BotLedgerError> {
        self.inner.lock().unwrap().admit_bot(submission)
    }
    fn bot_admission(&self, run: RunId) -> Result<Option<BotAdmissionRecord>, BotLedgerError> {
        self.inner.lock().unwrap().bot_admission(run)
    }
    fn bot_execution(&self, run: RunId) -> Result<Option<BotExecutionRecord>, BotLedgerError> {
        self.inner.lock().unwrap().bot_execution(run)
    }
    fn reconcile_bot_execution(
        &self,
        key: &BotSubmissionKey,
    ) -> Result<Option<BotExecutionRecord>, BotLedgerError> {
        self.inner.lock().unwrap().reconcile_bot_execution(key)
    }
    fn reconcile_bot(
        &self,
        key: &BotSubmissionKey,
    ) -> Result<Option<BotAdmissionRecord>, BotLedgerError> {
        self.inner.lock().unwrap().reconcile_bot(key)
    }
    fn queued_bots(
        &self,
        after: u64,
        limit: u16,
    ) -> Result<Vec<QueuedBotAdmission>, BotLedgerError> {
        self.inner.lock().unwrap().queued_bots(after, limit)
    }
    fn bot_barrier(
        &self,
        owner: BotOwner,
        bot: BotId,
    ) -> Result<Option<BotAdmissionBarrier>, BotLedgerError> {
        self.inner.lock().unwrap().bot_barrier(owner, bot)
    }
    fn apply_bot_barrier(
        &mut self,
        barrier: &BotAdmissionBarrier,
    ) -> Result<BotAdmissionBarrier, BotLedgerError> {
        self.inner.lock().unwrap().apply_bot_barrier(barrier)
    }
    fn reserve_bot(&mut self, run: RunId) -> Result<BotReservation, BotLedgerError> {
        self.inner.lock().unwrap().reserve_bot(run)
    }
    fn activate_bot(
        &mut self,
        run: RunId,
        attempt: RunAttemptId,
        generation: AttemptGeneration,
    ) -> Result<(), BotLedgerError> {
        self.inner
            .lock()
            .unwrap()
            .activate_bot(run, attempt, generation)
    }
    fn set_bot_active(
        &mut self,
        run: RunId,
        attempt: RunAttemptId,
        generation: AttemptGeneration,
        active: bool,
    ) -> Result<(), BotLedgerError> {
        self.inner
            .lock()
            .unwrap()
            .set_bot_active(run, attempt, generation, active)
    }
    fn settle_bot(&mut self, run: RunId, outcome: BotSettlement) -> Result<(), BotLedgerError> {
        self.check("bot_settle")?;
        self.inner.lock().unwrap().settle_bot(run, outcome)
    }
    fn prepare_bot_initial_send(
        &mut self,
        run: RunId,
        send: &BotInitialSend,
    ) -> Result<BotInitialSend, BotLedgerError> {
        self.inner
            .lock()
            .unwrap()
            .prepare_bot_initial_send(run, send)
    }
    fn acknowledge_bot_initial_send(
        &mut self,
        run: RunId,
        send: &BotInitialSend,
    ) -> Result<BotInitialSend, BotLedgerError> {
        self.inner
            .lock()
            .unwrap()
            .acknowledge_bot_initial_send(run, send)
    }
    fn record_bot_delivery(
        &mut self,
        receipt: &BotDeliveryReceipt,
    ) -> Result<EventId, BotLedgerError> {
        self.inner.lock().unwrap().record_bot_delivery(receipt)
    }
}
fn submission(n: u8) -> BotSubmission {
    let mut request = request(
        run_id(n),
        WorkspaceScope::BotFolder {
            folder: BotWorkingFolder::new(format!("C:/fixture/bot-{n}")).unwrap(),
        },
    );
    request.context.clear();
    BotSubmission {
        provenance: BotProvenance {
            owner: BotOwner {
                user_scope: serde_json::from_value(serde_json::json!(v4(0xf0))).unwrap(),
                device: request.executing_device,
            },
            bot: BotId::from_uuid(v4(0xe0)),
            submission: BotRequestId::from_uuid(v4(n)),
            chat_id: v4(0xe1),
            ai_attempt_id: Some(v4(0xe2)),
            assistant_message_id: Some(v4(0xe3)),
            config_revision: 1,
            policy_revision: 1,
            continued_from: None,
        },
        policy: CapturedBotPolicy {
            workspace: workspace(),
            destination: workspace(),
            access: BotAccess::Default,
            selected_pages: Default::default(),
        },
        workspace: workspace(),
        request,
        input: PendingInput {
            idempotency_key: v4(n).to_string().parse().unwrap(),
            text: "task".into(),
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
fn key(s: &BotSubmission) -> BotRunKey {
    BotRunKey {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        run: s.request.run,
    }
}
fn submission_key(s: &BotSubmission) -> BotSubmissionKey {
    BotSubmissionKey {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        request: s.provenance.submission,
    }
}
fn prepared(h: &mut Harness, s: &BotSubmission) -> BotExecutionFence {
    assert!(s.valid());
    h.orchestrator
        .bot_submit(s.clone(), s.provenance.owner.device)
        .unwrap();
    let started = h
        .orchestrator
        .bot_dispatch(&key(s), s.provenance.owner.device)
        .unwrap()
        .unwrap();
    assert!(matches!(started.status, RunStatus::Starting));
    assert!(
        !h.sessions
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|call| matches!(call, Call::Submit { .. }))
    );
    h.orchestrator
        .bot_reconcile(&submission_key(s), s.provenance.owner.device)
        .unwrap()
        .unwrap()
        .fence
        .unwrap()
}
fn observe(h: &mut Harness, fence: &BotExecutionFence) {
    h.orchestrator
        .bot_observe(fence, fence.owner.device, None)
        .unwrap();
}
fn count_submits(h: &Harness) -> usize {
    h.sessions
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|call| matches!(call, Call::Submit { .. }))
        .count()
}

#[test]
fn native_question_skip_requires_current_owner_turn_prompt_and_controller() {
    use koru_session_protocol::{HandoverState, QuestionsDelivery, QuestionsOutcome};
    let mut h = bot_harness();
    let s = submission(41);
    let mut fence = prepared(&mut h, &s);
    observe(&mut h, &fence);
    let turn = h
        .orchestrator
        .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
        .unwrap();
    fence.turn = Some(turn);
    h.agent(fence.run, AgentEvent::TurnStarted { turn_id: turn });
    let request = QuestionRequestId::mint();
    h.agent(
        fence.run,
        AgentEvent::QuestionsRequired {
            turn_id: turn,
            request_id: request,
            questions: vec![],
            blocking: true,
            delivery: QuestionsDelivery::InTurn,
        },
    );
    let mut wrong = Vec::new();
    let mut value = fence.clone();
    value.owner.user_scope = serde_json::from_value(serde_json::json!(v4(0x42))).unwrap();
    wrong.push(value);
    let mut value = fence.clone();
    value.bot = BotId::mint();
    wrong.push(value);
    let mut value = fence.clone();
    value.attempt = RunAttemptId::mint();
    wrong.push(value);
    let mut value = fence.clone();
    value.generation = AttemptGeneration::from_raw(2).unwrap();
    wrong.push(value);
    let mut value = fence.clone();
    value.session = SessionId::mint();
    wrong.push(value);
    let mut value = fence.clone();
    value.turn = Some(AgentTurnId::mint());
    wrong.push(value);
    for stale in wrong {
        assert!(
            h.orchestrator
                .bot_skip_questions(&stale, fence.owner.device, request)
                .is_err()
        );
    }
    assert!(
        h.orchestrator
            .bot_skip_questions(&fence, DeviceId::from_uuid(v4(0x43)), request)
            .is_err()
    );
    assert!(
        h.orchestrator
            .bot_skip_questions(&fence, fence.owner.device, QuestionRequestId::mint())
            .is_err()
    );
    assert!(h.sessions.lock().unwrap().bot_skips.is_empty());
    h.orchestrator
        .bot_skip_questions(&fence, fence.owner.device, request)
        .unwrap();
    assert_eq!(
        h.sessions.lock().unwrap().bot_skips,
        vec![(fence.session, request)]
    );
    // Acknowledgment alone does not invent a native resolution.
    h.agent(
        fence.run,
        AgentEvent::QuestionsResolved {
            turn_id: turn,
            request_id: request,
            outcome: QuestionsOutcome::Skipped,
        },
    );
    assert!(
        h.orchestrator
            .bot_skip_questions(&fence, fence.owner.device, request)
            .is_err()
    );
    let next = QuestionRequestId::mint();
    h.agent(
        fence.run,
        AgentEvent::QuestionsRequired {
            turn_id: turn,
            request_id: next,
            questions: vec![],
            blocking: true,
            delivery: QuestionsDelivery::InTurn,
        },
    );
    h.session_event(
        fence.run,
        SessionEvent::HandoverChanged {
            handover: HandoverState::Requested,
        },
    );
    h.session_event(
        fence.run,
        SessionEvent::HandoverChanged {
            handover: HandoverState::Taken,
        },
    );
    assert!(
        h.orchestrator
            .bot_skip_questions(&fence, fence.owner.device, next)
            .is_err()
    );
    assert_eq!(h.sessions.lock().unwrap().bot_skips.len(), 1);
}
#[test]
fn bot_start_requires_observation_and_retries_one_exact_transient_input() {
    let mut h = bot_harness();
    let s = submission(1);
    let fence = prepared(&mut h, &s);
    assert!(
        h.orchestrator
            .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
            .is_err()
    );
    observe(&mut h, &fence);
    let turn = h
        .orchestrator
        .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
        .unwrap();
    assert_eq!(
        h.orchestrator
            .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
            .unwrap(),
        turn
    );
    let mut changed = s.input.clone();
    changed.text.push_str("changed");
    assert!(
        h.orchestrator
            .bot_begin_turn(&fence, fence.owner.device, changed)
            .is_err()
    );
    assert_eq!(count_submits(&h), 1);
    assert!(
        h.orchestrator
            .start(s.request.run, StartOrigin::Manual)
            .is_err()
    );
}
#[test]
fn lost_initial_ack_reconciles_native_idempotency_without_second_turn() {
    let mut h = bot_harness();
    let s = submission(2);
    let fence = prepared(&mut h, &s);
    observe(&mut h, &fence);
    h.sessions.lock().unwrap().bot_reply_lost = true;
    assert!(
        h.orchestrator
            .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
            .is_err()
    );
    assert_eq!(count_submits(&h), 1);
    h.orchestrator
        .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
        .unwrap();
    assert_eq!(count_submits(&h), 1);
}
fn begin_refusal(h: &mut Harness, fence: &BotExecutionFence, s: &BotSubmission) -> PortError {
    match h
        .orchestrator
        .bot_begin_turn(fence, fence.owner.device, s.input.clone())
    {
        Err(RunError::Port(error)) => error,
        other => panic!("expected a port refusal, got {other:?}"),
    }
}
#[test]
fn a_definite_session_refusal_of_the_initial_input_names_its_reason_and_is_not_retryable() {
    // The session pins its answer to the input's key, so asking again can only
    // repeat it. Before this, every such answer surfaced as the generic
    // retryable `port` code and the AI re-issued the same Begin until the
    // ten-minute budget ended the task.
    for (session, code) in [
        ("input_too_large", "bot_input_too_large"),
        ("unsupported_media", "bot_input_pictures_refused"),
        ("invalid_image", "bot_input_pictures_refused"),
        ("capability_not_negotiated", "bot_input_capability_refused"),
        ("provider_model_unavailable", "bot_input_model_refused"),
        ("invalid_transition", "bot_input_invalid"),
    ] {
        let mut h = bot_harness();
        let s = submission(40);
        let fence = prepared(&mut h, &s);
        observe(&mut h, &fence);
        h.sessions.lock().unwrap().bot_refusal = Some(session);
        for _ in 0..2 {
            let error = begin_refusal(&mut h, &fence, &s);
            assert_eq!(error.code, code, "{session}");
            assert_eq!(error.port, Port::Sessions);
            assert!(!error.detail.contains("SECRET"), "no session text travels");
        }
        // Each repeat reached the session once and was refused again.
        assert_eq!(count_submits(&h), 2);
    }
}
#[test]
fn path_references_are_part_of_the_frozen_initial_input_and_never_sent_twice() {
    let mut h = bot_harness();
    let s = submission(42);
    let fence = prepared(&mut h, &s);
    observe(&mut h, &fence);
    let mut input = s.input.clone();
    input.references = vec![koru_session_protocol::PathReference {
        kind: koru_session_protocol::PathReferenceKind::Folder,
        path: "C:\\work folder\\文書".into(),
    }];
    let turn = h
        .orchestrator
        .bot_begin_turn(&fence, fence.owner.device, input.clone())
        .unwrap();
    // An exact replay is answered from the ledger: the session is not asked.
    assert_eq!(
        h.orchestrator
            .bot_begin_turn(&fence, fence.owner.device, input.clone())
            .unwrap(),
        turn
    );
    // The same key with different references is a different input.
    let mut changed = input.clone();
    changed.references[0].path = "C:\\work folder\\other".into();
    assert!(
        h.orchestrator
            .bot_begin_turn(&fence, fence.owner.device, changed)
            .is_err()
    );
    let mut dropped = input.clone();
    dropped.references.clear();
    assert!(
        h.orchestrator
            .bot_begin_turn(&fence, fence.owner.device, dropped)
            .is_err()
    );
    assert_eq!(count_submits(&h), 1);
    let sent = h
        .sessions
        .lock()
        .unwrap()
        .bot_inputs
        .values()
        .next()
        .cloned();
    assert_eq!(sent.unwrap().0.references, input.references);
}
#[test]
fn no_answer_or_a_session_state_answer_to_the_initial_input_stays_uncertain_and_retryable() {
    for session in [
        "disconnected",
        "timed_out",
        "unavailable",
        "backpressure",
        "stale_lease",
        "session_exited",
        "provider_failed",
        "internal",
    ] {
        let mut h = bot_harness();
        let s = submission(41);
        let fence = prepared(&mut h, &s);
        observe(&mut h, &fence);
        h.sessions.lock().unwrap().bot_refusal = Some(session);
        let error = begin_refusal(&mut h, &fence, &s);
        assert_eq!(error.code, session, "unchanged, so still retryable");
        assert!(!error.code.starts_with("bot_"));
        // The same exact input may still begin once the session answers.
        h.sessions.lock().unwrap().bot_refusal = None;
        h.orchestrator
            .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
            .unwrap();
        assert_eq!(count_submits(&h), 2);
    }
}
#[test]
fn pause_before_dispatch_keeps_queue_and_resume_uses_original_snapshot() {
    let mut h = bot_harness();
    let s = submission(3);
    let owner = s.provenance.owner;
    h.orchestrator.bot_submit(s.clone(), owner.device).unwrap();
    let mut barrier = BotAdmissionBarrier {
        owner,
        bot: s.provenance.bot,
        request: BotRequestId::mint(),
        epoch: 2,
        paused: true,
    };
    h.orchestrator.bot_barrier(&barrier, owner.device).unwrap();
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&s), owner.device)
            .unwrap()
            .is_none()
    );
    barrier.epoch = 3;
    barrier.paused = false;
    barrier.request = BotRequestId::mint();
    h.orchestrator.bot_barrier(&barrier, owner.device).unwrap();
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&s), owner.device)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        h.schedule_store
            .bot_admission(s.request.run)
            .unwrap()
            .unwrap()
            .submission,
        s
    );
}
#[test]
fn native_completion_waits_for_ai_delivery_and_terminal_observation_survives_stop() {
    let mut h = bot_harness();
    let s = submission(4);
    let fence = prepared(&mut h, &s);
    observe(&mut h, &fence);
    let turn = h
        .orchestrator
        .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
        .unwrap();
    h.agent(s.request.run, AgentEvent::TurnCompleted { turn_id: turn });
    assert!(!h.orchestrator.status(s.request.run).unwrap().is_terminal());
    assert!(
        h.orchestrator
            .header(s.request.run)
            .unwrap()
            .artifacts
            .is_empty()
    );
    assert_eq!(
        h.orchestrator.bot_budget_delay_ms().unwrap(),
        None,
        "proven native idle does not consume active budget"
    );
    assert!(
        h.orchestrator
            .bot_observe(&fence, fence.owner.device, None)
            .is_ok()
    );
    h.orchestrator
        .bot_stop(&key(&s), fence.owner.device, Some(&fence))
        .unwrap();
    assert!(
        h.orchestrator
            .bot_observe(&fence, fence.owner.device, None)
            .is_ok()
    );
    assert!(
        h.orchestrator
            .bot_begin_turn(&fence, fence.owner.device, s.input)
            .is_err()
    );
    assert_eq!(count_submits(&h), 1);
}
#[test]
fn stop_uncertainty_retains_claim_and_reservation_then_verified_stop_releases() {
    let mut h = bot_harness();
    let s = submission(5);
    let fence = prepared(&mut h, &s);
    let row = h
        .schedule_store
        .current_attempt(s.request.run)
        .unwrap()
        .unwrap();
    let native = row.provider_session.unwrap().idempotency;
    h.sessions
        .lock()
        .unwrap()
        .lookups
        .insert(native, (host(2), CreationLookup::NoEvidence));
    assert!(
        h.orchestrator
            .bot_stop(&key(&s), fence.owner.device, Some(&fence))
            .is_err()
    );
    assert!(
        !h.schedule_store
            .current_attempt(s.request.run)
            .unwrap()
            .unwrap()
            .retired
    );
    assert_eq!(
        h.schedule_store
            .bot_execution(s.request.run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Executing
    );
    assert!(
        !h.orchestrator
            .claims()
            .holders_of(&koru_delegate_scope::ScopeIdentity::fixture_canonical(
                Path::new("C:/fixture/bot-5")
            ))
            .is_empty()
    );
    h.sessions
        .lock()
        .unwrap()
        .lookups
        .insert(native, (host(1), CreationLookup::NoEvidence));
    h.orchestrator
        .bot_stop(&key(&s), fence.owner.device, Some(&fence))
        .unwrap();
    assert_eq!(
        h.schedule_store
            .bot_execution(s.request.run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Settled
    );
}
#[test]
fn owner_mismatch_cannot_read_control_or_change_barrier() {
    let mut h = bot_harness();
    let s = submission(6);
    let fence = prepared(&mut h, &s);
    let other = DeviceId::from_uuid(v4(0xfd));
    assert!(h.orchestrator.bot_observe(&fence, other, None).is_err());
    assert!(
        h.orchestrator
            .bot_stop(&key(&s), other, Some(&fence))
            .is_err()
    );
    let barrier = BotAdmissionBarrier {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        request: BotRequestId::mint(),
        epoch: 2,
        paused: true,
    };
    assert!(h.orchestrator.bot_barrier(&barrier, other).is_err());
}

struct BotClock(Arc<AtomicI64>);
impl crate::Clock for BotClock {
    fn now_ms(&mut self) -> i64 {
        self.0.load(Ordering::Acquire)
    }
}
fn bot_harness() -> Harness {
    let mut h = Harness::without_folders();
    h.orchestrator.ports_mut().clock = Box::new(BotClock(h.schedule_clock.clone()));
    h.orchestrator.ports_mut().preamble = Box::new(BotPreamble("stable fixture preamble"));
    h
}
#[test]
fn genuine_takeover_before_initial_input_fences_then_handback_does_not_send() {
    use koru_session_protocol::HandoverState;
    let mut h = bot_harness();
    let s = submission(7);
    let fence = prepared(&mut h, &s);
    observe(&mut h, &fence);
    for state in [HandoverState::Requested, HandoverState::Taken] {
        h.session_event(
            s.request.run,
            SessionEvent::HandoverChanged { handover: state },
        );
        assert!(
            h.orchestrator
                .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
                .is_err()
        );
    }
    h.session_event(
        s.request.run,
        SessionEvent::HandoverChanged {
            handover: HandoverState::Returned,
        },
    );
    assert_eq!(count_submits(&h), 0);
    h.orchestrator
        .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
        .unwrap();
    assert_eq!(count_submits(&h), 1);
}
#[test]
fn owner_stop_works_while_human_holds_the_session() {
    use koru_session_protocol::HandoverState;
    let mut h = bot_harness();
    let s = submission(8);
    let fence = prepared(&mut h, &s);
    h.session_event(
        s.request.run,
        SessionEvent::HandoverChanged {
            handover: HandoverState::Requested,
        },
    );
    h.session_event(
        s.request.run,
        SessionEvent::HandoverChanged {
            handover: HandoverState::Taken,
        },
    );
    h.orchestrator
        .bot_stop(&key(&s), fence.owner.device, Some(&fence))
        .unwrap();
    assert_eq!(h.status(s.request.run), RunStatus::Cancelled);
    assert!(h.calls().contains(&Call::Terminate(fence.session)));
}
#[test]
fn native_approval_wait_is_excluded_from_durable_active_budget() {
    let mut h = bot_harness();
    let s = submission(9);
    let fence = prepared(&mut h, &s);
    observe(&mut h, &fence);
    let turn = h
        .orchestrator
        .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
        .unwrap();
    let approval = v4(0x71).to_string().parse().unwrap();
    h.schedule_clock.fetch_add(1000, Ordering::AcqRel);
    h.agent(
        s.request.run,
        AgentEvent::ApprovalRequired {
            turn_id: turn,
            approval_id: approval,
            summary: "fixture".into(),
            choices: vec![],
        },
    );
    assert_eq!(h.orchestrator.bot_budget_delay_ms().unwrap(), None);
    h.schedule_clock.fetch_add(3_600_000, Ordering::AcqRel);
    assert!(!h.orchestrator.bot_budget_tick(s.request.run).unwrap());
    let paused = h
        .schedule_store
        .bot_execution(s.request.run)
        .unwrap()
        .unwrap();
    assert_eq!(paused.active_elapsed_ms, 1000);
    h.agent(
        s.request.run,
        AgentEvent::ApprovalResolved {
            turn_id: turn,
            approval_id: approval,
            decision: ApprovalDecision::Approve,
            choice: None,
        },
    );
    h.schedule_clock.fetch_add(599_000, Ordering::AcqRel);
    assert!(h.orchestrator.bot_budget_tick(s.request.run).unwrap());
    assert_eq!(h.status(s.request.run), RunStatus::Cancelled);
}
#[test]
fn restart_after_admission_before_document_creation_keeps_pinned_context_and_does_not_start() {
    let mut h = bot_harness();
    let mut s = submission(10);
    s.request.context = request(s.request.run, s.request.scope.clone()).context;
    h.schedule_store.admit_bot(&s).unwrap();
    assert_eq!(
        h.schedule_store
            .intent(s.request.run)
            .unwrap()
            .unwrap()
            .state,
        IntentState::Pending
    );
    let report = h.orchestrator.restart().unwrap();
    assert_eq!(report.completed_forward, vec![s.request.run]);
    assert!(report.started.is_empty());
    assert_eq!(
        h.orchestrator.header(s.request.run).unwrap().context,
        s.request.context
    );
    assert!(
        h.calls()
            .iter()
            .all(|call| !matches!(call, Call::Create { .. }))
    );
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&s), s.provenance.owner.device)
            .unwrap()
            .is_some()
    );
}
#[test]
fn full_host_reconciles_reserved_slots_before_admitting_new_queued_work() {
    let mut h = bot_harness();
    let a = submission(11);
    let mut b = submission(12);
    let mut c = submission(13);
    b.provenance.bot = BotId::from_uuid(v4(0xd1));
    c.provenance.bot = BotId::from_uuid(v4(0xd2));
    for s in [&a, &b, &c] {
        h.orchestrator
            .bot_submit(s.clone(), s.provenance.owner.device)
            .unwrap();
    }
    h.schedule_store.reserve_bot(a.request.run).unwrap();
    h.schedule_store.reserve_bot(b.request.run).unwrap();
    assert_eq!(
        h.schedule_store
            .active_bot_executions(a.provenance.owner.device)
            .unwrap()
            .len(),
        2
    );
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&c), c.provenance.owner.device)
            .unwrap()
            .is_none()
    );
    let candidates = h.orchestrator.bot_candidates(0, 10).unwrap();
    assert_eq!(candidates.len(), 3);
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&a), a.provenance.owner.device)
            .unwrap()
            .is_some()
    );
    h.sessions.lock().unwrap().create.as_mut().unwrap().session = session(2);
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&b), b.provenance.owner.device)
            .unwrap()
            .is_some()
    );
    assert_eq!(h.orchestrator.bot_candidates(0, 10).unwrap().len(), 1);
}
#[test]
fn unknown_restart_retains_reservation_across_another_restart_without_launching() {
    let mut h = bot_harness();
    let s = submission(14);
    let fence = prepared(&mut h, &s);
    let native = h
        .attempt(s.request.run)
        .provider_session
        .unwrap()
        .idempotency;
    let mut h = restarted(h, (host(2), CreationLookup::NoEvidence));
    h.sessions.lock().unwrap().host = host(2);
    let report = h.orchestrator.restart().unwrap();
    assert!(report.claims.kept.len() == 1);
    assert!(!h.attempt(s.request.run).retired);
    assert_eq!(h.attempt(s.request.run).phase, AttemptPhase::Unknown);
    let mut h = restarted(h, (host(2), CreationLookup::NoEvidence));
    h.sessions
        .lock()
        .unwrap()
        .lookups
        .insert(native, (host(2), CreationLookup::NoEvidence));
    let report = h.orchestrator.restart().unwrap();
    assert!(report.unresolved.contains(&s.request.run));
    assert!(!h.attempt(s.request.run).retired);
    assert_eq!(
        h.schedule_store
            .bot_execution(s.request.run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Executing
    );
    assert!(
        h.calls()
            .iter()
            .all(|call| !matches!(call, Call::Create { .. } | Call::Submit { .. }))
    );
    assert!(
        h.orchestrator
            .bot_observe(&fence, fence.owner.device, None)
            .is_ok()
    );
}
#[test]
fn reattached_bot_uses_durable_send_identity_without_timeline_guess_or_resubmit() {
    let mut h = bot_harness();
    let s = submission(15);
    let fence = prepared(&mut h, &s);
    observe(&mut h, &fence);
    let turn = h
        .orchestrator
        .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
        .unwrap();
    // No TurnStarted step was observed before this crash.
    let mut h = restarted(
        h,
        (
            host(1),
            CreationLookup::Known {
                session: summary(ManagedState::Running, Some(turn)),
            },
        ),
    );
    let report = h.orchestrator.restart().unwrap();
    assert_eq!(report.reattached.len(), 1);
    assert_eq!(count_submits(&h), 0);
    observe(&mut h, &fence);
    assert_eq!(
        h.orchestrator
            .bot_begin_turn(&fence, fence.owner.device, s.input)
            .unwrap(),
        turn
    );
    assert_eq!(count_submits(&h), 0);
}

struct BotPreamble(&'static str);
impl Preamble for BotPreamble {
    fn resolve(&mut self) -> koru_delegate_preamble::Resolved {
        koru_delegate_preamble::Resolved {
            text: self.0.into(),
            source: koru_delegate_preamble::Source::Bundled,
            notice: None,
            path: None,
        }
    }
}
#[test]
fn initial_input_contains_pinned_context_instructions_and_ai_history_exactly_once() {
    let mut h = bot_harness();
    let mut s = submission(16);
    s.input.text = "AI-history-sentinel\ncurrent-task-sentinel".into();
    let fence = prepared(&mut h, &s);
    observe(&mut h, &fence);
    h.orchestrator
        .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
        .unwrap();
    let sent = h.submissions();
    assert_eq!(sent.len(), 1);
    for marker in [
        "You are working as a Bot in Koru.",
        "AI-history-sentinel",
        "current-task-sentinel",
        ".koru",
    ] {
        assert_eq!(sent[0].matches(marker).count(), 1);
    }
    assert!(sent[0].contains(&s.request.run.to_string()));
    assert!(!sent[0].contains("stable fixture preamble"));
    assert!(!sent[0].contains(".artifacts"));
    h.orchestrator
        .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
        .unwrap();
    assert_eq!(count_submits(&h), 1);
    h.orchestrator.ports_mut().preamble = Box::new(BotPreamble("changed preamble"));
    // Ordinary artifact instructions are not part of a Bot's retry identity.
    h.orchestrator
        .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
        .unwrap();
    s.input.text.push_str("changed task");
    assert!(
        h.orchestrator
            .bot_begin_turn(&fence, fence.owner.device, s.input)
            .is_err()
    );
    assert_eq!(count_submits(&h), 1);
}

#[test]
fn prepared_session_restart_requires_fresh_observer_and_never_auto_submits() {
    let mut h = bot_harness();
    let s = submission(17);
    let fence = prepared(&mut h, &s);
    let mut h = restarted(
        h,
        (
            host(1),
            CreationLookup::Known {
                session: summary(ManagedState::Idle, None),
            },
        ),
    );
    let report = h.orchestrator.restart().unwrap();
    assert_eq!(report.reattached.len(), 1);
    assert!(matches!(h.status(s.request.run), RunStatus::Starting));
    assert!(
        h.orchestrator
            .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
            .is_err()
    );
    observe(&mut h, &fence);
    h.orchestrator
        .bot_begin_turn(&fence, fence.owner.device, s.input)
        .unwrap();
    assert_eq!(count_submits(&h), 1);
}
#[test]
fn missing_working_folder_refuses_without_creating_or_launching_replacement() {
    let mut h = bot_harness();
    let mut s = submission(18);
    s.request.scope = WorkspaceScope::BotFolder {
        folder: BotWorkingFolder::new("C:/fixture/missing".into()).unwrap(),
    };
    h.orchestrator
        .bot_submit(s.clone(), s.provenance.owner.device)
        .unwrap();
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&s), s.provenance.owner.device)
            .is_err()
    );
    assert!(h.calls().is_empty());
    assert!(
        h.schedule_store
            .current_attempt(s.request.run)
            .unwrap()
            .is_none()
    );
}
#[test]
fn retained_schedule_identity_blocks_only_overlapping_bot_folder() {
    let mut h = bot_harness();
    let s = submission(19);
    h.orchestrator
        .bot_submit(s.clone(), s.provenance.owner.device)
        .unwrap();
    let overlap = koru_delegate_scope::ScopeIdentity::fixture_canonical(Path::new("C:/fixture"));
    assert!(
        h.orchestrator
            .bot_dispatch_reserved(&key(&s), s.provenance.owner.device, &[overlap])
            .unwrap()
            .is_none()
    );
    let disjoint = koru_delegate_scope::ScopeIdentity::fixture_canonical(Path::new("C:/unrelated"));
    assert!(
        h.orchestrator
            .bot_dispatch_reserved(&key(&s), s.provenance.owner.device, &[disjoint])
            .unwrap()
            .is_some()
    );
}

#[test]
fn retirement_settlement_failure_retries_without_releasing_folder_claim_early() {
    let mut h = bot_harness();
    let s = submission(20);
    let fence = prepared(&mut h, &s);
    h.ledger.lock().unwrap().fail = Some("bot_settle");
    assert!(
        h.orchestrator
            .bot_stop(&key(&s), fence.owner.device, Some(&fence))
            .is_err()
    );
    assert!(h.attempt(s.request.run).retired);
    assert_eq!(h.orchestrator.claims().len(), 1);
    assert_eq!(
        h.schedule_store
            .bot_execution(s.request.run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Executing
    );
    h.orchestrator
        .bot_stop(&key(&s), fence.owner.device, Some(&fence))
        .unwrap();
    assert_eq!(h.orchestrator.claims().len(), 0);
    assert_eq!(
        h.schedule_store
            .bot_execution(s.request.run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Settled
    );
}

#[test]
fn recovering_creation_keeps_budget_deadline_and_claim_without_a_second_launch() {
    let mut h = bot_harness();
    let s = submission(21);
    prepared(&mut h, &s);
    let mut h = restarted(h, (host(1), CreationLookup::Creating));
    let report = h.orchestrator.restart().unwrap();
    assert!(report.unresolved.contains(&s.request.run));
    assert_eq!(h.orchestrator.claims().len(), 1);
    assert_eq!(h.orchestrator.bot_budget_runs(), vec![s.request.run]);
    assert!(h.orchestrator.bot_budget_delay_ms().unwrap().is_some());
    assert!(
        h.calls()
            .iter()
            .all(|call| !matches!(call, Call::Create { .. } | Call::Submit { .. }))
    );
}

#[test]
fn lost_initial_reply_after_restart_reconciles_exact_turn_without_claiming_human_turn() {
    for different_human_turn in [false, true] {
        let mut h = bot_harness();
        let s = submission(22);
        let fence = prepared(&mut h, &s);
        observe(&mut h, &fence);
        h.sessions.lock().unwrap().bot_reply_lost = true;
        assert!(
            h.orchestrator
                .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
                .is_err()
        );
        let native = h.sessions.lock().unwrap().bot_inputs[&s.input.idempotency_key].1;
        let active = if different_human_turn {
            AgentTurnId::mint()
        } else {
            native
        };
        let mut h = restarted(
            h,
            (
                host(1),
                CreationLookup::Known {
                    session: summary(ManagedState::Running, Some(active)),
                },
            ),
        );
        assert_eq!(h.orchestrator.restart().unwrap().reattached.len(), 1);
        assert_eq!(count_submits(&h), 0);
        observe(&mut h, &fence);
        assert_eq!(
            h.orchestrator
                .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
                .unwrap(),
            native
        );
        assert_eq!(count_submits(&h), 0);
        let attempt = h.orchestrator.runs[&s.request.run]
            .attempt
            .as_ref()
            .unwrap();
        if different_human_turn {
            assert_eq!(attempt.recovered_unowned_turn, Some(active));
            assert_eq!(attempt.orchestrator_turn, None);
            assert!(
                h.orchestrator
                    .bot_live_fence(&fence, fence.owner.device, true, false)
                    .is_err()
            );
        } else {
            assert_eq!(attempt.recovered_unowned_turn, None);
            assert_eq!(attempt.orchestrator_turn, Some(native));
            assert!(
                h.orchestrator
                    .bot_live_fence(&fence, fence.owner.device, true, false)
                    .is_ok()
            );
        }
    }
}

#[test]
fn prepared_restart_with_genuine_human_controller_uses_existing_handover_state() {
    let mut h = bot_harness();
    let s = submission(23);
    let fence = prepared(&mut h, &s);
    let mut h = restarted(
        h,
        (
            host(1),
            CreationLookup::Known {
                session: summary(ManagedState::Idle, None),
            },
        ),
    );
    h.sessions.lock().unwrap().attach_fails = true;
    assert_eq!(h.orchestrator.restart().unwrap().reattached.len(), 1);
    assert!(matches!(
        h.status(s.request.run).control(),
        Some(ControlState::Human { .. })
    ));
    observe(&mut h, &fence);
    assert!(
        h.orchestrator
            .bot_begin_turn(&fence, fence.owner.device, s.input)
            .is_err()
    );
    assert_eq!(count_submits(&h), 0);
}

mod delivery;
