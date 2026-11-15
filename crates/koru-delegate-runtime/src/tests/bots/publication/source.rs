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
    assert!(
        h.orchestrator
            .next_run_publication(run)
            .unwrap_err()
            .publication_pending()
    );
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
