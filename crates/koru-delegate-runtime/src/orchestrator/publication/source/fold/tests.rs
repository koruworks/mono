// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::orchestrator::state::{Attempt, TurnState};
use koru_delegate_core::{
    AttemptGeneration, BotId, BotOwner, DeviceId, RunAttemptId, RunId, SessionId,
};
use koru_session_protocol::StreamSequence;

fn fixture() -> (
    Attempt,
    DecisionSourceBoundary,
    DecisionProjectionPredecessor,
) {
    let mut attempt = Attempt::new(RunAttemptId::mint(), AttemptGeneration::FIRST);
    attempt.is_bot = true;
    attempt.session = Some(SessionId::mint());
    let turn = AgentTurnId::mint();
    attempt.orchestrator_turn = Some(turn);
    attempt
        .turns
        .insert(turn, TurnState::new(TurnOriginator::Orchestrator));
    let boundary = DecisionSourceBoundary {
        fence: DecisionFence {
            owner: BotOwner {
                user_scope: koru_store_core::UserScopeId::from_uuid(uuid::Uuid::new_v4()),
                device: DeviceId::from_uuid(uuid::Uuid::new_v4()),
            },
            bot: BotId::mint(),
            run: RunId::mint(),
            attempt: attempt.id,
            generation: attempt.generation,
            session: attempt.session.unwrap(),
            turn,
        },
        producer_epoch: uuid::Uuid::new_v4(),
        stream: DecisionSourceStream::Agent,
        sequence: StreamSequence::new(9).unwrap(),
        resulting: DecisionSourceFrontiers {
            agent_after: Some(StreamSequence::new(9).unwrap()),
            session_after: Some(StreamSequence::new(4).unwrap()),
        },
    };
    let predecessor = DecisionProjectionPredecessor {
        observed_created_at_ms: 1,
        timeline_start: 3,
        projection: None,
        status: RunStatus::running(),
        failure_reason: None,
    };
    (attempt, boundary, predecessor)
}

#[test]
fn decision_source_fold_retains_exact_tool_flush_without_mutating_live_tally() {
    let (mut attempt, boundary, predecessor) = fixture();
    let turn = boundary.fence.turn;
    attempt.turns.get_mut(&turn).unwrap().tool_calls = 64;
    let fold = SourceFold::agent(
        SourceMemory::freeze(&attempt).unwrap(),
        boundary,
        predecessor,
        &AgentEvent::TurnCompleted { turn_id: turn },
        None,
        20,
    )
    .unwrap();
    assert_eq!(attempt.turns[&turn].tool_calls, 64);
    assert_eq!(fold.target.effects().steps.as_slice().len(), 2);
    assert!(matches!(
        fold.target.effects().steps.as_slice()[0].kind,
        StepKind::ToolsCompleted {
            calls: 64,
            failed: 0,
            ..
        }
    ));
    assert!(fold.memory.turns.as_slice().is_empty());
    assert_eq!(fold.memory.terminal_turn, Some(turn));
    assert_eq!(fold.memory.orchestrator_turn, None);
    assert!(
        matches!(&fold.target, RunProjectionTargetV1::Source { transitions, .. }
        if transitions.as_slice().is_empty())
    );
}

#[test]
fn decision_source_fold_terminal_failure_closes_attention_before_failure_once() {
    let (mut attempt, boundary, mut predecessor) = fixture();
    let turn = boundary.fence.turn;
    attempt.pending_approvals.insert(ApprovalId::mint(), turn);
    predecessor.status = transition_bot(&predecessor.status, &RunEvent::ApprovalRequired).unwrap();
    let event = AgentEvent::TurnFailed {
        turn_id: turn,
        error: koru_session_protocol::SessionFailure {
            code: koru_session_protocol::ErrorCode::Internal,
            message: "private provider detail".into(),
        },
    };
    let fold = SourceFold::agent(
        SourceMemory::freeze(&attempt).unwrap(),
        boundary.clone(),
        predecessor.clone(),
        &event,
        None,
        20,
    )
    .unwrap();
    assert!(
        matches!(&fold.target, RunProjectionTargetV1::Source { transitions, .. }
        if transitions.as_slice() == [RunEvent::ApprovalResolved,
            RunEvent::TurnFailed { originator: TurnOriginator::Orchestrator, stage: FailureStage::Turn }])
    );
    assert_eq!(
        fold.target
            .effects()
            .steps
            .as_slice()
            .iter()
            .filter(|step| matches!(step.kind, StepKind::Failed { .. }))
            .count(),
        1
    );
    assert!(
        !String::from_utf8(fold.target.canonical_bytes().unwrap().as_slice().to_vec())
            .unwrap()
            .contains("private provider detail")
    );
    predecessor.status = RunStatus::Failed {
        stage: FailureStage::Turn,
    };
    predecessor.failure_reason = Some(RunFailureReason::from_code("internal"));
    let late = SourceFold::agent(
        SourceMemory::freeze(&attempt).unwrap(),
        boundary,
        predecessor.clone(),
        &event,
        None,
        21,
    )
    .unwrap();
    assert!(late.target.effects().steps.as_slice().is_empty());
    assert_eq!(
        late.target.effects().failure_reason,
        predecessor.failure_reason
    );
}

#[test]
fn decision_source_fold_approval_keeps_ordinary_refs_and_content_free_fact() {
    let (attempt, boundary, predecessor) = fixture();
    let turn = boundary.fence.turn;
    let approval = ApprovalId::mint();
    let fold = SourceFold::agent(
        SourceMemory::freeze(&attempt).unwrap(),
        boundary,
        predecessor,
        &AgentEvent::ApprovalRequired {
            turn_id: turn,
            approval_id: approval,
            summary: "PRIVATE_SENTINEL_ASK".into(),
            choices: vec![],
        },
        None,
        20,
    )
    .unwrap();
    let step = &fold.target.effects().steps.as_slice()[0];
    assert_eq!(
        step.refs,
        TimelineRefs::new([
            TimelineRef::Turn { turn },
            TimelineRef::Approval { approval }
        ])
        .unwrap()
    );
    assert_eq!(fold.memory.approvals.as_slice(), &[(approval, turn)]);
    assert!(attempt.pending_approvals.is_empty());
    assert!(
        !String::from_utf8(fold.target.canonical_bytes().unwrap().as_slice().to_vec())
            .unwrap()
            .contains("PRIVATE_SENTINEL_ASK")
    );
}

#[test]
fn decision_source_fold_child_detection_carries_only_the_answered_request() {
    let (attempt, boundary, predecessor) = fixture();
    let turn = boundary.fence.turn;
    let child = SessionId::mint();
    let event = AgentEvent::ChildSessionCreated {
        turn_id: turn,
        child_session_id: child,
    };
    for terminated in [true, false] {
        let fold = SourceFold::agent(
            SourceMemory::freeze(&attempt).unwrap(),
            boundary.clone(),
            predecessor.clone(),
            &event,
            Some(terminated),
            20,
        )
        .unwrap();
        let expected: Vec<RunEvent> = if terminated {
            vec![
                RunEvent::ChildSessionDetected,
                RunEvent::ChildSessionTerminated,
            ]
        } else {
            vec![RunEvent::ChildSessionDetected]
        };
        assert!(
            matches!(&fold.target, RunProjectionTargetV1::Source { transitions, .. }
            if transitions.as_slice() == expected.as_slice())
        );
        assert!(fold.target.publications().iter().any(|publication| matches!(
            publication.fact, RunFact::ChildSessionDetected { terminated: t, .. } if t == terminated)));
    }
    // A live child needs the answered request; a non-child frame carries none.
    assert!(
        SourceFold::agent(
            SourceMemory::freeze(&attempt).unwrap(),
            boundary.clone(),
            predecessor.clone(),
            &event,
            None,
            20
        )
        .is_err()
    );
    assert!(
        SourceFold::agent(
            SourceMemory::freeze(&attempt).unwrap(),
            boundary,
            predecessor,
            &AgentEvent::TurnStarted { turn_id: turn },
            Some(true),
            20
        )
        .is_err()
    );
}

#[test]
fn decision_source_fold_process_end_interrupts_live_and_absorbs_terminal() {
    let (attempt, mut boundary, mut predecessor) = fixture();
    boundary.stream = DecisionSourceStream::Session;
    boundary.sequence = StreamSequence::new(5).unwrap();
    boundary.resulting.session_after = Some(boundary.sequence);
    let live = SourceFold::process_ended(
        SourceMemory::freeze(&attempt).unwrap(),
        boundary.clone(),
        predecessor.clone(),
        20,
    )
    .unwrap();
    assert!(
        matches!(&live.target, RunProjectionTargetV1::Source { transitions, .. }
        if transitions.as_slice() == [RunEvent::Interrupt { prior_attempt: PriorAttempt::Ended }])
    );
    assert!(matches!(
        live.target.effects().steps.as_slice()[0].kind,
        StepKind::Interrupted {
            prior_attempt: PriorAttempt::Ended
        }
    ));
    predecessor.status = RunStatus::Cancelled;
    let absorbed = SourceFold::process_ended(
        SourceMemory::freeze(&attempt).unwrap(),
        boundary,
        predecessor,
        21,
    )
    .unwrap();
    assert!(absorbed.target.effects().steps.as_slice().is_empty());
    assert_eq!(absorbed.target.effects().status, RunStatus::Cancelled);
}

#[test]
fn decision_source_fold_usage_is_one_exact_envelope_with_economics_time() {
    let (attempt, boundary, mut predecessor) = fixture();
    let model = ModelIdentity {
        id: koru_delegate_core::ModelRef::mint(),
        provider: "unknown".parse().unwrap(),
        requested: koru_delegate_core::ModelReport::Unknown,
        reported: koru_delegate_core::ModelReport::Unknown,
    };
    let usage = DecisionUsageSnapshot {
        id: koru_delegate_core::UsageFactId::mint(),
        revision: koru_delegate_core::AmendmentRevision(2),
        event: EventId::mint(),
        attempt: attempt.id,
        turn: boundary.fence.turn,
        input: Some(3),
        output: Some(4),
        cached_input: None,
        cached_input_relationship: koru_session_protocol::CachedInputRelationship::Separate,
        model: model.id,
        completeness: DecisionUsageCompleteness::Complete {},
    };
    // Usage amends even a terminal Run; it never changes status.
    predecessor.status = RunStatus::Cancelled;
    let fold = SourceFold::usage(
        SourceMemory::freeze(&attempt).unwrap(),
        boundary.clone(),
        predecessor.clone(),
        usage.clone(),
        Some(model.clone()),
        33,
    )
    .unwrap();
    assert_eq!(fold.target.effects().economics_at_ms, Some(33));
    assert_eq!(
        fold.target.effects().models.as_slice(),
        std::slice::from_ref(&model)
    );
    assert_eq!(
        fold.target.source_bookkeeping().unwrap().usage.as_ref(),
        Some(&usage)
    );
    assert_eq!(fold.target.publications().len(), 1);
    assert_eq!(fold.target.publications()[0].event, usage.event);
    // A minted identity must be the one the envelope names.
    let mut other = model;
    other.id = koru_delegate_core::ModelRef::mint();
    assert!(
        SourceFold::usage(
            SourceMemory::freeze(&attempt).unwrap(),
            boundary,
            predecessor,
            usage,
            Some(other),
            33
        )
        .is_err()
    );
}
