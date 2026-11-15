// SPDX-License-Identifier: MPL-2.0

//! Bounded borrowed observation at the source owner entrance. An ordinary Run
//! can have more working memory than one decision target can retain. Refuse
//! that boundary before cloning it; do not invent an empty accepted snapshot.

use crate::orchestrator::state::Attempt;
use koru_delegate_core::decisions::{
    DECISION_PAYLOAD_BYTES, DecisionError, DecisionList, DecisionTurnTally, DecisionValidate,
    encode_decision_value,
};
use koru_delegate_core::{
    AgentTurnId, ApprovalId, AttemptGeneration, QuestionRequestId, RunAttemptId, SessionId,
};
use serde::{Deserialize, Serialize, Serializer, ser::SerializeSeq};
use std::collections::HashMap;

/// Only source bookkeeping, not capture, document history, controller ownership
/// or process/capacity authority. Physical control fields are never restored
/// from an earlier staged observation after a concurrent Stop.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceMemory {
    pub attempt: RunAttemptId,
    pub generation: AttemptGeneration,
    pub session: Option<SessionId>,
    pub orchestrator_turn: Option<AgentTurnId>,
    pub terminal_turn: Option<AgentTurnId>,
    pub turns: DecisionList<DecisionTurnTally, 65_536>,
    pub approvals: DecisionList<(ApprovalId, AgentTurnId), 65_536>,
    pub questions: DecisionList<(QuestionRequestId, AgentTurnId), 65_536>,
}

impl SourceMemory {
    pub(super) fn freeze(attempt: &Attempt) -> Result<Self, DecisionError> {
        if !attempt.is_bot
            || !attempt.approval_evidence_complete
            || attempt.recovered_unowned_turn.is_some()
        {
            return Err(DecisionError::Invalid);
        }
        #[derive(Serialize)]
        struct Borrowed<'a> {
            attempt: RunAttemptId,
            generation: AttemptGeneration,
            session: Option<SessionId>,
            orchestrator_turn: Option<AgentTurnId>,
            terminal_turn: Option<AgentTurnId>,
            turns: Turns<'a>,
            approvals: Pairs<'a, ApprovalId>,
            questions: Pairs<'a, QuestionRequestId>,
        }
        let borrowed = Borrowed {
            attempt: attempt.id,
            generation: attempt.generation,
            session: attempt.session,
            orchestrator_turn: attempt.orchestrator_turn,
            terminal_turn: attempt.bot_terminal_turn,
            turns: Turns(&attempt.turns),
            approvals: Pairs(&attempt.pending_approvals),
            questions: Pairs(&attempt.pending_questions),
        };
        // The existing bounded writer stops during borrowed iteration. No
        // whole-map clone, Value tree, capture copy or history traversal occurs.
        let bytes = encode_decision_value(&borrowed, DECISION_PAYLOAD_BYTES)?;
        let mut frozen: Self =
            serde_json::from_slice(&bytes).map_err(|_| DecisionError::Invalid)?;
        for turn in frozen.turns.as_slice() {
            turn.validate()?;
        }
        // HashMap iteration order is not retained authority. The target builder
        // uses deterministic typed-key order when freezing the resulting tally.
        let mut turns = frozen.turns.as_slice().to_vec();
        turns.sort_by_key(|turn| turn.turn);
        frozen.turns = DecisionList::new(turns)?;
        Ok(frozen)
    }
}

struct Turns<'a>(&'a HashMap<AgentTurnId, crate::orchestrator::state::TurnState>);
impl Serialize for Turns<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for (turn, tally) in self.0 {
            sequence.serialize_element(&DecisionTurnTally {
                turn: *turn,
                originator: tally.originator,
                tool_calls: tally.tool_calls,
                tool_failures: tally.tool_failures,
                failure_breakdown: tally.failure_breakdown,
            })?;
        }
        sequence.end()
    }
}
struct Pairs<'a, K>(&'a HashMap<K, AgentTurnId>);
impl<K: Serialize> Serialize for Pairs<'_, K> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for pair in self.0 {
            sequence.serialize_element(&pair)?;
        }
        sequence.end()
    }
}

#[cfg(test)]
mod tests;
