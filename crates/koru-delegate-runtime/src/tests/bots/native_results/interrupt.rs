// SPDX-License-Identifier: MPL-2.0
//! Automatic interruption records the existing uncertainty and keeps capacity
//! until owned end proof; it never borrows the person's stronger Stop.
use super::*;
use koru_session_protocol::HandoverState;

fn borrowed(h: &mut Harness, fence: &BotExecutionFence) -> IdempotencyKey {
    let native = h.attempt(fence.run).provider_session.unwrap().idempotency;
    let mut human = summary(ManagedState::Running, fence.turn);
    human.handover = HandoverState::Taken;
    let mut sessions = h.sessions.lock().unwrap();
    sessions
        .lookups
        .insert(native, (host(1), CreationLookup::Known { session: human }));
    sessions.owned_termination_refused = true;
    native
}
fn handed_back(h: &mut Harness, native: IdempotencyKey) {
    let mut ended = summary(ManagedState::Completed, None);
    ended.process = ProcessState::Exited { code: Some(0) };
    ended.handover = HandoverState::Returned;
    let mut sessions = h.sessions.lock().unwrap();
    sessions.owned_termination_refused = false;
    sessions.lookups.insert(
        native,
        (host(1), CreationLookup::KnownEnded { session: ended }),
    );
}
fn due(h: &mut Harness, run: RunId) {
    h.orchestrator
        .attempt_mut(run)
        .unwrap()
        .bot_cleanup
        .as_mut()
        .unwrap()
        .due = std::time::Instant::now();
}
fn interruptions(h: &mut Harness, run: RunId) -> usize {
    h.steps(run)
        .iter()
        .filter(|step| matches!(step, StepKind::Interrupted { .. }))
        .count()
}

#[test]
fn queued_interrupt_is_durable_idempotent_and_never_invents_an_attempt() {
    let mut h = bot_harness();
    let s = submission(85);
    let device = s.provenance.owner.device;
    h.orchestrator.bot_submit(s.clone(), device).unwrap();
    assert_eq!(h.status(s.request.run), RunStatus::Created);
    {
        let mut documents = h.documents.lock().unwrap();
        documents.not_durable = true;
        documents.hold_settle_ack = true;
    }
    // An unknown Run, or any fence before an execution identity exists, is
    // refused without touching the queued Run.
    let mut invented = s.clone();
    invented.request.run = RunId::mint();
    assert!(
        h.orchestrator
            .bot_interrupt(&key(&invented), device, None)
            .is_err()
    );
    let forged = BotExecutionFence {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        run: s.request.run,
        attempt: RunAttemptId::mint(),
        generation: AttemptGeneration::FIRST,
        session: SessionId::mint(),
        turn: None,
    };
    assert!(
        h.orchestrator
            .bot_interrupt(&key(&s), device, Some(&forged))
            .is_err()
    );
    assert_eq!(h.status(s.request.run), RunStatus::Created);
    assert!(
        h.orchestrator
            .bot_interrupt(&key(&s), device, None)
            .is_err()
    );
    assert_eq!(
        h.status(s.request.run),
        RunStatus::Interrupted {
            prior_attempt: PriorAttempt::Unknown
        }
    );
    // Without the canonical acknowledgment the queued capacity is not settled,
    // and the interrupted Run can no longer start.
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
            .bot_dispatch(&key(&s), device)
            .unwrap()
            .is_none()
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
        .bot_interrupt(&key(&s), device, None)
        .unwrap();
    h.orchestrator
        .bot_interrupt(&key(&s), device, None)
        .unwrap();
    assert_eq!(
        h.schedule_store
            .bot_execution(s.request.run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Settled
    );
    assert!(
        h.orchestrator
            .ports_mut()
            .store
            .current_attempt(s.request.run)
            .unwrap()
            .is_none()
    );
    assert_eq!(interruptions(&mut h, s.request.run), 1);
    assert!(
        !h.steps(s.request.run)
            .iter()
            .any(|step| matches!(step, StepKind::Started))
    );
    assert!(
        !h.calls()
            .iter()
            .any(|call| matches!(call, Call::Create { .. } | Call::Submit { .. }))
    );
}

#[test]
fn automatic_interrupt_during_takeover_retains_native_and_process_reservations_until_handback() {
    let (mut h, s, fence, policy, request) = active();
    let device = fence.owner.device;
    let native = borrowed(&mut h, &fence);
    let submits = count_submits(&h);
    h.orchestrator
        .synchronize_native_policy(policy, device)
        .unwrap();
    h.orchestrator
        .prepare_native_effect(&request, device)
        .unwrap();
    assert!(
        h.orchestrator
            .bot_interrupt(&key(&s), device, Some(&fence))
            .is_err()
    );
    assert!(
        h.orchestrator
            .bot_interrupt(&key(&s), device, Some(&fence))
            .is_err()
    );
    // A stale generation or a missing fence for a live session is refused; no
    // newer identity is substituted for the caller.
    let mut stale = fence.clone();
    stale.generation = AttemptGeneration::from_raw(fence.generation.get() + 1).unwrap();
    assert!(
        h.orchestrator
            .bot_interrupt(&key(&s), device, Some(&stale))
            .is_err()
    );
    assert!(
        h.orchestrator
            .bot_interrupt(&key(&s), device, None)
            .is_err()
    );
    assert_eq!(
        h.status(fence.run),
        RunStatus::Interrupted {
            prior_attempt: PriorAttempt::Unknown
        }
    );
    assert_eq!(interruptions(&mut h, fence.run), 1);
    due(&mut h, fence.run);
    assert!(!h.orchestrator.bot_budget_tick(fence.run).unwrap());
    assert!(!h.attempt(fence.run).retired);
    assert_eq!(h.orchestrator.claims().len(), 1);
    assert!(!h.calls().contains(&Call::Terminate(fence.session)));
    let target = h
        .orchestrator
        .check_native_effect_drain(&request.key, &NativeDrainReason::RunRecovery, device)
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
    // Effects are settled, but the person still holds the session: only the
    // lease-owned termination is attempted, and it is refused.
    due(&mut h, fence.run);
    assert!(!h.orchestrator.bot_budget_tick(fence.run).unwrap());
    assert!(h.calls().contains(&Call::TerminateOwned(fence.session)));
    assert!(!h.attempt(fence.run).retired);
    assert_eq!(h.orchestrator.claims().len(), 1);
    assert!(
        h.orchestrator
            .bot_interrupt(&key(&s), device, Some(&fence))
            .is_err()
    );
    handed_back(&mut h, native);
    due(&mut h, fence.run);
    assert!(h.orchestrator.bot_budget_tick(fence.run).unwrap());
    h.orchestrator
        .bot_interrupt(&key(&s), device, Some(&fence))
        .unwrap();
    assert!(h.attempt(fence.run).retired);
    assert_eq!(h.orchestrator.claims().len(), 0);
    assert_eq!(
        h.status(fence.run),
        RunStatus::Interrupted {
            prior_attempt: PriorAttempt::Unknown
        }
    );
    assert_eq!(interruptions(&mut h, fence.run), 1);
    assert_eq!(count_submits(&h), submits);
    assert!(!h.calls().contains(&Call::Terminate(fence.session)));
}

#[test]
fn explicit_stop_stays_strong_after_automatic_interruption_during_takeover() {
    let (mut h, s, fence, _, _) = active();
    let device = fence.owner.device;
    let native = borrowed(&mut h, &fence);
    assert!(
        h.orchestrator
            .bot_interrupt(&key(&s), device, Some(&fence))
            .is_err()
    );
    assert!(!h.calls().contains(&Call::Terminate(fence.session)));
    // The person's Stop still reaches the session without the lease.
    assert!(
        h.orchestrator
            .bot_stop(&key(&s), device, Some(&fence))
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
        .bot_stop(&key(&s), device, Some(&fence))
        .unwrap();
    assert_eq!(h.status(fence.run), RunStatus::Cancelled);
    assert!(h.attempt(fence.run).retired);
    assert_eq!(h.orchestrator.claims().len(), 0);
    // A late automatic interruption cannot replace the explicit outcome.
    h.orchestrator
        .bot_interrupt(&key(&s), device, Some(&fence))
        .unwrap();
    assert_eq!(h.status(fence.run), RunStatus::Cancelled);
    assert_eq!(interruptions(&mut h, fence.run), 1);
}

#[test]
fn an_ended_interruption_is_absorbing_and_cleans_up_without_a_second_step() {
    let (mut h, s, fence, _, _) = active();
    let device = fence.owner.device;
    let native = borrowed(&mut h, &fence);
    h.orchestrator
        .apply(
            fence.run,
            &RunEvent::Interrupt {
                prior_attempt: PriorAttempt::Ended,
            },
        )
        .unwrap();
    let before = interruptions(&mut h, fence.run);
    assert!(
        h.orchestrator
            .bot_interrupt(&key(&s), device, Some(&fence))
            .is_err()
    );
    assert_eq!(
        h.status(fence.run),
        RunStatus::Interrupted {
            prior_attempt: PriorAttempt::Ended
        }
    );
    assert_eq!(interruptions(&mut h, fence.run), before);
    due(&mut h, fence.run);
    assert!(!h.orchestrator.bot_budget_tick(fence.run).unwrap());
    assert!(!h.attempt(fence.run).retired);
    handed_back(&mut h, native);
    due(&mut h, fence.run);
    assert!(h.orchestrator.bot_budget_tick(fence.run).unwrap());
    h.orchestrator
        .bot_interrupt(&key(&s), device, Some(&fence))
        .unwrap();
    assert_eq!(
        h.status(fence.run),
        RunStatus::Interrupted {
            prior_attempt: PriorAttempt::Ended
        }
    );
    assert_eq!(interruptions(&mut h, fence.run), before);
    assert!(!h.calls().contains(&Call::Terminate(fence.session)));
}

#[test]
fn ordinary_created_interruption_still_refuses_through_the_shared_reducer() {
    let mut h = Harness::without_folders();
    let run = run_id(86);
    h.orchestrator
        .create(request(run, h.scope()), workspace())
        .unwrap();
    assert!(h.orchestrator.header(run).unwrap().bot.is_none());
    assert_eq!(h.status(run), RunStatus::Created);
    assert!(
        h.orchestrator
            .apply(
                run,
                &RunEvent::Interrupt {
                    prior_attempt: PriorAttempt::Unknown,
                },
            )
            .is_err()
    );
    assert_eq!(h.status(run), RunStatus::Created);
}
