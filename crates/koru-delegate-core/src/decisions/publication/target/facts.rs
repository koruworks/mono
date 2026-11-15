// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{RunFact, StepKind, TimelineEntryId, TimelineRef};
use std::collections::BTreeMap;

/// Exact existing publication vocabulary with identities allocated before SQL
/// acceptance. This is data, never permission to execute a source-side action.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionProjectionPublication {
    pub event: EventId,
    pub step: Option<TimelineEntryId>,
    #[serde(deserialize_with = "strict_leaf")]
    pub fact: RunFact,
}
impl std::fmt::Debug for DecisionProjectionPublication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DecisionProjectionPublication([REDACTED])")
    }
}

pub(super) fn validate(target: &RunProjectionTargetV1) -> Result<(), DecisionError> {
    let fence = target.fence();
    let projected = target.projected_steps()?;
    let steps: BTreeMap<_, _> = projected.iter().map(|step| (step.id, step)).collect();
    let mut events = BTreeSet::new();
    let mut referenced = BTreeSet::new();
    let mut usage_published = false;
    let mut child_published = false;
    for publication in target.publications() {
        require(events.insert(publication.event) && publication.fact.run() == fence.run)?;
        if matches!(publication.fact, RunFact::Completed { .. }) {
            require(target.delivery().is_some())?;
        }
        if let RunFact::ChildSessionDetected { terminated, .. } = &publication.fact {
            let RunProjectionTargetV1::Source { transitions, .. } = target else {
                return Err(DecisionError::Invalid);
            };
            require(!child_published)?;
            child_published = true;
            // The boolean retains request acceptance, never process-death
            // evidence. It must agree with the exact owner-observed transition.
            require(if *terminated {
                transitions.as_slice()
                    == [
                        RunEvent::ChildSessionDetected,
                        RunEvent::ChildSessionTerminated,
                    ]
            } else {
                transitions.as_slice() == [RunEvent::ChildSessionDetected]
            })?;
        }
        if let RunFact::Usage {
            attempt,
            turn,
            report,
            ..
        } = &publication.fact
        {
            let usage = target
                .source_bookkeeping()
                .and_then(|b| b.usage.as_ref())
                .ok_or(DecisionError::Invalid)?;
            require(
                publication.step.is_none()
                    && publication.event == usage.event
                    && *attempt == fence.attempt
                    && *attempt == usage.attempt
                    && *turn == usage.turn
                    && report.usage == usage.id
                    && report.revision == usage.revision
                    && report.input == usage.input
                    && report.output == usage.output
                    && report.cached_input == usage.cached_input
                    && report.model == usage.model
                    && report.completeness == usage.fact().completeness
                    && report.cached_relationship == usage.cached_input_relationship,
            )?;
            require(!usage_published)?;
            usage_published = true;
            continue;
        }
        let id = publication.step.ok_or(DecisionError::Invalid)?;
        require(referenced.insert(id))?;
        let step = steps.get(&id).ok_or(DecisionError::Invalid)?;
        validate_step(&publication.fact, step, fence)?;
        if let RunProjectionTargetV1::Control { action, flush, .. } = target {
            if flush
                .as_slice()
                .iter()
                .any(|r| r.step == id && r.event == publication.event)
            {
                // derive() already matched the complete exact fact to its tally.
                continue;
            }
            require(matches!(
                (action, &publication.fact),
                (
                    DecisionControlAction::DecisionWaiting {},
                    RunFact::ApprovalRequired { .. }
                ) | (
                    DecisionControlAction::DecisionResolved { .. },
                    RunFact::ApprovalResolved { .. }
                ) | (DecisionControlAction::Stop {}, RunFact::Cancelled { .. })
                    | (
                        DecisionControlAction::Interrupt { .. },
                        RunFact::Interrupted { .. }
                    )
                    | (
                        DecisionControlAction::Resume { .. },
                        RunFact::Resumed { .. }
                    )
                    | (
                        DecisionControlAction::ControlTaken { .. },
                        RunFact::ControlTaken { .. }
                    )
                    | (
                        DecisionControlAction::ControlReturned {},
                        RunFact::ControlReturned { .. }
                    )
                    | (
                        DecisionControlAction::Delivery {
                            outcome: DecisionDeliveryOutcome::Completed
                        },
                        RunFact::Completed { .. }
                    )
            ))?;
        }
    }
    if let RunProjectionTargetV1::Source { transitions, .. } = target {
        // A Source target is the complete accepted event, not an optional
        // subset of its bus effects. Bookkeeping-only events have no steps;
        // every retained source step and usage amendment otherwise has its
        // stable envelope before Prepared, including fact-only source detail.
        require(referenced.len() == steps.len())?;
        require(
            usage_published
                == target
                    .source_bookkeeping()
                    .is_some_and(|b| b.usage.is_some()),
        )?;
        require(
            child_published
                == transitions.as_slice().iter().any(|event| {
                    matches!(
                        event,
                        RunEvent::ChildSessionDetected | RunEvent::ChildSessionTerminated
                    )
                }),
        )?;
    }
    Ok(())
}

fn validate_step(
    fact: &RunFact,
    step: &TimelineEntry,
    fence: &DecisionFence,
) -> Result<(), DecisionError> {
    let attempt;
    let mut turn = None;
    let expected = match fact {
        RunFact::TurnStarted {
            attempt: a,
            turn: t,
            originator,
            ..
        } => {
            attempt = Some(*a);
            turn = Some(*t);
            StepKind::TurnStarted {
                originator: *originator,
            }
        }
        RunFact::ToolsCompleted {
            attempt: a,
            turn: t,
            calls,
            failed,
            ..
        } => {
            attempt = Some(*a);
            turn = Some(*t);
            let StepKind::ToolsCompleted { failures, .. } = step.kind else {
                return Err(DecisionError::Invalid);
            };
            require(failures.total() == *failed)?;
            StepKind::ToolsCompleted {
                calls: *calls,
                failed: *failed,
                failures,
            }
        }
        RunFact::ApprovalRequired {
            attempt: a,
            turn: t,
            approval,
            ..
        } => {
            attempt = Some(*a);
            turn = Some(*t);
            require(step.refs.iter().any(|r| {
                *r == TimelineRef::Approval {
                    approval: *approval,
                }
            }))?;
            StepKind::ApprovalRequired
        }
        RunFact::ApprovalResolved {
            attempt: a,
            turn: t,
            approval,
            outcome,
            ..
        } => {
            attempt = Some(*a);
            turn = Some(*t);
            require(step.refs.iter().any(|r| {
                *r == TimelineRef::Approval {
                    approval: *approval,
                }
            }))?;
            StepKind::ApprovalResolved { outcome: *outcome }
        }
        RunFact::WaitingForInput {
            attempt: a,
            turn: t,
            ..
        } => {
            attempt = Some(*a);
            turn = Some(*t);
            StepKind::WaitingForInput
        }
        RunFact::AttachmentAccepted {
            attempt: a,
            turn: t,
            ..
        } => {
            attempt = Some(*a);
            turn = Some(*t);
            // This historical fact deliberately has no attachment ID. Its
            // owning step retains the single exact attachment reference.
            require(
                step.refs
                    .iter()
                    .filter(|r| matches!(r, TimelineRef::Attachment { .. }))
                    .count()
                    == 1,
            )?;
            StepKind::AttachmentAccepted
        }
        RunFact::AttachmentRejected {
            attempt: a,
            turn: t,
            attachment,
            ..
        } => {
            attempt = Some(*a);
            turn = Some(*t);
            require(step.refs.iter().any(|r| {
                *r == TimelineRef::Attachment {
                    attachment: *attachment,
                }
            }))?;
            StepKind::AttachmentRejected
        }
        RunFact::ChildSessionDetected {
            attempt: a,
            turn: t,
            child,
            ..
        } => {
            attempt = Some(*a);
            turn = Some(*t);
            require(
                step.refs
                    .iter()
                    .any(|r| *r == TimelineRef::Session { session: *child }),
            )?;
            StepKind::ChildSessionDetected
        }
        RunFact::TurnCompleted {
            attempt: a,
            turn: t,
            originator,
            artifacts_registered,
            ..
        } => {
            attempt = Some(*a);
            turn = Some(*t);
            StepKind::TurnCompleted {
                originator: *originator,
                artifacts_registered: *artifacts_registered,
            }
        }
        RunFact::TurnCancelled {
            attempt: a,
            turn: t,
            originator,
            reason,
            ..
        } => {
            attempt = Some(*a);
            turn = Some(*t);
            StepKind::TurnCancelled {
                originator: *originator,
                reason: *reason,
            }
        }
        RunFact::TurnFailed {
            attempt: a,
            turn: t,
            originator,
            stage,
            ..
        } => {
            attempt = Some(*a);
            turn = Some(*t);
            StepKind::TurnFailed {
                originator: *originator,
                stage: *stage,
            }
        }
        RunFact::Gap {
            attempt: a,
            events_lost,
            ..
        } => {
            attempt = Some(*a);
            StepKind::Gap {
                events_lost: *events_lost,
            }
        }
        RunFact::ControlTaken {
            attempt: a,
            discovered,
            ..
        } => {
            attempt = Some(*a);
            StepKind::ControlTaken {
                discovered: *discovered,
            }
        }
        RunFact::ControlReturned { attempt: a, .. } => {
            attempt = Some(*a);
            StepKind::ControlReturned
        }
        RunFact::Interrupted {
            attempt: a,
            generation,
            prior_attempt,
            ..
        } => {
            attempt = Some(*a);
            require(*generation == fence.generation)?;
            StepKind::Interrupted {
                prior_attempt: *prior_attempt,
            }
        }
        RunFact::Resumed {
            attempt: a,
            generation,
            via,
            ..
        } => {
            attempt = Some(*a);
            require(*generation == fence.generation)?;
            StepKind::Resumed { via: *via }
        }
        RunFact::Cancelled {
            attempt: a,
            generation,
            reason,
            ..
        } => {
            attempt = *a;
            require(*generation == fence.generation && a.is_some())?;
            StepKind::Cancelled { reason: *reason }
        }
        RunFact::Failed {
            attempt: a,
            generation,
            stage,
            ..
        } => {
            attempt = *a;
            require(*generation == fence.generation && a.is_some())?;
            let StepKind::Failed { reason, .. } = step.kind else {
                return Err(DecisionError::Invalid);
            };
            StepKind::Failed {
                stage: *stage,
                reason,
            }
        }
        RunFact::ArtifactRegistered {
            attempt: a,
            artifact,
            ..
        } => {
            attempt = Some(*a);
            require(step.refs.iter().any(|r| {
                *r == TimelineRef::Artifact {
                    artifact: *artifact,
                }
            }))?;
            StepKind::ArtifactRegistered
        }
        RunFact::Completed {
            attempt: a,
            generation,
            ..
        } => {
            attempt = Some(*a);
            require(*generation == fence.generation)?;
            StepKind::Completed
        }
        // Created/Started/ScopeBusy/Rating are outside the decision window;
        // completion is the separate receipt-qualified Bot delivery owner.
        _ => return Err(DecisionError::Invalid),
    };
    require(step.kind == expected && attempt == Some(fence.attempt))?;
    // These existing producers authored Turn + subject refs, never Attempt.
    // The fact's attempt above remains mandatory authority. Do not change old
    // step bytes merely to accommodate this new envelope's validation.
    let attempt_optional = matches!(
        fact,
        RunFact::ApprovalRequired { .. }
            | RunFact::ApprovalResolved { .. }
            | RunFact::AttachmentAccepted { .. }
            | RunFact::AttachmentRejected { .. }
            | RunFact::ChildSessionDetected { .. }
    );
    let mut attempt_seen = false;
    for reference in step.refs.iter() {
        if let TimelineRef::Attempt { attempt } = reference {
            require(*attempt == fence.attempt)?;
            attempt_seen = true;
        }
    }
    require(attempt_optional || attempt_seen)?;
    if let Some(turn) = turn {
        require(step.refs.iter().any(|r| *r == TimelineRef::Turn { turn }))?;
    }
    Ok(())
}
