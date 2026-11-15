// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::orchestrator::state::{Attempt, TurnState};
use koru_delegate_core::{AttemptGeneration, BotId, BotOwner, DeviceId, RunAttemptId, RunId, SessionId};
use koru_session_protocol::StreamSequence;

fn fixture() -> (Attempt, DecisionSourceBoundary, DecisionProjectionPredecessor) {
    let mut attempt = Attempt::new(RunAttemptId::mint(), AttemptGeneration::FIRST);
    attempt.is_bot = true;
    attempt.session = Some(SessionId::mint());
    let turn = AgentTurnId::mint();
    attempt.orchestrator_turn = Some(turn);
    attempt.turns.insert(turn, TurnState::new(TurnOriginator::Orchestrator));
    let boundary = DecisionSourceBoundary {
        fence: DecisionFence {
            owner: BotOwner { user_scope: koru_store_core::UserScopeId::from_uuid(uuid::Uuid::new_v4()),
                device: DeviceId::from_uuid(uuid::Uuid::new_v4()) },
            bot: BotId::mint(), run: RunId::mint(), attempt: attempt.id,
            generation: attempt.generation, session: attempt.session.unwrap(), turn,
        },
        producer_epoch: uuid::Uuid::new_v4(), stream: DecisionSourceStream::Agent,
        sequence: StreamSequence::new(9).unwrap(),
        resulting: DecisionSourceFrontiers { agent_after: Some(StreamSequence::new(9).unwrap()),
            session_after: Some(StreamSequence::new(4).unwrap()) },
    };
    let predecessor = DecisionProjectionPredecessor { observed_created_at_ms: 1,
        timeline_start: 3, projection: None, status: RunStatus::running(), failure_reason: None };
    (attempt, boundary, predecessor)
}

#[test]
fn decision_source_fold_retains_exact_tool_flush_without_mutating_live_tally() {
    let (mut attempt, boundary, predecessor) = fixture();
    let turn = boundary.fence.turn;
    attempt.turns.get_mut(&turn).unwrap().tool_calls = 64;
    let fold = SourceFold::agent(SourceMemory::freeze(&attempt).unwrap(), boundary, predecessor,
        &AgentEvent::TurnCompleted { turn_id: turn }, 20).unwrap();
    assert_eq!(attempt.turns[&turn].tool_calls, 64);
    assert_eq!(fold.target.effects().steps.as_slice().len(), 2);
    assert!(matches!(fold.target.effects().steps.as_slice()[0].kind,
        StepKind::ToolsCompleted { calls: 64, failed: 0, .. }));
    assert!(fold.memory.turns.as_slice().is_empty());
    assert_eq!(fold.memory.terminal_turn, Some(turn));
    assert_eq!(fold.memory.orchestrator_turn, None);
    assert!(matches!(&fold.target, RunProjectionTargetV1::Source { transitions, .. }
        if transitions.as_slice().is_empty()));
}

#[test]
fn decision_source_fold_terminal_failure_closes_attention_before_failure_once() {
    let (mut attempt, boundary, mut predecessor) = fixture();
    let turn = boundary.fence.turn;
    attempt.pending_approvals.insert(ApprovalId::mint(), turn);
    predecessor.status = transition_bot(&predecessor.status, &RunEvent::ApprovalRequired).unwrap();
    let event = AgentEvent::TurnFailed { turn_id: turn,
        error: koru_session_protocol::SessionFailure {
            code: koru_session_protocol::ErrorCode::Internal,
            message: "private provider detail".into(),
        } };
    let fold = SourceFold::agent(SourceMemory::freeze(&attempt).unwrap(), boundary.clone(),
        predecessor.clone(), &event, 20).unwrap();
    assert!(matches!(&fold.target, RunProjectionTargetV1::Source { transitions, .. }
        if transitions.as_slice() == [RunEvent::ApprovalResolved,
            RunEvent::TurnFailed { originator: TurnOriginator::Orchestrator, stage: FailureStage::Turn }]));
    assert_eq!(fold.target.effects().steps.as_slice().iter()
        .filter(|step| matches!(step.kind, StepKind::Failed { .. })).count(), 1);
    assert!(!String::from_utf8(fold.target.canonical_bytes().unwrap().as_slice().to_vec())
        .unwrap().contains("private provider detail"));
    predecessor.status = RunStatus::Failed { stage: FailureStage::Turn };
    predecessor.failure_reason = Some(RunFailureReason::from_code("internal"));
    let late = SourceFold::agent(SourceMemory::freeze(&attempt).unwrap(), boundary,
        predecessor.clone(), &event, 21).unwrap();
    assert!(late.target.effects().steps.as_slice().is_empty());
    assert_eq!(late.target.effects().failure_reason, predecessor.failure_reason);
}

#[test]
fn decision_source_fold_approval_keeps_ordinary_refs_and_content_free_fact() {
    let (attempt, boundary, predecessor) = fixture();
    let turn = boundary.fence.turn;
    let approval = ApprovalId::mint();
    let fold = SourceFold::agent(SourceMemory::freeze(&attempt).unwrap(), boundary, predecessor,
        &AgentEvent::ApprovalRequired { turn_id: turn, approval_id: approval,
            summary: "PRIVATE_SENTINEL_ASK".into(), choices: vec![] }, 20).unwrap();
    let step = &fold.target.effects().steps.as_slice()[0];
    assert_eq!(step.refs, TimelineRefs::new([TimelineRef::Turn { turn },
        TimelineRef::Approval { approval }]).unwrap());
    assert_eq!(fold.memory.approvals.as_slice(), &[(approval, turn)]);
    assert!(attempt.pending_approvals.is_empty());
    assert!(!String::from_utf8(fold.target.canonical_bytes().unwrap().as_slice().to_vec())
        .unwrap().contains("PRIVATE_SENTINEL_ASK"));
}
