// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{
    AgentTurnId, RunFact, StepKind, TimelineEntryId, TimelineRef, TimelineRefs, ToolFailures,
};
use std::collections::BTreeMap;

/// Only allocation metadata. Counts and originator come from the exact applied
/// predecessor, never a second caller tally or an invented source sequence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionControlFlush {
    pub turn: AgentTurnId,
    pub step: TimelineEntryId,
    pub event: EventId,
    pub at_ms: i64,
}

pub(super) fn derive(
    target: &RunProjectionTargetV1,
) -> Result<(Option<DecisionSourceBookkeeping>, Vec<TimelineEntry>), DecisionError> {
    let RunProjectionTargetV1::Control {
        flush,
        action,
        retained_bookkeeping,
        fence,
        ..
    } = target
    else {
        return Ok((target.bookkeeping().cloned(), Vec::new()));
    };
    if flush.as_slice().is_empty() {
        return Ok((retained_bookkeeping.clone(), Vec::new()));
    }
    require(matches!(
        action,
        DecisionControlAction::Stop {} | DecisionControlAction::Interrupt { .. }
    ))?;
    let mut result = retained_bookkeeping.clone().ok_or(DecisionError::Invalid)?;
    require(result.boundary.fence == *fence)?;
    let mut by_turn: BTreeMap<_, _> = result
        .unflushed
        .as_slice()
        .iter()
        .enumerate()
        .map(|(index, tally)| (tally.turn, index))
        .collect();
    let mut tallies = result.unflushed.as_slice().to_vec();
    let mut steps: BTreeSet<_> = result.allocated_steps.as_slice().iter().copied().collect();
    for step in target.effects().steps.as_slice() {
        require(steps.insert(step.id))?;
    }
    let mut events = BTreeSet::new();
    let publications: BTreeMap<_, _> = target.publications().iter().map(|p| (p.event, p)).collect();
    require(publications.len() == target.publications().len())?;
    let mut derived = Vec::with_capacity(flush.as_slice().len());
    for offered in flush.as_slice() {
        let index = by_turn
            .remove(&offered.turn)
            .ok_or(DecisionError::Invalid)?;
        let tally = &mut tallies[index];
        require(
            tally.tool_calls > 0 && steps.insert(offered.step) && events.insert(offered.event),
        )?;
        let expected = DecisionProjectionPublication {
            event: offered.event,
            step: Some(offered.step),
            fact: RunFact::ToolsCompleted {
                run: fence.run,
                attempt: fence.attempt,
                turn: offered.turn,
                calls: tally.tool_calls,
                failed: tally.tool_failures,
            },
        };
        require(publications.get(&offered.event).copied() == Some(&expected))?;
        derived.push(TimelineEntry {
            id: offered.step,
            at_ms: offered.at_ms,
            kind: StepKind::ToolsCompleted {
                calls: tally.tool_calls,
                failed: tally.tool_failures,
                failures: tally.failure_breakdown,
            },
            refs: TimelineRefs::new([
                TimelineRef::Attempt {
                    attempt: fence.attempt,
                },
                TimelineRef::Turn { turn: offered.turn },
            ])
            .map_err(|_| DecisionError::Invalid)?,
        });
        tally.tool_calls = 0;
        tally.tool_failures = 0;
        tally.failure_breakdown = ToolFailures::default();
    }
    result.unflushed = DecisionList::new(tallies)?;
    // Turn/originator, both source frontiers, usage and prior allocated_steps
    // remain exactly as accepted. No new Source boundary is manufactured.
    Ok((Some(result), derived))
}
