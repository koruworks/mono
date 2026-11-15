// SPDX-License-Identifier: MPL-2.0

//! Pure staging of one accepted observation. This owns no document, ledger,
//! lease or provider. The caller installs memory only after the exact Applied
//! receipt and handle return, and retries the retained target on unknown results.

use super::snapshot::SourceMemory;
use koru_delegate_core::decisions::*;
use koru_delegate_core::{
    AgentTurnId, ApprovalId, ApprovalOutcome, Attention, CancelReason, EventId, FailureStage,
    RunEvent, RunFact, RunFailureReason, RunStatus, StepKind, TimelineEntry, TimelineRef,
    TimelineRefs, ToolFailure, ToolFailures, ToolKind, TurnOriginator, transition_bot,
};
use koru_session_protocol::{AgentEvent, ApprovalDecision};

#[cfg(test)]
mod tests;

pub(super) struct SourceFold {
    pub memory: SourceMemory,
    pub target: RunProjectionTargetV1,
}

struct Fold {
    memory: SourceMemory,
    boundary: DecisionSourceBoundary,
    predecessor: DecisionProjectionPredecessor,
    status: RunStatus,
    failure: Option<RunFailureReason>,
    transitions: Vec<RunEvent>,
    steps: Vec<TimelineEntry>,
    publications: Vec<DecisionProjectionPublication>,
    turns: Vec<DecisionTurnTally>,
    approvals: Vec<(ApprovalId, AgentTurnId)>,
    now: i64,
}

impl SourceFold {
    pub(super) fn agent(
        memory: SourceMemory,
        boundary: DecisionSourceBoundary,
        predecessor: DecisionProjectionPredecessor,
        event: &AgentEvent,
        now: i64,
    ) -> Result<Self, DecisionError> {
        boundary.validate()?;
        if memory.attempt != boundary.fence.attempt
            || memory.generation != boundary.fence.generation
        {
            return Err(DecisionError::Invalid);
        }
        let mut fold = Fold {
            status: predecessor.status.clone(),
            failure: predecessor.failure_reason,
            turns: memory.turns.as_slice().to_vec(),
            approvals: memory.approvals.as_slice().to_vec(),
            memory,
            boundary,
            predecessor,
            transitions: Vec::new(),
            steps: Vec::new(),
            publications: Vec::new(),
            now,
        };
        // Usage is accepted after terminal status by its own amendment owner.
        // It must never be silently acknowledged by this non-usage fold.
        if matches!(event, AgentEvent::Usage { .. }) {
            return Err(DecisionError::Invalid);
        }
        if !fold.status.is_terminal() {
            fold.agent(event)?;
        }
        fold.finish()
    }
}

impl Fold {
    fn agent(&mut self, event: &AgentEvent) -> Result<(), DecisionError> {
        let run = self.boundary.fence.run;
        let attempt = self.memory.attempt;
        match event {
            AgentEvent::TurnStarted { turn_id } => {
                let originator = self.turn(*turn_id).originator;
                self.apply(RunEvent::TurnStarted { originator })?;
                self.step(
                    StepKind::TurnStarted { originator },
                    self.turn_refs(*turn_id),
                    RunFact::TurnStarted {
                        run,
                        attempt,
                        turn: *turn_id,
                        originator,
                    },
                );
            }
            AgentEvent::ToolCompleted {
                turn_id,
                succeeded,
                tool,
                failure,
                ..
            } => {
                let tally = self.turn(*turn_id);
                tally.tool_calls = tally.tool_calls.saturating_add(1);
                if !succeeded {
                    tally.tool_failures = tally.tool_failures.saturating_add(1);
                    tally.failure_breakdown.record(
                        tool.unwrap_or(ToolKind::Other),
                        failure.unwrap_or(ToolFailure::Unclassified),
                    );
                }
                if tally.tool_calls >= crate::orchestrator::events::TOOL_STEP_BOUND {
                    self.flush_tools(*turn_id);
                }
            }
            AgentEvent::ApprovalRequired {
                turn_id,
                approval_id,
                ..
            } => {
                self.approvals.retain(|(id, _)| id != approval_id);
                self.approvals.push((*approval_id, *turn_id));
                self.apply(RunEvent::ApprovalRequired)?;
                self.step(
                    StepKind::ApprovalRequired,
                    Self::approval_refs(*turn_id, *approval_id),
                    RunFact::ApprovalRequired {
                        run,
                        attempt,
                        turn: *turn_id,
                        approval: *approval_id,
                    },
                );
            }
            AgentEvent::ApprovalResolved {
                turn_id,
                approval_id,
                decision,
                ..
            } => {
                self.close_approval(
                    *turn_id,
                    *approval_id,
                    match decision {
                        ApprovalDecision::Approve => ApprovalOutcome::Approved,
                        ApprovalDecision::Reject => ApprovalOutcome::Rejected,
                    },
                )?;
            }
            AgentEvent::ApprovalExpired {
                turn_id,
                approval_id,
            } => {
                self.close_approval(*turn_id, *approval_id, ApprovalOutcome::Expired)?;
            }
            AgentEvent::WaitingForInput { turn_id } => {
                self.apply(RunEvent::WaitingForInput)?;
                self.step(
                    StepKind::WaitingForInput,
                    self.turn_refs(*turn_id),
                    RunFact::WaitingForInput {
                        run,
                        attempt,
                        turn: *turn_id,
                    },
                );
            }
            AgentEvent::AttachmentAccepted {
                turn_id,
                attachment_id,
            } => {
                self.step(
                    StepKind::AttachmentAccepted,
                    TimelineRefs::new([
                        TimelineRef::Turn { turn: *turn_id },
                        TimelineRef::Attachment {
                            attachment: *attachment_id,
                        },
                    ])
                    .expect("two refs"),
                    RunFact::AttachmentAccepted {
                        run,
                        attempt,
                        turn: *turn_id,
                    },
                );
            }
            AgentEvent::AttachmentRejected {
                turn_id,
                attachment_id,
                code,
            } => {
                self.step(
                    StepKind::AttachmentRejected,
                    TimelineRefs::new([
                        TimelineRef::Turn { turn: *turn_id },
                        TimelineRef::Attachment {
                            attachment: *attachment_id,
                        },
                    ])
                    .expect("two refs"),
                    RunFact::AttachmentRejected {
                        run,
                        attempt,
                        turn: *turn_id,
                        attachment: *attachment_id,
                        code: *code,
                    },
                );
            }
            AgentEvent::TurnCompleted { turn_id } => {
                self.close_turn(*turn_id)?;
                let originator = self.turn(*turn_id).originator;
                if self.memory.orchestrator_turn == Some(*turn_id) {
                    self.memory.terminal_turn = Some(*turn_id);
                }
                // Bot completion remains native evidence for the delivery
                // receipt owner. It does not complete the Run or invent artifacts.
                self.step(
                    StepKind::TurnCompleted {
                        originator,
                        artifacts_registered: 0,
                    },
                    self.turn_refs(*turn_id),
                    RunFact::TurnCompleted {
                        run,
                        attempt,
                        turn: *turn_id,
                        originator,
                        artifacts_registered: 0,
                    },
                );
                self.end_turn(*turn_id)?;
            }
            AgentEvent::TurnCancelled { turn_id, reason } => {
                self.close_turn(*turn_id)?;
                let originator = self.turn(*turn_id).originator;
                let reason = match reason {
                    koru_session_protocol::CancelReason::Run => CancelReason::Run,
                    koru_session_protocol::CancelReason::TakeoverBoundary => {
                        CancelReason::TakeoverBoundary
                    }
                };
                self.apply(RunEvent::TurnCancelled { originator, reason })?;
                self.step(
                    StepKind::TurnCancelled { originator, reason },
                    self.turn_refs(*turn_id),
                    RunFact::TurnCancelled {
                        run,
                        attempt,
                        turn: *turn_id,
                        originator,
                        reason,
                    },
                );
                if self.status == RunStatus::Cancelled {
                    self.step(
                        StepKind::Cancelled {
                            reason: CancelReason::Run,
                        },
                        TimelineRefs::new([TimelineRef::Attempt { attempt }]).expect("one ref"),
                        RunFact::Cancelled {
                            run,
                            attempt: Some(attempt),
                            generation: self.memory.generation,
                            reason: CancelReason::Run,
                        },
                    );
                }
                self.end_turn(*turn_id)?;
            }
            AgentEvent::TurnFailed { turn_id, error } => {
                self.close_turn(*turn_id)?;
                let originator = self.turn(*turn_id).originator;
                let stage = FailureStage::Turn;
                self.step(
                    StepKind::TurnFailed { originator, stage },
                    self.turn_refs(*turn_id),
                    RunFact::TurnFailed {
                        run,
                        attempt,
                        turn: *turn_id,
                        originator,
                        stage,
                    },
                );
                self.apply(RunEvent::TurnFailed { originator, stage })?;
                if matches!(self.status, RunStatus::Failed { .. }) {
                    self.failure = Some(RunFailureReason::from_code(error.code.as_str()));
                    self.step(
                        StepKind::Failed {
                            stage,
                            reason: self.failure,
                        },
                        TimelineRefs::new([TimelineRef::Attempt { attempt }]).expect("one ref"),
                        RunFact::Failed {
                            run,
                            attempt: Some(attempt),
                            generation: self.memory.generation,
                            stage,
                        },
                    );
                }
                self.end_turn(*turn_id)?;
            }
            AgentEvent::QuestionsRequired {
                turn_id,
                request_id,
                ..
            } => {
                let mut questions = self.memory.questions.as_slice().to_vec();
                questions.retain(|(id, _)| id != request_id);
                questions.push((*request_id, *turn_id));
                questions.sort_by_key(|(id, _)| *id);
                self.memory.questions = DecisionList::new(questions)?;
            }
            AgentEvent::QuestionsResolved { request_id, .. } => {
                self.memory.questions = DecisionList::new(
                    self.memory
                        .questions
                        .as_slice()
                        .iter()
                        .filter(|(id, _)| id != request_id)
                        .copied()
                        .collect(),
                )?;
            }
            // These need their own observed side-effect/amendment boundary.
            AgentEvent::Usage { .. } | AgentEvent::ChildSessionCreated { .. } => {
                return Err(DecisionError::Invalid);
            }
            AgentEvent::MessageDelta { .. }
            | AgentEvent::ToolStarted { .. }
            | AgentEvent::ProviderState { .. }
            | AgentEvent::ModeChanged { .. }
            | AgentEvent::Notice { .. }
            | AgentEvent::CommandsUpdated { .. }
            | AgentEvent::ContextUsage { .. } => {}
        }
        Ok(())
    }

    fn turn(&mut self, turn: AgentTurnId) -> &mut DecisionTurnTally {
        let index = match self.turns.iter().position(|tally| tally.turn == turn) {
            Some(index) => index,
            None => {
                self.turns.push(DecisionTurnTally {
                    turn,
                    originator: if self.memory.orchestrator_turn == Some(turn) {
                        TurnOriginator::Orchestrator
                    } else {
                        TurnOriginator::Human
                    },
                    tool_calls: 0,
                    tool_failures: 0,
                    failure_breakdown: ToolFailures::default(),
                });
                self.turns.len() - 1
            }
        };
        &mut self.turns[index]
    }

    fn apply(&mut self, event: RunEvent) -> Result<(), DecisionError> {
        if self.transitions.len() == 2 {
            return Err(DecisionError::Invalid);
        }
        self.status = transition_bot(&self.status, &event).map_err(|_| DecisionError::Invalid)?;
        self.transitions.push(event);
        self.failure = None;
        Ok(())
    }

    fn step(&mut self, kind: StepKind, refs: TimelineRefs, fact: RunFact) {
        let entry = TimelineEntry::new(kind, refs, self.now);
        self.publications.push(DecisionProjectionPublication {
            event: EventId::mint(),
            step: Some(entry.id),
            fact,
        });
        self.steps.push(entry);
    }

    fn turn_refs(&self, turn: AgentTurnId) -> TimelineRefs {
        TimelineRefs::new([
            TimelineRef::Attempt {
                attempt: self.memory.attempt,
            },
            TimelineRef::Turn { turn },
        ])
        .expect("two refs")
    }

    fn approval_refs(turn: AgentTurnId, approval: ApprovalId) -> TimelineRefs {
        TimelineRefs::new([
            TimelineRef::Turn { turn },
            TimelineRef::Approval { approval },
        ])
        .expect("two refs")
    }

    fn clear_attention(&mut self) -> Result<(), DecisionError> {
        if self.approvals.is_empty()
            && self
                .status
                .attention()
                .is_some_and(|attention| attention.contains(Attention::ApprovalRequired))
        {
            self.apply(RunEvent::ApprovalResolved)?;
        }
        Ok(())
    }

    fn close_approval(
        &mut self,
        turn: AgentTurnId,
        approval: ApprovalId,
        outcome: ApprovalOutcome,
    ) -> Result<(), DecisionError> {
        let Some(index) = self
            .approvals
            .iter()
            .position(|pair| *pair == (approval, turn))
        else {
            return Ok(());
        };
        self.approvals.remove(index);
        self.clear_attention()?;
        self.step(
            StepKind::ApprovalResolved { outcome },
            Self::approval_refs(turn, approval),
            RunFact::ApprovalResolved {
                run: self.boundary.fence.run,
                attempt: self.memory.attempt,
                turn,
                approval,
                outcome,
            },
        );
        Ok(())
    }

    fn close_turn(&mut self, turn: AgentTurnId) -> Result<(), DecisionError> {
        self.approvals.retain(|(_, pending)| *pending != turn);
        self.clear_attention()?;
        self.flush_tools(turn);
        Ok(())
    }

    fn end_turn(&mut self, turn: AgentTurnId) -> Result<(), DecisionError> {
        self.turns.retain(|tally| tally.turn != turn);
        self.approvals.retain(|(_, pending)| *pending != turn);
        self.memory.questions = DecisionList::new(
            self.memory
                .questions
                .as_slice()
                .iter()
                .filter(|(_, pending)| *pending != turn)
                .copied()
                .collect(),
        )?;
        if self.memory.orchestrator_turn == Some(turn) {
            self.memory.orchestrator_turn = None;
        }
        Ok(())
    }

    fn flush_tools(&mut self, turn: AgentTurnId) {
        let Some(tally) = self.turns.iter_mut().find(|tally| tally.turn == turn) else {
            return;
        };
        if tally.tool_calls == 0 {
            return;
        }
        let calls = tally.tool_calls;
        let failed = tally.tool_failures;
        let failures = tally.failure_breakdown;
        tally.tool_calls = 0;
        tally.tool_failures = 0;
        tally.failure_breakdown = ToolFailures::default();
        self.step(
            StepKind::ToolsCompleted {
                calls,
                failed,
                failures,
            },
            self.turn_refs(turn),
            RunFact::ToolsCompleted {
                run: self.boundary.fence.run,
                attempt: self.memory.attempt,
                turn,
                calls,
                failed,
            },
        );
    }

    fn finish(mut self) -> Result<SourceFold, DecisionError> {
        self.turns.sort_by_key(|tally| tally.turn);
        self.approvals.sort_by_key(|(id, _)| *id);
        self.memory.turns = DecisionList::new(self.turns)?;
        self.memory.approvals = DecisionList::new(self.approvals)?;
        encode_decision_value(&self.memory, DECISION_PAYLOAD_BYTES)?;
        let target = RunProjectionTargetV1::Source {
            version: DecisionVersionV1,
            predecessor: self.predecessor,
            transitions: DecisionList::new(self.transitions)?,
            bookkeeping: DecisionSourceBookkeeping {
                boundary: self.boundary,
                unflushed: self.memory.turns.clone(),
                usage: None,
                allocated_steps: DecisionList::new(
                    self.steps.iter().map(|step| step.id).collect(),
                )?,
            },
            effects: DecisionProjectionEffects {
                event: None,
                status: self.status,
                failure_reason: self.failure,
                economics_at_ms: None,
                steps: DecisionList::new(self.steps)?,
                models: DecisionList::new(Vec::new())?,
                publications: DecisionList::new(self.publications)?,
            },
        };
        target.validate()?;
        Ok(SourceFold {
            memory: self.memory,
            target,
        })
    }
}
