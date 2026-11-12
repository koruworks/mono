// SPDX-License-Identifier: MPL-2.0
use super::*;

fn delivered_turn() -> (
    Harness,
    BotSubmission,
    BotExecutionFence,
    BotDeliveryReceipt,
) {
    let mut h = bot_harness();
    let s = submission(30);
    let fence = prepared(&mut h, &s);
    observe(&mut h, &fence);
    let turn = h
        .orchestrator
        .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
        .unwrap();
    h.agent(s.request.run, AgentEvent::TurnCompleted { turn_id: turn });
    {
        let mut script = h.sessions.lock().unwrap();
        script
            .summaries
            .insert(fence.session, summary(ManagedState::Completed, None));
        script.observation.events.push(SequencedAgentEvent {
            session_id: fence.session,
            sequence: StreamSequence::new(50).unwrap(),
            event: AgentEvent::TurnCompleted { turn_id: turn },
        });
    }
    let receipt = BotDeliveryReceipt {
        run: fence.run,
        attempt: fence.attempt,
        generation: fence.generation,
        chat_id: s.provenance.chat_id,
        ai_attempt_id: s.provenance.ai_attempt_id.unwrap(),
        assistant_message_id: s.provenance.assistant_message_id.unwrap(),
        confirmed_at_ms: 1000,
    };
    (h, s, fence, receipt)
}

fn complete_count(h: &mut Harness, run: RunId) -> usize {
    h.orchestrator.open(run).unwrap().doc.document().timeline_all().into_iter()
        .filter(|item| matches!(item, koru_delegate_document::TimelineItem::Step(entry) if entry.kind == StepKind::Completed)).count()
}

#[test]
fn durable_delivery_completes_without_fabricating_artifacts_and_replays_once() {
    let (mut h, s, fence, receipt) = delivered_turn();
    h.orchestrator
        .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
        .unwrap();
    let header = h.orchestrator.header(fence.run).unwrap();
    assert_eq!(header.status, RunStatus::Completed);
    assert_eq!(header.chat_delivery, Some(receipt.clone()));
    assert!(header.artifacts.is_empty());
    assert!(header.native_results.is_empty());
    assert_eq!(complete_count(&mut h, fence.run), 1);
    let id = h
        .schedule_store
        .inner
        .lock()
        .unwrap()
        .bot_admission(fence.run)
        .unwrap()
        .unwrap()
        .delivery_publication
        .unwrap();
    assert!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .publication(id)
            .unwrap()
            .unwrap()
            .acknowledged_at_ms
            .is_some()
    );
    h.orchestrator
        .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
        .unwrap();
    assert_eq!(complete_count(&mut h, fence.run), 1);
    assert_eq!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .bot_admission(fence.run)
            .unwrap()
            .unwrap()
            .delivery_publication,
        Some(id)
    );
    assert_eq!(count_submits(&h), 1);
}

#[test]
fn wrong_identity_and_ordinary_transition_refuse_before_completion() {
    let (mut h, s, fence, receipt) = delivered_turn();
    for field in 0..4 {
        let mut wrong = receipt.clone();
        match field {
            0 => wrong.chat_id = uuid::Uuid::new_v4(),
            1 => wrong.ai_attempt_id = uuid::Uuid::new_v4(),
            2 => wrong.assistant_message_id = uuid::Uuid::new_v4(),
            _ => wrong.attempt = RunAttemptId::mint(),
        }
        assert!(
            h.orchestrator
                .bot_acknowledge_delivery(&key(&s), fence.owner.device, &wrong)
                .is_err()
        );
    }
    assert!(
        h.orchestrator
            .apply(fence.run, &RunEvent::BotResponseDelivered)
            .is_err()
    );
    assert!(
        h.orchestrator
            .header(fence.run)
            .unwrap()
            .chat_delivery
            .is_none()
    );
    assert_ne!(h.status(fence.run), RunStatus::Completed);
}

#[test]
fn human_takeover_retains_delivery_and_waits_for_genuine_handback() {
    use koru_session_protocol::HandoverState;
    let (mut h, s, fence, receipt) = delivered_turn();
    h.session_event(
        fence.run,
        SessionEvent::HandoverChanged {
            handover: HandoverState::Taken,
        },
    );
    assert!(
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
            .is_err()
    );
    assert_eq!(
        h.orchestrator.header(fence.run).unwrap().chat_delivery,
        Some(receipt.clone())
    );
    assert_ne!(h.status(fence.run), RunStatus::Completed);
    assert_eq!(complete_count(&mut h, fence.run), 0);
    h.session_event(
        fence.run,
        SessionEvent::HandoverChanged {
            handover: HandoverState::Returned,
        },
    );
    h.sessions
        .lock()
        .unwrap()
        .summaries
        .get_mut(&fence.session)
        .unwrap()
        .handover = HandoverState::Returned;
    h.orchestrator
        .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
        .unwrap();
    assert_eq!(h.status(fence.run), RunStatus::Completed);
    assert_eq!(count_submits(&h), 1);
}

#[test]
fn absorbing_non_success_keeps_delivery_history_without_success() {
    for status in [
        RunStatus::Cancelled,
        RunStatus::Failed {
            stage: FailureStage::Turn,
        },
        RunStatus::Interrupted {
            prior_attempt: PriorAttempt::Unknown,
        },
    ] {
        let (mut h, s, fence, receipt) = delivered_turn();
        h.orchestrator
            .open(fence.run)
            .unwrap()
            .doc
            .commit(&mut |doc| doc.set_status(&status))
            .unwrap();
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
            .unwrap();
        assert_eq!(h.status(fence.run), status);
        assert_eq!(
            h.orchestrator.header(fence.run).unwrap().chat_delivery,
            Some(receipt)
        );
        assert_eq!(complete_count(&mut h, fence.run), 0);
    }
}

#[test]
fn active_or_unproven_native_turn_never_completes_from_ai_receipt_alone() {
    for active in [false, true] {
        let (mut h, s, fence, receipt) = delivered_turn();
        if active {
            h.sessions
                .lock()
                .unwrap()
                .summaries
                .get_mut(&fence.session)
                .unwrap()
                .active_turn_id = Some(AgentTurnId::mint());
        } else {
            h.sessions.lock().unwrap().observation.events.clear();
        }
        assert!(
            h.orchestrator
                .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
                .is_err()
        );
        assert_ne!(h.status(fence.run), RunStatus::Completed);
        assert_eq!(
            h.orchestrator.header(fence.run).unwrap().chat_delivery,
            Some(receipt)
        );
    }
}

#[test]
fn crash_between_document_and_enqueue_reuses_receipt_publication_and_single_step() {
    let (mut h, s, fence, receipt) = delivered_turn();
    h.ledger.lock().unwrap().fail = Some("enqueue");
    assert!(
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
            .is_err()
    );
    assert_eq!(h.status(fence.run), RunStatus::Completed);
    let id = h
        .schedule_store
        .inner
        .lock()
        .unwrap()
        .bot_admission(fence.run)
        .unwrap()
        .unwrap()
        .delivery_publication
        .unwrap();
    assert!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .publication(id)
            .unwrap()
            .is_none()
    );
    h.orchestrator
        .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
        .unwrap();
    assert_eq!(complete_count(&mut h, fence.run), 1);
    assert!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .publication(id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn enqueue_lost_ack_and_changed_existing_body_never_mint_a_new_completion() {
    let (mut h, s, fence, receipt) = delivered_turn();
    h.ledger.lock().unwrap().fail = Some("acknowledge");
    assert!(
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
            .is_err()
    );
    let id = h
        .schedule_store
        .inner
        .lock()
        .unwrap()
        .bot_admission(fence.run)
        .unwrap()
        .unwrap()
        .delivery_publication
        .unwrap();
    h.orchestrator
        .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
        .unwrap();
    assert_eq!(complete_count(&mut h, fence.run), 1);
    assert_eq!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .bot_admission(fence.run)
            .unwrap()
            .unwrap()
            .delivery_publication,
        Some(id)
    );
    // The generic journal accepts duplicate IDs; the Bot path must independently
    // reject a body not bound to its completed receipt.
    let (mut other, s, fence, receipt) = delivered_turn();
    let id = other
        .schedule_store
        .inner
        .lock()
        .unwrap()
        .record_bot_delivery(&receipt)
        .unwrap();
    other
        .schedule_store
        .inner
        .lock()
        .unwrap()
        .enqueue(id, fence.run, PublicationKind::Lifecycle, "{}")
        .unwrap();
    assert!(
        other
            .orchestrator
            .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
            .is_err()
    );
    assert_eq!(
        other
            .schedule_store
            .inner
            .lock()
            .unwrap()
            .publication(id)
            .unwrap()
            .unwrap()
            .body
            .as_str(),
        "{}"
    );
}

#[test]
fn pending_document_ack_retains_capacity_and_retries_the_same_completed_step() {
    for successful in [false, true] {
        let (mut h, s, fence, receipt) = delivered_turn();
        if !successful {
            h.orchestrator
                .open(fence.run)
                .unwrap()
                .doc
                .commit(&mut |doc| doc.set_status(&RunStatus::Cancelled))
                .unwrap();
        }
        {
            let mut script = h.documents.lock().unwrap();
            script.not_durable = true;
            script.hold_settle_ack = true;
        }
        assert!(
            h.orchestrator
                .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
                .is_err()
        );
        assert!(
            !h.schedule_store
                .inner
                .lock()
                .unwrap()
                .current_attempt(fence.run)
                .unwrap()
                .unwrap()
                .retired
        );
        let id = h
            .schedule_store
            .inner
            .lock()
            .unwrap()
            .bot_admission(fence.run)
            .unwrap()
            .unwrap()
            .delivery_publication
            .unwrap();
        assert!(
            h.schedule_store
                .inner
                .lock()
                .unwrap()
                .publication(id)
                .unwrap()
                .is_none()
        );
        assert_eq!(complete_count(&mut h, fence.run), usize::from(successful));
        h.documents.lock().unwrap().hold_settle_ack = false;
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
            .unwrap();
        assert_eq!(complete_count(&mut h, fence.run), usize::from(successful));
        assert_eq!(
            h.status(fence.run),
            if successful {
                RunStatus::Completed
            } else {
                RunStatus::Cancelled
            }
        );
    }
}

#[test]
fn confirmed_completion_waits_for_native_fencing_before_releasing_capacity() {
    let (mut h, s, fence, receipt) = delivered_turn();
    let native = h.attempt(fence.run).provider_session.unwrap().idempotency;
    h.sessions
        .lock()
        .unwrap()
        .lookups
        // The host still runs the process: no proof. (A new incarnation's
        // `NoEvidence` stood for that until window run 2; it now proves
        // nothing survived, ADR-341.)
        .insert(
            native,
            (
                host(2),
                CreationLookup::Known {
                    session: summary(ManagedState::Running, None),
                },
            ),
        );
    assert!(
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
            .is_err()
    );
    assert_eq!(h.status(fence.run), RunStatus::Completed);
    assert!(!h.attempt(fence.run).retired);
    assert_eq!(
        h.schedule_store
            .bot_execution(fence.run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Executing
    );
    let mut ended = summary(ManagedState::Completed, None);
    ended.process = ProcessState::Exited { code: Some(0) };
    h.sessions.lock().unwrap().lookups.insert(
        native,
        (host(2), CreationLookup::KnownEnded { session: ended }),
    );
    h.orchestrator
        .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
        .unwrap();
    assert!(h.attempt(fence.run).retired);
    assert_eq!(
        h.schedule_store
            .bot_execution(fence.run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Settled
    );
    assert_eq!(complete_count(&mut h, fence.run), 1);
    assert_eq!(count_submits(&h), 1);
}

#[test]
fn failed_canonical_write_retains_exact_ledger_receipt_without_success() {
    let (mut h, s, fence, receipt) = delivered_turn();
    h.documents.lock().unwrap().fail_commit = true;
    assert!(
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
            .is_err()
    );
    assert_ne!(h.status(fence.run), RunStatus::Completed);
    assert!(!h.attempt(fence.run).retired);
    let first = h.schedule_store.bot_admission(fence.run).unwrap().unwrap();
    assert_eq!(first.delivery, Some(receipt.clone()));
    assert!(first.delivery_publication.is_some());
    h.documents.lock().unwrap().fail_commit = false;
    h.orchestrator
        .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
        .unwrap();
    assert_eq!(
        h.schedule_store
            .bot_admission(fence.run)
            .unwrap()
            .unwrap()
            .delivery_publication,
        first.delivery_publication
    );
    assert_eq!(complete_count(&mut h, fence.run), 1);
}

#[test]
fn completed_retry_waits_when_a_human_took_control_before_cleanup() {
    use koru_session_protocol::HandoverState;
    let (mut h, s, fence, receipt) = delivered_turn();
    h.ledger.lock().unwrap().fail = Some("enqueue");
    assert!(
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
            .is_err()
    );
    assert_eq!(h.status(fence.run), RunStatus::Completed);
    h.sessions
        .lock()
        .unwrap()
        .summaries
        .get_mut(&fence.session)
        .unwrap()
        .handover = HandoverState::Taken;
    assert!(
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
            .is_err()
    );
    assert!(!h.calls().contains(&Call::Terminate(fence.session)));
    assert!(!h.attempt(fence.run).retired);
    h.sessions
        .lock()
        .unwrap()
        .summaries
        .get_mut(&fence.session)
        .unwrap()
        .handover = HandoverState::Returned;
    h.orchestrator
        .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
        .unwrap();
    assert!(h.attempt(fence.run).retired);
    assert_eq!(complete_count(&mut h, fence.run), 1);
}

#[test]
fn a_held_native_effect_blocks_chat_success_until_definitively_reconciled() {
    let mut h = bot_harness();
    let s = submission(31);
    let mut fence = prepared(&mut h, &s);
    observe(&mut h, &fence);
    let turn = h
        .orchestrator
        .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
        .unwrap();
    fence.turn = Some(turn);
    let policy = BotPolicyBarrier {
        key: BotPolicyBarrierKey {
            owner: fence.owner,
            bot: fence.bot,
            request: BotRequestId::mint().into(),
        },
        config_revision: 1,
        policy_revision: 1,
        policy: s.policy.clone(),
        effects_allowed: true,
    };
    h.orchestrator
        .synchronize_native_policy(policy, fence.owner.device)
        .unwrap();
    let request = PrepareNativeResult {
        key: NativeResultKey {
            run: fence.run,
            attempt: fence.attempt,
            generation: fence.generation,
            turn,
            operation_id: uuid::Uuid::new_v4(),
        },
        kind: NativeResultOperation::CreatePage,
        destination: s.workspace,
        target: None,
        request_fingerprint: [6; 32],
        expected_revision: None,
        config_revision: 1,
        policy_revision: 1,
        title: "result".into(),
    };
    assert_eq!(
        h.orchestrator
            .prepare_native_effect(&request, fence.owner.device)
            .unwrap()
            .disposition,
        NativeResultDisposition::Execute
    );
    h.agent(fence.run, AgentEvent::TurnCompleted { turn_id: turn });
    {
        let mut script = h.sessions.lock().unwrap();
        script
            .summaries
            .insert(fence.session, summary(ManagedState::Completed, None));
        script.observation.events.push(SequencedAgentEvent {
            session_id: fence.session,
            sequence: StreamSequence::new(50).unwrap(),
            event: AgentEvent::TurnCompleted { turn_id: turn },
        });
    }
    let receipt = BotDeliveryReceipt {
        run: fence.run,
        attempt: fence.attempt,
        generation: fence.generation,
        chat_id: s.provenance.chat_id,
        ai_attempt_id: s.provenance.ai_attempt_id.unwrap(),
        assistant_message_id: s.provenance.assistant_message_id.unwrap(),
        confirmed_at_ms: 1000,
    };
    assert!(
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
            .is_err()
    );
    assert_ne!(h.status(fence.run), RunStatus::Completed);
    assert!(!h.attempt(fence.run).retired);
    h.orchestrator
        .confirm_native_result(
            &ConfirmNativeResult {
                key: request.key,
                request_fingerprint: request.request_fingerprint,
                observation: NativeResultObservation::NotApplied {
                    code: NativeResultRefusal::Conflict,
                },
            },
            fence.owner.device,
        )
        .unwrap();
    h.orchestrator
        .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
        .unwrap();
    assert_eq!(h.status(fence.run), RunStatus::Completed);
    assert_eq!(complete_count(&mut h, fence.run), 1);
}

#[test]
fn takeover_between_summary_and_termination_never_uses_stop_authority() {
    let (mut h, s, fence, receipt) = delivered_turn();
    // Summary still says idle; the session owner sees a newly held human lease.
    h.sessions.lock().unwrap().owned_termination_refused = true;
    assert!(
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
            .is_err()
    );
    assert!(h.calls().contains(&Call::TerminateOwned(fence.session)));
    assert!(!h.calls().contains(&Call::Terminate(fence.session)));
    assert!(!h.attempt(fence.run).retired);
    h.sessions.lock().unwrap().owned_termination_refused = false;
    h.orchestrator
        .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
        .unwrap();
    assert!(h.attempt(fence.run).retired);
    assert!(!h.calls().contains(&Call::Terminate(fence.session)));
    assert_eq!(complete_count(&mut h, fence.run), 1);
}

#[test]
fn missing_summary_requires_positive_fencing_proof_without_termination() {
    let (mut h, s, fence, receipt) = delivered_turn();
    h.ledger.lock().unwrap().fail = Some("enqueue");
    assert!(
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
            .is_err()
    );
    let native = h.attempt(fence.run).provider_session.unwrap().idempotency;
    h.sessions.lock().unwrap().summaries.clear();
    h.sessions
        .lock()
        .unwrap()
        .lookups
        // The host still runs the process: no proof. (A new incarnation's
        // `NoEvidence` stood for that until window run 2; it now proves
        // nothing survived, ADR-341.)
        .insert(
            native,
            (
                host(2),
                CreationLookup::Known {
                    session: summary(ManagedState::Running, None),
                },
            ),
        );
    assert!(
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
            .is_err()
    );
    assert!(!h.attempt(fence.run).retired);
    assert!(!h.calls().contains(&Call::Terminate(fence.session)));
    assert!(!h.calls().contains(&Call::TerminateOwned(fence.session)));
    let mut ended = summary(ManagedState::Completed, None);
    ended.process = ProcessState::Exited { code: Some(0) };
    h.sessions.lock().unwrap().lookups.insert(
        native,
        (host(2), CreationLookup::KnownEnded { session: ended }),
    );
    h.orchestrator
        .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
        .unwrap();
    assert!(h.attempt(fence.run).retired);
}
