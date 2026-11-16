// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_session_protocol::HandoverState;

fn delivery(s: &BotSubmission, fence: &BotExecutionFence) -> BotDeliveryReceipt {
    BotDeliveryReceipt {
        run: fence.run,
        attempt: fence.attempt,
        generation: fence.generation,
        chat_id: s.provenance.chat_id,
        ai_attempt_id: s.provenance.ai_attempt_id.unwrap(),
        assistant_message_id: s.provenance.assistant_message_id.unwrap(),
        confirmed_at_ms: 1000,
    }
}
fn settle_unknown(h: &mut Harness, request: &PrepareNativeResult, device: DeviceId) {
    let target = h
        .orchestrator
        .check_native_effect_drain(&request.key, &NativeDrainReason::GatewayStartup, device)
        .unwrap()
        .unwrap();
    h.orchestrator
        .confirm_native_result(
            &ConfirmNativeResult {
                key: request.key.clone(),
                request_fingerprint: request.request_fingerprint,
                observation: NativeResultObservation::DrainedUnknown {
                    receipt: NativeEffectDrainReceipt {
                        binding: target.binding,
                        fenced_at: NativeEffectTimestamp::from_millis(100),
                        drained_at: NativeEffectTimestamp::from_millis(90),
                    },
                },
            },
            device,
        )
        .unwrap();
}
fn cleanup_due(h: &mut Harness, run: RunId) {
    h.orchestrator
        .attempt_mut(run)
        .unwrap()
        .bot_cleanup
        .as_mut()
        .unwrap()
        .due = std::time::Instant::now();
}

#[test]
fn cleanup_republishes_late_durable_proof_after_failed_and_lost_canonical_ack_once() {
    let (mut h, _, fence, policy, request) = active();
    let native = h.attempt(fence.run).provider_session.unwrap().idempotency;
    h.orchestrator
        .synchronize_native_policy(policy, fence.owner.device)
        .unwrap();
    let intent = h
        .orchestrator
        .prepare_native_effect(&request, fence.owner.device)
        .unwrap();
    h.orchestrator
        .apply(
            fence.run,
            &RunEvent::Interrupt {
                prior_attempt: PriorAttempt::Unknown,
            },
        )
        .unwrap();
    settle_unknown(&mut h, &request, fence.owner.device);
    let evidence = NativeResultReference {
        version_id: None,
        operation_id: request.key.operation_id,
        workspace: request.destination,
        document: intent.target,
        version: vec![1, 2],
        title: "Late Saved proof".into(),
        confirmed_at_ms: 900,
        kind: NativeResultKind::CreatedPage,
    };
    h.documents.lock().unwrap().fail_commit = true;
    assert!(
        h.orchestrator
            .confirm_native_result(
                &ConfirmNativeResult {
                    key: request.key.clone(),
                    request_fingerprint: request.request_fingerprint,
                    observation: NativeResultObservation::Durable {
                        evidence: evidence.clone()
                    },
                },
                fence.owner.device
            )
            .is_err()
    );
    cleanup_due(&mut h, fence.run);
    assert!(h.orchestrator.bot_budget_tick(fence.run).is_err());
    assert!(!h.attempt(fence.run).retired);
    let mut ended = summary(ManagedState::Completed, None);
    ended.process = ProcessState::Exited { code: Some(0) };
    h.sessions.lock().unwrap().lookups.insert(
        native,
        (host(1), CreationLookup::KnownEnded { session: ended }),
    );
    {
        let mut documents = h.documents.lock().unwrap();
        documents.fail_commit = false;
        documents.not_durable = true;
        documents.hold_settle_ack = true;
    }
    cleanup_due(&mut h, fence.run);
    assert!(h.orchestrator.bot_budget_tick(fence.run).is_err());
    assert!(h.orchestrator.native_effects_pending(fence.run).unwrap());
    assert!(!h.attempt(fence.run).retired);
    h.documents.lock().unwrap().hold_settle_ack = false;
    cleanup_due(&mut h, fence.run);
    assert!(h.orchestrator.bot_budget_tick(fence.run).unwrap());
    assert!(h.attempt(fence.run).retired);
    assert_eq!(h.orchestrator.claims().len(), 0);
    let header = h.orchestrator.header(fence.run).unwrap();
    assert_eq!(
        header.status,
        RunStatus::Interrupted {
            prior_attempt: PriorAttempt::Unknown
        }
    );
    assert_eq!(header.native_results, vec![evidence]);
    let retained = h
        .orchestrator
        .ports_mut()
        .store
        .native_results()
        .unwrap()
        .native_result(&request.key)
        .unwrap()
        .unwrap();
    assert_eq!(retained.stage, NativeResultStage::Registered);
    assert!(retained.drain.is_some());
    assert_eq!(
        retained.view().disposition,
        NativeResultDisposition::AlreadyRegistered
    );
    assert!(!h.calls().contains(&Call::Terminate(fence.session)));
}

#[test]
fn acknowledged_interruption_rebuilds_cleanup_and_releases_after_handback_without_ui() {
    let (mut h, s, fence, policy, request) = active();
    let native = h.attempt(fence.run).provider_session.unwrap().idempotency;
    h.orchestrator
        .synchronize_native_policy(policy, fence.owner.device)
        .unwrap();
    h.orchestrator
        .prepare_native_effect(&request, fence.owner.device)
        .unwrap();
    h.orchestrator
        .apply(
            fence.run,
            &RunEvent::Interrupt {
                prior_attempt: PriorAttempt::Unknown,
            },
        )
        .unwrap();
    // Legacy delivery may already have acknowledged history while Held remained.
    h.orchestrator
        .bot_acknowledge_delivery(&key(&s), fence.owner.device, &delivery(&s, &fence))
        .unwrap();
    assert!(!h.attempt(fence.run).retired);
    settle_unknown(&mut h, &request, fence.owner.device);
    assert!(
        h.orchestrator
            .attempt_mut(fence.run)
            .unwrap()
            .bot_cleanup
            .is_some()
    );
    let mut human = summary(ManagedState::Running, Some(fence.turn.unwrap()));
    human.handover = HandoverState::Taken;
    let lookup = CreationLookup::Known { session: human };
    let mut h = restarted(h, (host(1), lookup.clone()));
    {
        let mut sessions = h.sessions.lock().unwrap();
        sessions.lookups.insert(native, (host(1), lookup));
        sessions.owned_termination_refused = true;
    }
    assert!(
        h.orchestrator
            .restart()
            .unwrap()
            .unresolved
            .contains(&fence.run)
    );
    cleanup_due(&mut h, fence.run);
    assert!(!h.orchestrator.bot_budget_tick(fence.run).unwrap());
    assert!(!h.attempt(fence.run).retired);
    assert_eq!(h.orchestrator.claims().len(), 1);
    assert!(h.orchestrator.bot_budget_delay_ms().unwrap().is_some());
    // Handback authorizes owned termination, but its acceptance alone is not end proof.
    h.sessions.lock().unwrap().owned_termination_refused = false;
    cleanup_due(&mut h, fence.run);
    assert!(!h.orchestrator.bot_budget_tick(fence.run).unwrap());
    let mut ended = summary(ManagedState::Completed, None);
    ended.handover = HandoverState::Returned;
    ended.process = ProcessState::Exited { code: Some(0) };
    h.sessions.lock().unwrap().lookups.insert(
        native,
        (host(1), CreationLookup::KnownEnded { session: ended }),
    );
    {
        let mut documents = h.documents.lock().unwrap();
        documents.not_durable = true;
        documents.hold_settle_ack = true;
    }
    cleanup_due(&mut h, fence.run);
    assert!(h.orchestrator.bot_budget_tick(fence.run).is_err());
    assert!(!h.attempt(fence.run).retired);
    assert_eq!(h.orchestrator.claims().len(), 1);
    h.documents.lock().unwrap().hold_settle_ack = false;
    cleanup_due(&mut h, fence.run);
    assert!(h.orchestrator.bot_budget_tick(fence.run).unwrap());
    assert!(h.attempt(fence.run).retired);
    assert_eq!(h.orchestrator.claims().len(), 0);
    assert_eq!(
        h.status(fence.run),
        RunStatus::Interrupted {
            prior_attempt: PriorAttempt::Unknown
        }
    );
    assert!(!h.calls().contains(&Call::Terminate(fence.session)));
    assert!(h.calls().contains(&Call::TerminateOwned(fence.session)));
}

#[test]
fn drain_on_healthy_run_arms_cleanup_only_when_later_delivery_interrupts() {
    let (mut h, s, fence, policy, request) = active();
    let native = h.attempt(fence.run).provider_session.unwrap().idempotency;
    h.orchestrator
        .synchronize_native_policy(policy, fence.owner.device)
        .unwrap();
    h.orchestrator
        .prepare_native_effect(&request, fence.owner.device)
        .unwrap();
    settle_unknown(&mut h, &request, fence.owner.device);
    assert!(matches!(h.status(fence.run), RunStatus::Running { .. }));
    assert!(
        h.orchestrator
            .attempt_mut(fence.run)
            .unwrap()
            .bot_cleanup
            .is_none()
    );
    h.agent(
        fence.run,
        AgentEvent::TurnCompleted {
            turn_id: fence.turn.unwrap(),
        },
    );
    {
        let mut sessions = h.sessions.lock().unwrap();
        sessions
            .summaries
            .insert(fence.session, summary(ManagedState::Completed, None));
        sessions.lookups.insert(
            native,
            (
                host(1),
                CreationLookup::Known {
                    session: summary(ManagedState::Completed, None),
                },
            ),
        );
        sessions.observation.events.push(SequencedAgentEvent {
            session_id: fence.session,
            sequence: StreamSequence::new(50).unwrap(),
            event: AgentEvent::TurnCompleted {
                turn_id: fence.turn.unwrap(),
            },
        });
        sessions.owned_termination_refused = true;
    }
    assert!(
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), fence.owner.device, &delivery(&s, &fence))
            .is_err()
    );
    assert_eq!(
        h.status(fence.run),
        RunStatus::Interrupted {
            prior_attempt: PriorAttempt::Unknown
        }
    );
    assert!(
        h.orchestrator
            .attempt_mut(fence.run)
            .unwrap()
            .bot_cleanup
            .is_some()
    );
    cleanup_due(&mut h, fence.run);
    assert!(!h.orchestrator.bot_budget_tick(fence.run).unwrap());
    assert!(!h.attempt(fence.run).retired);
    assert!(!h.calls().contains(&Call::Terminate(fence.session)));
}

#[test]
fn owned_cleanup_resolves_lost_creation_ack_through_exact_key() {
    let mut h = bot_harness();
    let submission = submission(81);
    let run = submission.request.run;
    h.orchestrator
        .bot_submit(submission.clone(), submission.provenance.owner.device)
        .unwrap();
    boot::replay(&mut h.orchestrator, &submission);
    h.ledger.lock().unwrap().fail = Some("bind_session:id");
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&submission), submission.provenance.owner.device)
            .is_err()
    );
    let binding = h.attempt(run).provider_session.unwrap();
    assert!(binding.session.is_none());
    let known = summary(ManagedState::Running, None);
    let session = known.id;
    h.sessions.lock().unwrap().lookups.insert(
        binding.idempotency,
        (host(1), CreationLookup::Known { session: known }),
    );
    // Acceptance alone is not end proof. The exact lookup still says live.
    assert!(!h.orchestrator.bot_prove_end_owned(run).unwrap());
    assert!(h.calls().contains(&Call::TerminateOwned(session)));
    assert!(!h.calls().contains(&Call::Terminate(session)));
    let mut ended = summary(ManagedState::Completed, None);
    ended.process = ProcessState::Exited { code: Some(0) };
    h.sessions.lock().unwrap().lookups.insert(
        binding.idempotency,
        (host(1), CreationLookup::KnownEnded { session: ended }),
    );
    assert!(h.orchestrator.bot_prove_end_owned(run).unwrap());
}

#[test]
fn restart_repairs_retired_unsettled_bot_only_after_canonical_ack() {
    let (mut h, _, fence, _, _) = active();
    let native = h.attempt(fence.run).provider_session.unwrap().idempotency;
    h.schedule_store
        .advance(fence.run, fence.generation, AttemptPhase::Ended)
        .unwrap();
    h.schedule_store
        .retire(fence.run, fence.generation)
        .unwrap();
    let mut ended = summary(ManagedState::Completed, None);
    ended.process = ProcessState::Exited { code: Some(0) };
    let lookup = CreationLookup::KnownEnded { session: ended };
    let mut h = restarted(h, (host(1), lookup.clone()));
    h.sessions
        .lock()
        .unwrap()
        .lookups
        .insert(native, (host(1), lookup));
    {
        let mut document = h.documents.lock().unwrap();
        document.not_durable = true;
        document.hold_settle_ack = true;
    }
    assert!(h.orchestrator.restart().is_err());
    assert_eq!(
        h.schedule_store
            .bot_execution(fence.run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Executing
    );
    assert_eq!(
        h.status(fence.run),
        RunStatus::Interrupted {
            prior_attempt: PriorAttempt::Unknown
        }
    );
    h.documents.lock().unwrap().hold_settle_ack = false;
    let report = h.orchestrator.restart().unwrap();
    assert!(report.reconciled.contains(&fence.run));
    assert_eq!(
        h.schedule_store
            .bot_execution(fence.run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Settled
    );
    assert!(!h.calls().contains(&Call::Terminate(fence.session)));
}

#[test]
fn restart_unknown_attempt_records_interruption_and_never_uses_human_stop_authority() {
    for human in [false, true] {
        let (mut h, s, fence, _, _) = active();
        let native = h.attempt(fence.run).provider_session.unwrap().idempotency;
        h.schedule_store
            .advance(fence.run, fence.generation, AttemptPhase::Unknown)
            .unwrap();
        assert!(matches!(h.status(fence.run), RunStatus::Running { .. }));
        let mut observed = summary(
            if human {
                ManagedState::Running
            } else {
                ManagedState::Completed
            },
            human.then_some(fence.turn.unwrap()),
        );
        observed.handover = if human {
            HandoverState::Taken
        } else {
            HandoverState::Idle
        };
        if !human {
            observed.process = ProcessState::Exited { code: Some(0) };
        }
        let lookup = if human {
            CreationLookup::Known { session: observed }
        } else {
            CreationLookup::KnownEnded { session: observed }
        };
        let mut h = restarted(h, (host(1), lookup.clone()));
        {
            let mut sessions = h.sessions.lock().unwrap();
            sessions.lookups.insert(native, (host(1), lookup));
            sessions.owned_termination_refused = human;
        }
        let report = h.orchestrator.restart().unwrap();
        assert_eq!(
            h.status(fence.run),
            RunStatus::Interrupted {
                prior_attempt: PriorAttempt::Unknown
            }
        );
        assert_eq!(h.attempt(fence.run).retired, !human);
        assert!(!h.calls().contains(&Call::Terminate(fence.session)));
        if human {
            assert!(report.unresolved.contains(&fence.run));
            assert_eq!(h.orchestrator.claims().len(), 1);
            assert!(h.calls().contains(&Call::TerminateOwned(fence.session)));
            assert!(
                h.orchestrator
                    .bot_stop(&key(&s), fence.owner.device, Some(&fence))
                    .is_err()
            );
            assert!(h.calls().contains(&Call::Terminate(fence.session)));
            assert!(!h.attempt(fence.run).retired);
            let mut ended = summary(ManagedState::Completed, None);
            ended.process = ProcessState::Exited { code: Some(0) };
            h.sessions.lock().unwrap().lookups.insert(
                native,
                (host(1), CreationLookup::KnownEnded { session: ended }),
            );
            h.orchestrator
                .bot_stop(&key(&s), fence.owner.device, Some(&fence))
                .unwrap();
            assert!(h.attempt(fence.run).retired);
            assert_eq!(h.status(fence.run), RunStatus::Cancelled);
        } else {
            assert!(report.reconciled.contains(&fence.run));
            assert_eq!(h.orchestrator.claims().len(), 0);
        }
    }
}

#[test]
fn queued_stop_keeps_admission_until_canonical_cancellation_is_durable() {
    let mut h = bot_harness();
    let s = submission(80);
    h.orchestrator
        .bot_submit(s.clone(), s.provenance.owner.device)
        .unwrap();
    {
        let mut doc = h.documents.lock().unwrap();
        doc.not_durable = true;
        doc.hold_settle_ack = true;
    }
    assert!(
        h.orchestrator
            .bot_stop(&key(&s), s.provenance.owner.device, None)
            .is_err()
    );
    assert_eq!(
        h.schedule_store
            .bot_execution(s.request.run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Queued
    );
    assert!(
        h.orchestrator
            .ports_mut()
            .store
            .current_attempt(s.request.run)
            .unwrap()
            .is_none()
    );
    h.documents.lock().unwrap().hold_settle_ack = false;
    h.orchestrator
        .bot_stop(&key(&s), s.provenance.owner.device, None)
        .unwrap();
    assert_eq!(
        h.schedule_store
            .bot_execution(s.request.run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Settled
    );
    assert_eq!(h.status(s.request.run), RunStatus::Cancelled);
}

#[test]
fn stopped_bot_keeps_capacity_until_canonical_settle_is_acknowledged() {
    let (mut h, s, fence, _, _) = active();
    {
        let mut doc = h.documents.lock().unwrap();
        doc.not_durable = true;
        doc.hold_settle_ack = true;
    }
    assert!(
        h.orchestrator
            .bot_stop(&key(&s), fence.owner.device, Some(&fence))
            .is_err()
    );
    assert_eq!(h.status(fence.run), RunStatus::Cancelled);
    assert!(!h.attempt(fence.run).retired);
    assert_eq!(h.orchestrator.claims().len(), 1);
    assert_eq!(
        h.schedule_store
            .bot_execution(fence.run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Executing
    );
    h.documents.lock().unwrap().hold_settle_ack = false;
    h.orchestrator
        .bot_stop(&key(&s), fence.owner.device, Some(&fence))
        .unwrap();
    assert!(h.attempt(fence.run).retired);
    assert_eq!(h.orchestrator.claims().len(), 0);
    assert_eq!(
        h.schedule_store
            .bot_execution(fence.run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Settled
    );
}

#[test]
fn interrupted_delivery_cleanup_replays_after_late_durable_registration() {
    let (mut h, s, fence, policy, request) = active();
    let device = fence.owner.device;
    h.orchestrator
        .synchronize_native_policy(policy, device)
        .unwrap();
    let intent = h
        .orchestrator
        .prepare_native_effect(&request, device)
        .unwrap();
    h.agent(
        fence.run,
        AgentEvent::TurnCompleted {
            turn_id: fence.turn.unwrap(),
        },
    );
    {
        let mut script = h.sessions.lock().unwrap();
        script
            .summaries
            .insert(fence.session, summary(ManagedState::Completed, None));
        script.observation.events.push(SequencedAgentEvent {
            session_id: fence.session,
            sequence: StreamSequence::new(50).unwrap(),
            event: AgentEvent::TurnCompleted {
                turn_id: fence.turn.unwrap(),
            },
        });
        script.owned_termination_refused = true;
    }
    let target = h
        .orchestrator
        .check_native_effect_drain(&request.key, &NativeDrainReason::TerminalTurn, device)
        .unwrap()
        .unwrap();
    let drain = NativeEffectDrainReceipt {
        binding: target.binding,
        fenced_at: NativeEffectTimestamp::from_millis(100),
        drained_at: NativeEffectTimestamp::from_millis(90),
    };
    h.orchestrator
        .confirm_native_result(
            &ConfirmNativeResult {
                key: request.key.clone(),
                request_fingerprint: request.request_fingerprint,
                observation: NativeResultObservation::DrainedUnknown { receipt: drain },
            },
            device,
        )
        .unwrap();
    let delivery = BotDeliveryReceipt {
        run: fence.run,
        attempt: fence.attempt,
        generation: fence.generation,
        chat_id: s.provenance.chat_id,
        ai_attempt_id: s.provenance.ai_attempt_id.unwrap(),
        assistant_message_id: s.provenance.assistant_message_id.unwrap(),
        confirmed_at_ms: 1000,
    };
    {
        let mut document = h.documents.lock().unwrap();
        document.not_durable = true;
        document.hold_settle_ack = true;
    }
    assert!(
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), device, &delivery)
            .is_err()
    );
    assert!(!h.attempt(fence.run).retired);
    assert!(!h.calls().contains(&Call::TerminateOwned(fence.session)));
    h.documents.lock().unwrap().hold_settle_ack = false;
    assert!(
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), device, &delivery)
            .is_err()
    );
    assert_eq!(
        h.status(fence.run),
        RunStatus::Interrupted {
            prior_attempt: PriorAttempt::Unknown
        }
    );
    assert!(!h.attempt(fence.run).retired);
    // The canonical status is durable but its capacity cleanup was interrupted.
    // Exact result evidence may arrive before the owner retries that cleanup.
    let evidence = NativeResultReference {
        version_id: None,
        operation_id: request.key.operation_id,
        workspace: request.destination,
        document: intent.target,
        version: vec![1, 2],
        title: request.title.clone(),
        confirmed_at_ms: 900,
        kind: NativeResultKind::CreatedPage,
    };
    let result = h
        .orchestrator
        .confirm_native_result(
            &ConfirmNativeResult {
                key: request.key.clone(),
                request_fingerprint: request.request_fingerprint,
                observation: NativeResultObservation::Durable {
                    evidence: evidence.clone(),
                },
            },
            device,
        )
        .unwrap();
    assert_eq!(result.stage, NativeResultStage::Registered);
    assert!(!h.orchestrator.native_outcomes_unknown(fence.run).unwrap());
    h.sessions.lock().unwrap().owned_termination_refused = false;
    h.orchestrator
        .bot_acknowledge_delivery(&key(&s), device, &delivery)
        .unwrap();
    h.orchestrator
        .bot_acknowledge_delivery(&key(&s), device, &delivery)
        .unwrap();
    assert!(h.attempt(fence.run).retired);
    let header = h.orchestrator.header(fence.run).unwrap();
    assert_eq!(
        header.status,
        RunStatus::Interrupted {
            prior_attempt: PriorAttempt::Unknown
        }
    );
    assert_eq!(header.chat_delivery, Some(delivery));
    assert_eq!(header.native_results, vec![evidence]);
    let timeline = h
        .orchestrator
        .open(fence.run)
        .unwrap()
        .doc
        .document()
        .unwrap()
        .timeline_all();
    assert_eq!(timeline.iter().filter(|item|matches!(item,koru_delegate_document::TimelineItem::Step(entry) if matches!(entry.kind,StepKind::Interrupted{prior_attempt:PriorAttempt::Unknown}))).count(),1);
    assert!(!timeline.iter().any(|item|matches!(item,koru_delegate_document::TimelineItem::Step(entry) if entry.kind==StepKind::Completed)));
    assert!(!h.calls().contains(&Call::Terminate(fence.session)));
}

#[test]
fn drain_eligibility_requires_canonical_reason_and_exact_identity() {
    let (mut h, _, fence, mut policy, request) = active();
    let device = fence.owner.device;
    h.orchestrator
        .synchronize_native_policy(policy.clone(), device)
        .unwrap();
    h.orchestrator
        .ports_mut()
        .store
        .native_results()
        .unwrap()
        .prepare_native_result(&request)
        .unwrap();
    assert!(
        h.orchestrator
            .check_native_effect_drain(&request.key, &NativeDrainReason::GatewayStartup, device)
            .unwrap()
            .is_none()
    );
    h.orchestrator
        .prepare_native_effect(&request, device)
        .unwrap();
    for reason in [
        NativeDrainReason::OwnerStop,
        NativeDrainReason::RunRecovery,
        NativeDrainReason::TerminalTurn,
        NativeDrainReason::UnconfirmedOperation,
        NativeDrainReason::PolicyPending {
            key: policy.key.clone(),
        },
    ] {
        assert!(
            h.orchestrator
                .check_native_effect_drain(&request.key, &reason, device)
                .unwrap()
                .is_none()
        );
    }
    let target = h
        .orchestrator
        .check_native_effect_drain(&request.key, &NativeDrainReason::GatewayStartup, device)
        .unwrap()
        .unwrap();
    assert_eq!(target.owner, fence.owner);
    assert!(
        h.orchestrator
            .check_native_effect_drain(
                &request.key,
                &NativeDrainReason::GatewayStartup,
                DeviceId::new()
            )
            .is_err()
    );
    let mut wrong = request.key.clone();
    wrong.turn = AgentTurnId::mint();
    assert!(
        h.orchestrator
            .check_native_effect_drain(&wrong, &NativeDrainReason::GatewayStartup, device)
            .is_err()
    );
    policy.config_revision = 2;
    policy.policy_revision = 2;
    policy.key.request = BotRequestId::mint().into();
    assert_eq!(
        h.orchestrator
            .synchronize_native_policy(policy.clone(), device)
            .unwrap()
            .state,
        BotPolicyBarrierState::Pending
    );
    assert!(
        h.orchestrator
            .check_native_effect_drain(
                &request.key,
                &NativeDrainReason::PolicyPending {
                    key: policy.key.clone()
                },
                device
            )
            .unwrap()
            .is_some()
    );
    h.orchestrator
        .confirm_native_result(
            &ConfirmNativeResult {
                key: request.key.clone(),
                request_fingerprint: request.request_fingerprint,
                observation: NativeResultObservation::Unconfirmed,
            },
            device,
        )
        .unwrap();
    assert!(
        h.orchestrator
            .check_native_effect_drain(
                &request.key,
                &NativeDrainReason::UnconfirmedOperation,
                device
            )
            .unwrap()
            .is_some()
    );
    h.agent(
        fence.run,
        AgentEvent::TurnCompleted {
            turn_id: fence.turn.unwrap(),
        },
    );
    assert!(
        h.orchestrator
            .check_native_effect_drain(&request.key, &NativeDrainReason::TerminalTurn, device)
            .unwrap()
            .is_some()
    );
    assert!(
        h.orchestrator
            .bot_stop(
                &BotRunKey {
                    owner: fence.owner,
                    bot: fence.bot,
                    run: fence.run
                },
                device,
                Some(&fence)
            )
            .is_err()
    );
    for reason in [NativeDrainReason::OwnerStop, NativeDrainReason::RunRecovery] {
        assert!(
            h.orchestrator
                .check_native_effect_drain(&request.key, &reason, device)
                .unwrap()
                .is_some()
        );
    }
    let receipt = NativeEffectDrainReceipt {
        binding: target.binding,
        fenced_at: NativeEffectTimestamp::from_millis(50),
        drained_at: NativeEffectTimestamp::from_millis(40),
    };
    let result = h
        .orchestrator
        .confirm_native_result(
            &ConfirmNativeResult {
                key: request.key.clone(),
                request_fingerprint: request.request_fingerprint,
                observation: NativeResultObservation::DrainedUnknown { receipt },
            },
            device,
        )
        .unwrap();
    assert_eq!(result.stage, NativeResultStage::SettledUnknown);
    assert!(
        h.orchestrator
            .check_native_effect_drain(&request.key, &NativeDrainReason::GatewayStartup, device)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        h.orchestrator
            .reconcile_native_policy(&policy.key, device)
            .unwrap()
            .state,
        BotPolicyBarrierState::Effective
    );
    h.orchestrator
        .bot_stop(
            &BotRunKey {
                owner: fence.owner,
                bot: fence.bot,
                run: fence.run,
            },
            device,
            Some(&fence),
        )
        .unwrap();
    assert!(h.attempt(fence.run).retired);
    assert_eq!(h.status(fence.run), RunStatus::Cancelled);
}
