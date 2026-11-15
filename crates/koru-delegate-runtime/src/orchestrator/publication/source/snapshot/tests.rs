// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::orchestrator::state::TurnState;
use koru_delegate_core::{ToolFailures, TurnOriginator};

fn attempt() -> Attempt {
    let mut attempt = Attempt::new(RunAttemptId::mint(), AttemptGeneration::FIRST);
    attempt.is_bot = true;
    attempt
}

#[test]
fn decision_source_snapshot_keeps_existing_subthreshold_tally_without_mutation() {
    let mut attempt = attempt();
    let turn = AgentTurnId::mint();
    attempt.turns.insert(
        turn,
        TurnState {
            originator: TurnOriginator::Orchestrator,
            tool_calls: 17,
            tool_failures: 0,
            failure_breakdown: ToolFailures::default(),
        },
    );
    let approval = ApprovalId::mint();
    attempt.pending_approvals.insert(approval, turn);
    let frozen = SourceMemory::freeze(&attempt).unwrap();
    assert_eq!(frozen.turns.as_slice()[0].turn, turn);
    assert_eq!(frozen.turns.as_slice()[0].tool_calls, 17);
    assert_eq!(frozen.approvals.as_slice(), &[(approval, turn)]);
    assert_eq!(attempt.turns[&turn].tool_calls, 17);
    assert_eq!(attempt.pending_approvals.len(), 1);
}

#[test]
fn decision_source_snapshot_refuses_large_or_unproved_boundary_without_empty_fallback() {
    let mut attempt = attempt();
    attempt.approval_evidence_complete = false;
    assert!(matches!(
        SourceMemory::freeze(&attempt),
        Err(DecisionError::Invalid)
    ));
    attempt.approval_evidence_complete = true;
    attempt.recovered_unowned_turn = Some(AgentTurnId::mint());
    assert!(matches!(
        SourceMemory::freeze(&attempt),
        Err(DecisionError::Invalid)
    ));
    attempt.recovered_unowned_turn = None;
    for _ in 0..2_000 {
        attempt
            .turns
            .insert(AgentTurnId::mint(), TurnState::new(TurnOriginator::Human));
    }
    assert!(matches!(
        SourceMemory::freeze(&attempt),
        Err(DecisionError::TooLarge)
    ));
    assert_eq!(
        attempt.turns.len(),
        2_000,
        "refusal never erases ordinary progress"
    );
}
