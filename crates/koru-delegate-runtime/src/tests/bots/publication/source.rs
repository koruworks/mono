// SPDX-License-Identifier: MPL-2.0

//! Decision Source production over real in-memory SQL and document staging.
//! No Store, provider, session host or disk is used.

use super::*;

fn source_usage(
    turn: AgentTurnId,
    sequence: u64,
    usage: koru_session_protocol::AgentUsage,
) -> SessionUpdate {
    SessionUpdate::Agent(SequencedAgentEvent {
        session_id: session(1),
        sequence: StreamSequence::new(sequence).unwrap(),
        event: AgentEvent::Usage {
            turn_id: turn,
            usage,
        },
    })
}

fn tally(h: &Harness, run: RunId, turn: AgentTurnId) -> u32 {
    h.orchestrator.runs[&run]
        .attempt
        .as_ref()
        .unwrap()
        .turns
        .get(&turn)
        .map_or(0, |tally| tally.tool_calls)
}

#[test]
fn decision_source_usage_cold_lookup_then_applied_envelope_dedupes_only_exact_report() {
    use crate::orchestrator::publication::source::SourceObligation;
    use koru_session_protocol::{AgentUsage, CachedInputRelationship};
    let (mut h, _, fence) = waiting_with_memory(true);
    apply_next(&mut h, fence.run);
    let run = fence.run;
    let turn = fence.turn.unwrap();
    let epoch = Uuid::new_v4();
    h.orchestrator.register_source_producer(run, epoch);
    let report = AgentUsage {
        input_tokens: Some(10),
        output_tokens: Some(4),
        cached_input_tokens: Some(2),
        cached_input: CachedInputRelationship::Included,
        model: None,
    };
    let frontiers = h.orchestrator.runs[&run].source_frontiers.clone();
    let before = latest(&h, &fence);
    // A cold key consumes nothing: one exact lookup is owed, nothing written.
    assert_eq!(
        h.orchestrator
            .handle_from_producer(run, epoch, source_usage(turn, 8, report.clone()))
            .unwrap(),
        crate::SourceProgress::Paused
    );
    assert!(matches!(
        h.orchestrator.runs[&run].source_obligation,
        Some(SourceObligation::Usage(_))
    ));
    assert_eq!(h.orchestrator.runs[&run].source_frontiers, frontiers);
    assert_eq!(latest(&h, &fence), before);
    // The same physical ticket runs the read-only observation.
    let start = h.orchestrator.next_run_publication(run).unwrap().unwrap();
    let crate::RunPublicationDocument::Attached(handle) = start.document else {
        panic!("warm")
    };
    let crate::RunPublicationOperation::ObserveSource(observation) = start.operation else {
        panic!("observation")
    };
    assert_eq!(observation.projection, before.identity);
    let observed = observation.observe(handle.as_ref());
    assert!(matches!(
        observed.as_ref().unwrap().usage,
        crate::RecoveredDecisionUsage::Absent
    ));
    assert!(
        h.orchestrator
            .accept_source_observation(start.ticket, handle, observed)
            .is_ok()
    );
    assert!(h.orchestrator.runs[&run].source_obligation.is_none());
    assert!(h.orchestrator.run_publication_pending(run));
    assert_eq!(latest(&h, &fence), before);
    // The producer reconnects from the unchanged frontier and replays it.
    let epoch = Uuid::new_v4();
    h.orchestrator.register_source_producer(run, epoch);
    assert_eq!(h.orchestrator.runs[&run].source_frontiers, frontiers);
    assert_eq!(
        h.orchestrator
            .handle_from_producer(run, epoch, source_usage(turn, 8, report.clone()))
            .unwrap(),
        crate::SourceProgress::Paused
    );
    assert!(matches!(
        latest(&h, &fence).state,
        DecisionProjectionState::Pending
    ));
    assert!(
        !h.orchestrator.runs[&run]
            .usage
            .contains_key(&(fence.attempt, turn)),
        "SQL acceptance alone does not advance the retry identity"
    );
    apply_next(&mut h, run);
    let identity = h.orchestrator.runs[&run].usage[&(fence.attempt, turn)].clone();
    assert_eq!(identity.revision, AmendmentRevision::FIRST);
    // The exact complete report again is a duplicate of the applied envelope.
    let applied = latest(&h, &fence);
    assert_eq!(
        h.orchestrator
            .handle_from_producer(run, epoch, source_usage(turn, 9, report.clone()))
            .unwrap(),
        crate::SourceProgress::Continue
    );
    assert_eq!(latest(&h, &fence), applied);
    // Equal totals with another declared relationship is the next checked
    // revision under the same identity and a new event.
    let mut corrected = report.clone();
    corrected.cached_input = CachedInputRelationship::Separate;
    assert_eq!(
        h.orchestrator
            .handle_from_producer(run, epoch, source_usage(turn, 10, corrected))
            .unwrap(),
        crate::SourceProgress::Paused
    );
    let RunProjectionTargetV1::Source { bookkeeping, .. } = latest(&h, &fence).target else {
        panic!("source")
    };
    let amended = bookkeeping.usage.unwrap();
    assert_eq!(amended.id, identity.id);
    assert_eq!(amended.revision, AmendmentRevision(1));
    assert_ne!(amended.event, identity.event);
}

#[test]
fn decision_source_delayed_old_applied_never_advances_successor_cursor() {
    let (mut h, _, fence) = waiting_with_memory(true);
    apply_next(&mut h, fence.run);
    let run = fence.run;
    let turn = fence.turn.unwrap();
    let epoch = Uuid::new_v4();
    h.orchestrator.register_source_producer(run, epoch);
    h.orchestrator
        .handle_from_producer(run, epoch, source_tool(turn, 80))
        .unwrap();
    // A successor consumer becomes current before the old target applies.
    h.orchestrator
        .runs
        .get_mut(&run)
        .unwrap()
        .attempt
        .as_mut()
        .unwrap()
        .session = Some(SessionId::mint());
    h.orchestrator.register_source_producer(run, Uuid::new_v4());
    assert_eq!(h.orchestrator.runs[&run].source_frontiers.agent_after, None);
    // The old exact retained target still applies on its own bytes.
    apply_next(&mut h, run);
    assert!(matches!(
        latest(&h, &fence).state,
        DecisionProjectionState::Applied { .. }
    ));
    let state = &h.orchestrator.runs[&run];
    assert!(state.decision_source.is_none());
    assert_eq!(
        state.source_frontiers.agent_after, None,
        "an old completion cannot advance the successor frontier"
    );
    assert_eq!(
        tally(&h, run, turn),
        0,
        "old memory is not installed into a successor"
    );
}

#[test]
fn decision_source_applied_installs_before_carried_control_replaces_marker() {
    let (mut h, submission, fence) = waiting_with_memory(true);
    apply_next(&mut h, fence.run);
    let run = fence.run;
    let turn = fence.turn.unwrap();
    let epoch = Uuid::new_v4();
    h.orchestrator.register_source_producer(run, epoch);
    h.orchestrator
        .handle_from_producer(run, epoch, source_tool(turn, 8))
        .unwrap();
    let source = latest(&h, &fence).identity.transition;
    // Stop journals a later Control behind the still-pending Source.
    let _ = h
        .orchestrator
        .bot_stop(&key(&submission), fence.owner.device, Some(&fence));
    assert_ne!(latest(&h, &fence).identity.transition, source);
    assert_eq!(
        latest(&h, &fence).identity.kind,
        DecisionProjectionKind::Control
    );
    // The Source prepares and applies first; its SQL ack commits, but the
    // install read is lost.
    let start = h.orchestrator.next_run_publication(run).unwrap().unwrap();
    let crate::RunPublicationDocument::Attached(mut handle) = start.document else {
        panic!("warm")
    };
    let crate::RunPublicationOperation::Prepare(intent) = start.operation else {
        panic!("prepare")
    };
    assert_eq!(intent.identity.transition, source);
    let candidate = handle.prepare_decision_projection(&intent).unwrap();
    let prepared = h
        .orchestrator
        .record_run_preparation(start.ticket, &candidate)
        .unwrap();
    let proof = handle.apply_prepared_projection(&prepared).unwrap();
    assert!(
        h.orchestrator
            .return_run_publication(start.ticket, handle)
            .is_ok()
    );
    h.ledger.lock().unwrap().arm_failure_after =
        Some(("decision_ack_after", "decision_projection_read"));
    assert!(h.orchestrator.acknowledge_run_projection(&proof).is_err());
    assert!(h.orchestrator.runs[&run].decision_source.is_some());
    assert_eq!(tally(&h, run, turn), 0);
    // Until the Applied Source installs, the carried Control cannot start.
    h.ledger.lock().unwrap().fail = Some("decision_projection_read");
    let Err(refused) = h.orchestrator.next_run_publication(run) else {
        panic!("a carried Control cannot overtake an uninstalled Source")
    };
    assert!(refused.publication_pending());
    h.orchestrator.retry_decision_source(run).unwrap();
    assert!(h.orchestrator.runs[&run].decision_source.is_none());
    assert_eq!(
        tally(&h, run, turn),
        1,
        "staged Source memory drained first"
    );
    let next = h.orchestrator.next_run_publication(run).unwrap().unwrap();
    assert!(matches!(&next.operation,
        crate::RunPublicationOperation::Prepare(intent)
            if intent.identity.kind == DecisionProjectionKind::Control));
    h.orchestrator.cancel_unstarted_publication(next);
}

#[test]
fn decision_wait_settlement_needs_private_return_proof_and_a_settled_decision() {
    let (mut h, _, fence) = waiting_with_memory(true);
    let run = fence.run;
    // Nothing has physically returned yet: there is no proof to offer C.
    assert!(
        h.orchestrator
            .settle_publication_wait(run)
            .unwrap()
            .is_none()
    );
    apply_next(&mut h, run);
    // The exact durable marker returned, but the decision is still open.
    h.ledger.lock().unwrap().ops.clear();
    assert!(
        h.orchestrator
            .settle_publication_wait(run)
            .unwrap()
            .is_none()
    );
    assert!(
        !h.ledger
            .lock()
            .unwrap()
            .ops
            .contains(&"decision_wait_settle")
    );
    assert!(h.orchestrator.run_publication_pending(run));
    // A staged Source retry also withholds the proof.
    let epoch = Uuid::new_v4();
    h.orchestrator.register_source_producer(run, epoch);
    h.orchestrator
        .handle_from_producer(run, epoch, source_tool(fence.turn.unwrap(), 8))
        .unwrap();
    assert!(h.orchestrator.runs[&run].decision_source.is_some());
    assert!(
        h.orchestrator
            .settle_publication_wait(run)
            .unwrap()
            .is_none()
    );
    assert!(
        !h.ledger
            .lock()
            .unwrap()
            .ops
            .contains(&"decision_wait_settle")
    );
}

#[test]
fn decision_projection_selection_is_a_bounded_rotating_keyset_over_open_rows() {
    let (mut h, _, fence) = waiting_with_memory(true);
    let run = fence.run;
    // The open decision Control is found through the partial indexes.
    assert_eq!(h.orchestrator.publication_runs(None).unwrap(), vec![run]);
    // A cursor at the Run wraps once to reach it; nothing is skipped forever.
    assert_eq!(
        h.orchestrator.publication_runs(Some(run)).unwrap(),
        vec![run]
    );
    apply_next(&mut h, run);
    assert!(h.orchestrator.publication_runs(None).unwrap().is_empty());
    assert!(
        h.orchestrator
            .publication_runs(Some(run))
            .unwrap()
            .is_empty()
    );
    // The reader refuses an unbounded page rather than scanning history.
    assert!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .decision_projection_runs(None, 17)
            .is_err()
    );
}

#[test]
fn decision_cleanup_confirms_retired_consumer_then_settles_only_the_wait() {
    let (mut h, submission, fence) = waiting_with_memory(true);
    let run = fence.run;
    apply_next(&mut h, run);
    let _ = h
        .orchestrator
        .bot_stop(&key(&submission), fence.owner.device, Some(&fence));
    assert!(!h.attempt(run).retired, "a request alone retires nothing");
    // The cleanup tick observes the exact consumer, retires its attempt row
    // and records the complete positive evidence once.
    let _ = h.orchestrator.bot_budget_tick(run);
    assert!(h.attempt(run).retired);
    let execution = h
        .schedule_store
        .inner
        .lock()
        .unwrap()
        .decision_wait_execution(fence.owner, run)
        .unwrap()
        .unwrap();
    assert!(matches!(
        execution.wait.cleanup,
        Some(ConsumerCleanup::Confirmed { .. })
    ));
    assert_eq!(
        h.schedule_store.bot_execution(run).unwrap().unwrap().state,
        BotAdmissionState::Executing,
        "physical cleanup never settles the admission"
    );
    // A later tick neither retires again nor mints a second final record.
    let records = latest(&h, &fence).identity.sequence;
    let _ = h.orchestrator.bot_budget_tick(run);
    assert_eq!(latest(&h, &fence).identity.sequence, records);
    // The owed Controls publish in order; then the wait settles with D's
    // private return proof, and the fence ends without resuming a stream.
    for _ in 0..3 {
        let Some(start) = h.orchestrator.next_run_publication(run).unwrap() else {
            break;
        };
        h.orchestrator.cancel_unstarted_publication(start);
        apply_next(&mut h, run);
    }
    assert!(
        h.orchestrator
            .settle_publication_wait(run)
            .unwrap()
            .is_none()
    );
    assert!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .decision_wait_execution(fence.owner, run)
            .unwrap()
            .is_none(),
        "the runtime wait is settled"
    );
    assert!(!h.orchestrator.run_publication_pending(run));
    assert_eq!(
        h.schedule_store.bot_execution(run).unwrap().unwrap().state,
        BotAdmissionState::Executing,
        "wait settlement never settles the admission"
    );
}

fn publish_owed(h: &mut Harness, run: RunId) {
    for _ in 0..4 {
        let Some(start) = h.orchestrator.next_run_publication(run).unwrap() else {
            return;
        };
        h.orchestrator.cancel_unstarted_publication(start);
        apply_next(h, run);
    }
    panic!("owed publications did not drain within the bound");
}

/// Answer the open native approval with Allow and record its exact
/// acknowledgment through the C writers; the decision is then Settled.
fn settle_native(
    h: &mut Harness,
    fence: &BotExecutionFence,
    proof: &FreshBotPolicyProof,
    boot: DelegateBootId,
) -> DecisionId {
    let run = fence.run;
    let decision = latest(h, fence).identity.decision.unwrap().decision;
    let record = h
        .schedule_store
        .inner
        .lock()
        .unwrap()
        .bot_decision(fence.owner, run, decision)
        .unwrap()
        .unwrap();
    let DecisionBindingV1::NativeApproval(binding) = &record.receipt.binding else {
        panic!("native approval")
    };
    let input = AnswerDecision {
        owner: fence.owner,
        run,
        decision,
        expected_revision: record.receipt.state.revision,
        binding_digest: record.receipt.binding_digest,
        request: DecisionAnswerId::mint(),
        human: binding.human_profile,
        choice: DecisionAnswerChoice::Native {
            offered: binding.choices.as_slice()[0].clone(),
        },
        authority_digest: DecisionDigest::from_bytes([9; 32]),
    };
    let answered = h
        .schedule_store
        .inner
        .lock()
        .unwrap()
        .answer_bot_decision(&input, DecisionTransitionId::mint(), proof, boot)
        .unwrap();
    let Some(DecisionNativeResponse::Prepared { key }) = answered.native_response else {
        panic!("prepared native response")
    };
    let settled = h
        .schedule_store
        .inner
        .lock()
        .unwrap()
        .record_decision_native_answer(
            fence.owner,
            run,
            DecisionTransitionId::mint(),
            &DecisionNativeResponse::Acknowledged {
                key: key.clone(),
                acknowledgment: key,
                observed_at_ms: h.schedule_clock.load(Ordering::Acquire),
            },
        )
        .unwrap();
    assert_eq!(settled.receipt.state.phase, DecisionPhase::Settled);
    decision
}

#[test]
fn decision_settled_native_answer_still_stops_on_the_execution_budget() {
    let (mut h, _, fence, proof, boot) = waiting_with(true, true, None);
    let run = fence.run;
    apply_next(&mut h, run);
    let decision = settle_native(&mut h, &fence, &proof, boot);
    publish_owed(&mut h, run);
    // The native Allow continues execution; its publication fence is still
    // held. The whole-Run execution budget then expires.
    h.schedule_clock.fetch_add(
        koru_delegate_sqlite::BOT_ACTIVE_BUDGET_MS as i64 + 1_000,
        Ordering::AcqRel,
    );
    let creation = h.attempt(run).provider_session.unwrap().idempotency;
    let _ = h.orchestrator.bot_budget_tick(run);
    // A settled answer does not make the Run unstoppable: the exact Canceled
    // request is retained against the settled disposition and the current
    // consumer is retired physically.
    let latest = latest(&h, &fence);
    assert!(matches!(
        latest.target,
        RunProjectionTargetV1::Control {
            action: DecisionControlAction::CleanupRequested {
                cause: DecisionClosureReason::Canceled,
                ..
            },
            disposition: Some(DecisionDisposition::Settled { .. }),
            ..
        }
    ));
    assert!(
        h.sessions
            .lock()
            .unwrap()
            .calls
            .contains(&Call::Retire(creation))
    );
    assert_eq!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .bot_decision(fence.owner, run, decision)
            .unwrap()
            .unwrap()
            .receipt
            .state
            .phase,
        DecisionPhase::Settled,
        "the settled answer is preserved, never reopened"
    );
    assert_eq!(
        h.schedule_store.bot_execution(run).unwrap().unwrap().state,
        BotAdmissionState::Executing,
        "the budget Stop retains admission until cleanup and publication"
    );
}

fn pending_code(error: RunError) -> bool {
    matches!(error, RunError::Port(error) if error.code == "bot_delivery_pending")
}

#[test]
fn decision_delivery_while_exclusive_records_receipt_then_publishes_owed_completion() {
    let (mut h, s, fence, proof, boot) = waiting_with(true, true, None);
    let run = fence.run;
    let turn = fence.turn.unwrap();
    apply_next(&mut h, run);
    settle_native(&mut h, &fence, &proof, boot);
    publish_owed(&mut h, run);
    let receipt = BotDeliveryReceipt {
        run,
        attempt: fence.attempt,
        generation: fence.generation,
        chat_id: s.provenance.chat_id,
        ai_attempt_id: s.provenance.ai_attempt_id.unwrap(),
        assistant_message_id: s.provenance.assistant_message_id.unwrap(),
        confirmed_at_ms: 1000,
    };
    // Before the native turn is proven complete nothing is written.
    let before = latest(&h, &fence);
    assert!(pending_code(
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
            .unwrap_err()
    ));
    assert_eq!(latest(&h, &fence), before);
    // The native turn completes through the decision Source producer.
    let epoch = Uuid::new_v4();
    h.orchestrator.register_source_producer(run, epoch);
    h.orchestrator
        .handle_from_producer(
            run,
            epoch,
            SessionUpdate::Agent(SequencedAgentEvent {
                session_id: session(1),
                sequence: StreamSequence::new(8).unwrap(),
                event: AgentEvent::TurnCompleted { turn_id: turn },
            }),
        )
        .unwrap();
    publish_owed(&mut h, run);
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
    // The receipt and its owed header publication commit together; delivery
    // is not durable until that Control is acknowledged.
    assert!(pending_code(
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
            .unwrap_err()
    ));
    let control = latest(&h, &fence);
    assert!(matches!(control.state, DecisionProjectionState::Pending));
    assert!(matches!(
        &control.target,
        RunProjectionTargetV1::Control {
            action: DecisionControlAction::Delivery {
                outcome: DecisionDeliveryOutcome::Completed
            },
            delivery: Some(delivery),
            ..
        } if delivery.receipt == receipt
    ));
    // An exact retry replays the same transition; no second Control.
    assert!(pending_code(
        h.orchestrator
            .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
            .unwrap_err()
    ));
    assert_eq!(latest(&h, &fence), control);
    // The owed projection publishes like any other; the header now carries
    // the exact receipt and the Completed status.
    publish_owed(&mut h, run);
    assert!(matches!(
        latest(&h, &fence).state,
        DecisionProjectionState::Applied { .. }
    ));
    let header = h.orchestrator.runs[&run]
        .doc
        .document()
        .unwrap()
        .header()
        .unwrap();
    assert_eq!(header.status, RunStatus::Completed);
    assert_eq!(header.chat_delivery, Some(receipt));
}

#[test]
fn decision_cleanup_with_a_drained_effect_completes_but_a_held_lease_keeps_occupancy() {
    for drained in [true, false] {
        let (mut h, submission, fence, _, _) = waiting_with(true, false, Some(drained));
        let run = fence.run;
        apply_next(&mut h, run);
        let _ = h
            .orchestrator
            .bot_stop(&key(&submission), fence.owner.device, Some(&fence));
        let _ = h.orchestrator.bot_budget_tick(run);
        let execution = h
            .schedule_store
            .inner
            .lock()
            .unwrap()
            .decision_wait_execution(fence.owner, run)
            .unwrap()
            .unwrap();
        if drained {
            // The complete drain from the C reader names the drained effect.
            let Some(ConsumerCleanup::Confirmed { native_drain, .. }) = execution.wait.cleanup
            else {
                panic!("final cleanup recorded")
            };
            assert_eq!(native_drain.effects.as_slice().len(), 1);
            assert!(h.attempt(run).retired);
        } else {
            // A held lease may still commit: nothing is recorded or retired.
            assert!(execution.wait.cleanup.is_none());
            assert!(!h.attempt(run).retired);
        }
        assert_eq!(
            h.schedule_store.bot_execution(run).unwrap().unwrap().state,
            BotAdmissionState::Executing
        );
    }
}

/// Drive the real ledger from the applied waiting decision to a recorded
/// successor through C's shared builders: enrollment, origin stop, AllowOnce,
/// reservation, binding and, when `turn` is given, the acknowledged input.
/// Mirrors the successor into working memory as each builder documents.
fn successor(
    h: &mut Harness,
    fence: &BotExecutionFence,
    proof: &FreshBotPolicyProof,
    boot: DelegateBootId,
    turn: Option<AgentTurnId>,
) -> (DecisionRecord, SessionId, RunAttemptId, AttemptGeneration) {
    use koru_delegate_sqlite::test_support as build;
    let run = fence.run;
    let decision = latest(h, fence).identity.decision.unwrap().decision;
    let host = h.sessions.lock().unwrap().host;
    let session = SessionId::mint();
    let now = h.schedule_clock.load(Ordering::Acquire);
    let record = {
        let mut db = h.schedule_store.inner.lock().unwrap();
        let record = db
            .bot_decision(fence.owner, run, decision)
            .unwrap()
            .unwrap();
        let record = build::request_enrollment(&mut db, &record).unwrap();
        let record = build::record_enrollment(&mut db, &record).unwrap();
        let record = build::stop_origin(&mut db, &record, now).unwrap();
        let record = build::bind_successor(&mut db, &record, host, session, proof, boot).unwrap();
        match turn {
            Some(turn) => {
                build::acknowledge_successor(&mut db, &record, turn)
                    .unwrap()
                    .0
            }
            None => record,
        }
    };
    let (attempt, generation, _, _) = record.resume.as_ref().unwrap().consumer_identity();
    let state = h.orchestrator.runs.get_mut(&run).unwrap();
    let memory = state.attempt.as_mut().unwrap();
    memory.id = attempt;
    memory.generation = generation;
    memory.session = Some(session);
    memory.orchestrator_turn = turn;
    memory.turns.clear();
    memory.pending_approvals.clear();
    memory.bot_terminal_turn = None;
    memory.holds_lease = true;
    memory.release_owed = false;
    (record, session, attempt, generation)
}

fn successor_receipt(
    s: &BotSubmission,
    run: RunId,
    attempt: RunAttemptId,
    generation: AttemptGeneration,
) -> BotDeliveryReceipt {
    BotDeliveryReceipt {
        run,
        attempt,
        generation,
        chat_id: s.provenance.chat_id,
        ai_attempt_id: s.provenance.ai_attempt_id.unwrap(),
        assistant_message_id: s.provenance.assistant_message_id.unwrap(),
        confirmed_at_ms: 1000,
    }
}

#[test]
fn decision_consumed_edit_cleanup_completes_but_an_unresolved_effect_keeps_occupancy() {
    use koru_delegate_sqlite::test_support as build;
    for resolved in [true, false] {
        let (mut h, _, fence, proof, boot) = waiting_with(true, false, None);
        let run = fence.run;
        apply_next(&mut h, run);
        let turn = AgentTurnId::mint();
        let (record, _, attempt, generation) = successor(&mut h, &fence, &proof, boot, Some(turn));
        let consumer = BotExecutionFence {
            attempt,
            generation,
            session: h.orchestrator.runs[&run]
                .attempt
                .as_ref()
                .unwrap()
                .session
                .unwrap(),
            turn: Some(turn),
            ..fence
        };
        {
            let mut db = h.schedule_store.inner.lock().unwrap();
            if resolved {
                build::consumed_for_cleanup(&mut db, &record, &consumer, &proof, boot).unwrap();
            } else {
                // Consumed but never observed: the lease stays held.
                let DecisionBindingV1::KoruEdit(edit) = &record.receipt.binding else {
                    panic!("edit")
                };
                let request = build::consume_request(&record, &consumer).unwrap();
                db.consume_decision_edit(
                    &build::expected(&record),
                    DecisionTransitionId::mint(),
                    &request,
                    edit.payload.as_slice(),
                    &consumer,
                    &proof,
                    boot,
                )
                .unwrap();
                let ticket = db
                    .decision_wait_execution(fence.owner, run)
                    .unwrap()
                    .unwrap()
                    .expected;
                db.request_decision_cleanup(
                    &ticket,
                    DecisionTransitionId::mint(),
                    DecisionClosureReason::Canceled,
                )
                .unwrap();
            }
        }
        // The runtime adopts the retained owner and runs its cleanup tick.
        h.orchestrator
            .request_publication_cleanup(run, DecisionClosureReason::Canceled)
            .unwrap();
        let _ = h.orchestrator.bot_budget_tick(run);
        let execution = h
            .schedule_store
            .inner
            .lock()
            .unwrap()
            .decision_wait_execution(fence.owner, run)
            .unwrap()
            .unwrap();
        if resolved {
            let Some(ConsumerCleanup::Confirmed {
                retained_effect_outcome,
                native_drain,
                ..
            }) = execution.wait.cleanup
            else {
                panic!("final cleanup recorded")
            };
            assert!(matches!(
                retained_effect_outcome,
                Some(DecisionRetainedEffect::NotApplied { .. })
            ));
            assert!(native_drain.effects.as_slice().is_empty());
            assert!(h.attempt(run).retired, "the successor is retired");
        } else {
            assert!(execution.wait.cleanup.is_none());
            assert!(!h.attempt(run).retired);
        }
        assert_eq!(
            h.schedule_store.bot_execution(run).unwrap().unwrap().state,
            BotAdmissionState::Executing
        );
    }
}

#[test]
fn decision_successor_delivery_is_accepted_after_the_acknowledged_continuation() {
    use koru_delegate_sqlite::test_support as build;
    let (mut h, s, fence, proof, boot) = waiting_with(true, false, None);
    let run = fence.run;
    apply_next(&mut h, run);
    let turn = AgentTurnId::mint();
    let (record, session, attempt, generation) =
        successor(&mut h, &fence, &proof, boot, Some(turn));
    let consumer = BotExecutionFence {
        attempt,
        generation,
        session,
        turn: Some(turn),
        ..fence
    };
    // The one-use edit resolves NotApplied and settles logically, so the
    // decision is Settled with no cleanup owed.
    {
        let mut db = h.schedule_store.inner.lock().unwrap();
        let DecisionBindingV1::KoruEdit(edit) = &record.receipt.binding else {
            panic!("edit")
        };
        let request = build::consume_request(&record, &consumer).unwrap();
        let consumed = db
            .consume_decision_edit(
                &build::expected(&record),
                DecisionTransitionId::mint(),
                &request,
                edit.payload.as_slice(),
                &consumer,
                &proof,
                boot,
            )
            .unwrap();
        drop(consumed);
        db.confirm_native_result(&ConfirmNativeResult {
            key: request.key.clone(),
            request_fingerprint: request.request_fingerprint,
            observation: NativeResultObservation::NotApplied {
                code: NativeResultRefusal::Conflict,
            },
        })
        .unwrap();
        let current = db
            .bot_decision(fence.owner, run, record.receipt.decision)
            .unwrap()
            .unwrap();
        let effect = db
            .decision_retained_effect(fence.owner, run, record.receipt.decision)
            .unwrap()
            .unwrap();
        let settled = db
            .settle_decision_effect(
                &build::expected(&current),
                DecisionTransitionId::mint(),
                &effect,
            )
            .unwrap();
        assert_eq!(settled.receipt.state.phase, DecisionPhase::Settled);
    }
    // The owed Controls of the chain publish before any delivery is recorded.
    publish_owed(&mut h, run);
    // The successor turn completed natively.
    {
        let memory = h
            .orchestrator
            .runs
            .get_mut(&run)
            .unwrap()
            .attempt
            .as_mut()
            .unwrap();
        memory.orchestrator_turn = None;
        memory.bot_terminal_turn = Some(turn);
        let mut script = h.sessions.lock().unwrap();
        let mut completed = summary(ManagedState::Completed, None);
        completed.id = session;
        script.summaries.insert(session, completed);
        // The fake host answers one observation for any session; the
        // successor session's own stream holds only its events.
        script.observation.events.clear();
        script.observation.events.push(SequencedAgentEvent {
            session_id: session,
            sequence: StreamSequence::new(60).unwrap(),
            event: AgentEvent::TurnCompleted { turn_id: turn },
        });
    }
    let receipt = successor_receipt(&s, run, attempt, generation);
    let native = (
        h.orchestrator.native_effects_pending(run).map_err(|e| e.to_string()),
        h.orchestrator.native_outcomes_unknown(run).map_err(|e| e.to_string()),
    );
    let error = h
        .orchestrator
        .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
        .unwrap_err();
    let shown = error.to_string();
    let execution = h
        .schedule_store
        .inner
        .lock()
        .unwrap()
        .decision_wait_execution(fence.owner, run)
        .unwrap();
    let state = execution.as_ref().map(|execution| {
        (
            execution.wait.phase,
            execution.decision_state.clone(),
            execution.resume.as_ref().map(|resume| resume.state.clone()),
            execution.current_attempt.clone(),
        )
    });
    assert!(pending_code(error), "{shown}: {native:?} {state:?}");
    let control = latest(&h, &fence);
    assert!(matches!(
        &control.target,
        RunProjectionTargetV1::Control {
            action: DecisionControlAction::Delivery {
                outcome: DecisionDeliveryOutcome::Completed
            },
            delivery: Some(delivery),
            ..
        } if delivery.receipt == receipt
    ));
}

#[test]
fn decision_successor_delivery_is_refused_while_its_input_is_unacknowledged() {
    let (mut h, s, fence, proof, boot) = waiting_with(true, false, None);
    let run = fence.run;
    apply_next(&mut h, run);
    let (_, _, attempt, generation) = successor(&mut h, &fence, &proof, boot, None);
    let before = latest(&h, &fence);
    let receipt = successor_receipt(&s, run, attempt, generation);
    let refused = h
        .orchestrator
        .bot_acknowledge_delivery(&key(&s), fence.owner.device, &receipt)
        .unwrap_err();
    assert!(matches!(refused, RunError::Port(error) if error.code == "bot_fenced"));
    assert_eq!(latest(&h, &fence), before, "no Control was inserted");
    assert!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .bot_admission(run)
            .unwrap()
            .unwrap()
            .delivery_publication
            .is_none(),
        "no receipt was recorded"
    );
}

#[test]
fn native_decision_answer_is_forwarded_with_the_journaled_key_and_replays_it() {
    let (mut h, s, fence, policy, _) = native_results::active();
    h.orchestrator
        .synchronize_native_policy(policy.clone(), fence.owner.device)
        .unwrap();
    let offered = vec![super::super::decisions::choice("allow"), super::super::decisions::choice("reject")];
    let approval = super::super::decisions::prompt(&mut h, &fence, offered.clone());
    let proof = FreshBotPolicyProof {
        global: policy,
        routine: None,
    };
    let boot = DelegateBootId::mint();
    let prepared = h
        .orchestrator
        .bot_prepare_native_decision(&fence, fence.owner.device, approval, &proof, boot)
        .unwrap();
    // The decision is answerable only once its initial projection is
    // published; drive it through real staging as the worker would.
    {
        use crate::orchestrator::publication::RunDocumentOwner;
        let state = h.orchestrator.runs.get_mut(&fence.run).unwrap();
        let RunDocumentOwner::Attached { handle, .. } =
            std::mem::replace(&mut state.doc, RunDocumentOwner::Unopened)
        else {
            panic!("attached")
        };
        state.doc = RunDocumentOwner::Attached {
            exclusive: false,
            handle: Box::new(ProjectionHandle {
                inner: handle,
                staging: koru_delegate_document::RunProjectionStaging::default(),
            }),
        };
    }
    publish_owed(&mut h, fence.run);
    // Publication advances the decision; answer its current revision.
    let prepared = h
        .schedule_store
        .inner
        .lock()
        .unwrap()
        .bot_decision(fence.owner, fence.run, prepared.receipt.decision)
        .unwrap()
        .unwrap();
    let DecisionBindingV1::NativeApproval(binding) = &prepared.receipt.binding else {
        panic!("native binding")
    };
    assert_eq!(binding.prompt, approval);
    assert_eq!(binding.choices.as_slice().len(), 2);
    assert_eq!(
        binding.native_deadline_at_ms, None,
        "O116: no native expiry"
    );
    // A second preparation for the same Run is owned by the open decision.
    let ActorRef::Person { profile } = s.request.requested_by else {
        panic!("person")
    };
    let input = AnswerDecision {
        owner: fence.owner,
        run: fence.run,
        decision: prepared.receipt.decision,
        expected_revision: prepared.receipt.state.revision,
        binding_digest: prepared.receipt.binding_digest,
        request: DecisionAnswerId::mint(),
        human: profile,
        choice: DecisionAnswerChoice::Native {
            offered: binding.choices.as_slice()[0].clone(),
        },
        authority_digest: DecisionDigest::from_bytes([9; 32]),
    };
    let answered = h
        .orchestrator
        .bot_answer_decision(&input, fence.owner.device, &proof, boot)
        .unwrap();
    let Some(DecisionNativeResponse::Acknowledged { key, .. }) = answered.native_response else {
        panic!("acknowledged native answer")
    };
    let forwarded: Vec<_> = h
        .sessions
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter_map(|call| match call {
            Call::AnswerApproval {
                key, approval: a, ..
            } if *a == approval => Some(*key),
            _ => None,
        })
        .collect();
    assert_eq!(forwarded, vec![key.response_key]);
    // The legacy unkeyed route cannot bypass the decision owner while the
    // Run's publication is held for it.
    if h.orchestrator.run_publication_pending(fence.run) {
        assert!(
            h.orchestrator
                .bot_answer_approval(
                    &fence,
                    fence.owner.device,
                    approval,
                    ApprovalDecision::Approve,
                    None
                )
                .is_err()
        );
    }
}
