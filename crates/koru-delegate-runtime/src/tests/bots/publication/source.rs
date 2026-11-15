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

#[test]
fn decision_settled_native_answer_still_stops_on_the_execution_budget() {
    let (mut h, _, fence, proof, boot) = waiting_with(true, true);
    let run = fence.run;
    apply_next(&mut h, run);
    let decision = latest(&h, &fence).identity.decision.unwrap().decision;
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
        .answer_bot_decision(&input, DecisionTransitionId::mint(), &proof, boot)
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
