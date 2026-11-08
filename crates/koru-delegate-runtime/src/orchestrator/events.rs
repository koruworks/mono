// SPDX-License-Identifier: MPL-2.0

//! §7.5: which provider event means what to the run, and §7.4's handover
//! as the session reports it.
//!
//! One update in, one row of the table applied: the status moves through
//! `transition`, the lean step is appended, the fact is published. Three
//! rules the table states and this file keeps:
//!
//! - **A message delta is never an event and never a write.** Tool
//!   completions are tallied per turn and written as one bounded
//!   `ToolsCompleted` step at the turn's end (or at [`TOOL_STEP_BOUND`]).
//! - **Only an orchestrator turn can complete the objective.** A turn is
//!   the orchestrator's when it submitted it; any other turn is a person's,
//!   recorded and never the run's result.
//! - **An identified approval is a cooperative boundary.** A requested
//!   handover can transfer the lease while the original turn stays active;
//!   returning that lease does not submit the objective again (ADR-389).
//! - **A pending takeover fences completion.** The transition function
//!   holds the run under `HandoverPending { objective_satisfied }`; this
//!   file releases the lease at the turn boundary and reacquires it when
//!   the person returns, completing then if the objective was satisfied.

use koru_delegate_core::{
    AgentTurnId, ApprovalOutcome, CancelReason, FailureStage, PriorAttempt, RunEvent, RunId,
    RunStatus, StepKind, TimelineRef, TimelineRefs, ToolFailure, ToolFailures, ToolKind,
};
use koru_delegate_sqlite::AttemptPhase;
use koru_session_protocol::{AgentEvent, ApprovalDecision, HandoverState, SessionEvent};

use crate::{HandleOutcome, RunFact, SessionUpdate, orchestrator::state::TurnState};

use super::{Orchestrator, RunError};

/// How many tool completions one turn coalesces before a step is written
/// even though the turn has not ended: bounded, so a thousand-call turn is
/// a handful of steps and never a thousand.
pub const TOOL_STEP_BOUND: u32 = 64;

impl Orchestrator {
    /// Apply one update from the run's session.
    pub fn handle(&mut self, run: RunId, update: SessionUpdate) -> Result<HandleOutcome, RunError> {
        match update {
            SessionUpdate::Agent(event) => self.agent_event(run, event.event),
            SessionUpdate::AgentGap { .. } => {
                if let Some(attempt) = self.open(run)?.attempt.as_mut() {
                    attempt.approval_evidence_complete = false;
                }
                let Some((attempt, _)) = self.open(run)?.attempt_ids() else {
                    return Ok(HandleOutcome::Ended);
                };
                self.step(
                    run,
                    StepKind::Gap { events_lost: None },
                    TimelineRefs::new([TimelineRef::Attempt { attempt }]).expect("one ref"),
                    Some(RunFact::Gap {
                        run,
                        attempt,
                        events_lost: None,
                    }),
                )?;
                Ok(HandleOutcome::Continue)
            }
            SessionUpdate::Session(event) => self.session_event(run, event.event),
            SessionUpdate::Closed { .. } => self.session_lost(run),
        }
    }

    fn agent_event(&mut self, run: RunId, event: AgentEvent) -> Result<HandleOutcome, RunError> {
        let status = self.status(run)?;
        // A late usage report still applies after the run ended (§17.2);
        // nothing else from a finished attempt touches the run.
        if status.is_terminal() || self.open(run)?.attempt.is_none() {
            if let AgentEvent::Usage { turn_id, usage } = event {
                self.record_usage(run, turn_id, usage)?;
            }
            return Ok(HandleOutcome::Ended);
        }
        if let AgentEvent::TurnCompleted { turn_id }
        | AgentEvent::TurnCancelled { turn_id, .. }
        | AgentEvent::TurnFailed { turn_id, .. } = &event
            && self.attempt_mut(run)?.recovered_unowned_turn == Some(*turn_id)
        {
            // Recovery proved activity but not an original orchestrator turn.
            // Preserve artifacts and require an explicit resume instead of
            // attributing this terminal or duplicating an unproved objective.
            return self.session_lost(run);
        }
        if let AgentEvent::TurnStarted { turn_id } = &event
            && self.attempt_mut(run)?.recovered_unowned_turn == Some(*turn_id)
        {
            // A replay cannot establish who submitted a recovered turn.
            return Ok(HandleOutcome::Continue);
        }
        let attempt_id = self.open(run)?.attempt.as_ref().expect("checked").id;
        let turn_refs = |turn: AgentTurnId| {
            TimelineRefs::new([
                TimelineRef::Attempt {
                    attempt: attempt_id,
                },
                TimelineRef::Turn { turn },
            ])
            .expect("two refs")
        };
        match event {
            AgentEvent::TurnStarted { turn_id } => {
                let originator = {
                    let attempt = self.attempt_mut(run)?;
                    let originator = attempt.originator_of(turn_id);
                    attempt
                        .turns
                        .entry(turn_id)
                        .or_insert_with(|| TurnState::new(originator));
                    originator
                };
                self.apply(run, &RunEvent::TurnStarted { originator })?;
                self.step(
                    run,
                    StepKind::TurnStarted { originator },
                    turn_refs(turn_id),
                    Some(RunFact::TurnStarted {
                        run,
                        attempt: attempt_id,
                        turn: turn_id,
                        originator,
                    }),
                )?;
            }
            // Never an event, never a write.
            AgentEvent::MessageDelta { .. } | AgentEvent::ToolStarted { .. } => {}
            AgentEvent::ToolCompleted {
                turn_id,
                succeeded,
                tool,
                failure,
                ..
            } => {
                let flush = {
                    let attempt = self.attempt_mut(run)?;
                    let originator = attempt.originator_of(turn_id);
                    let tally = attempt
                        .turns
                        .entry(turn_id)
                        .or_insert_with(|| TurnState::new(originator));
                    tally.tool_calls = tally.tool_calls.saturating_add(1);
                    if !succeeded {
                        tally.tool_failures = tally.tool_failures.saturating_add(1);
                        // **Nothing is dropped and nothing is guessed.** An
                        // adapter that named neither the tool nor the class
                        // — an older peer, or a provider whose answer it
                        // could not place — is recorded as the unknown it
                        // is, so the breakdown always accounts for every
                        // failure the count claims.
                        tally.failure_breakdown.record(
                            tool.unwrap_or(ToolKind::Other),
                            failure.unwrap_or(ToolFailure::Unclassified),
                        );
                    }
                    tally.tool_calls >= TOOL_STEP_BOUND
                };
                if flush {
                    self.flush_tools(run, turn_id)?;
                }
            }
            AgentEvent::ApprovalRequired {
                turn_id,
                approval_id,
                ..
            } => {
                self.attempt_mut(run)?
                    .pending_approvals
                    .insert(approval_id, turn_id);
                self.apply(run, &RunEvent::ApprovalRequired)?;
                self.step(
                    run,
                    StepKind::ApprovalRequired,
                    TimelineRefs::new([
                        TimelineRef::Turn { turn: turn_id },
                        TimelineRef::Approval {
                            approval: approval_id,
                        },
                    ])
                    .expect("two refs"),
                    Some(RunFact::ApprovalRequired {
                        run,
                        attempt: attempt_id,
                        turn: turn_id,
                        approval: approval_id,
                    }),
                )?;
                // The request may have arrived before this permission boundary.
                self.release_at_approval(run)?;
            }
            AgentEvent::ApprovalResolved {
                turn_id,
                approval_id,
                decision,
                ..
            } => {
                let outcome = match decision {
                    ApprovalDecision::Approve => ApprovalOutcome::Approved,
                    ApprovalDecision::Reject => ApprovalOutcome::Rejected,
                };
                self.approval_closed(run, turn_id, approval_id, outcome)?;
            }
            AgentEvent::ApprovalExpired {
                turn_id,
                approval_id,
            } => {
                self.approval_closed(run, turn_id, approval_id, ApprovalOutcome::Expired)?;
            }
            AgentEvent::WaitingForInput { turn_id } => {
                self.apply(run, &RunEvent::WaitingForInput)?;
                self.step(
                    run,
                    StepKind::WaitingForInput,
                    turn_refs(turn_id),
                    Some(RunFact::WaitingForInput {
                        run,
                        attempt: attempt_id,
                        turn: turn_id,
                    }),
                )?;
            }
            AgentEvent::AttachmentAccepted {
                turn_id,
                attachment_id,
            } => {
                self.step(
                    run,
                    StepKind::AttachmentAccepted,
                    TimelineRefs::new([
                        TimelineRef::Turn { turn: turn_id },
                        TimelineRef::Attachment {
                            attachment: attachment_id,
                        },
                    ])
                    .expect("two refs"),
                    Some(RunFact::AttachmentAccepted {
                        run,
                        attempt: attempt_id,
                        turn: turn_id,
                    }),
                )?;
            }
            AgentEvent::AttachmentRejected {
                turn_id,
                attachment_id,
                code,
            } => {
                self.step(
                    run,
                    StepKind::AttachmentRejected,
                    TimelineRefs::new([
                        TimelineRef::Turn { turn: turn_id },
                        TimelineRef::Attachment {
                            attachment: attachment_id,
                        },
                    ])
                    .expect("two refs"),
                    Some(RunFact::AttachmentRejected {
                        run,
                        attempt: attempt_id,
                        turn: turn_id,
                        attachment: attachment_id,
                        code,
                    }),
                )?;
            }
            AgentEvent::Usage { turn_id, usage } => {
                self.record_usage(run, turn_id, usage)?;
            }
            AgentEvent::ChildSessionCreated {
                turn_id,
                child_session_id,
            } => {
                self.apply(run, &RunEvent::ChildSessionDetected)?;
                self.step(
                    run,
                    StepKind::ChildSessionDetected,
                    TimelineRefs::new([
                        TimelineRef::Turn { turn: turn_id },
                        TimelineRef::Session {
                            session: child_session_id,
                        },
                    ])
                    .expect("two refs"),
                    None,
                )?;
                // §7.3: no unmanaged hierarchy. The marker clears when the
                // termination is observed to have been asked. The step is
                // committed above (§17.1) and the fact follows the answer,
                // because the row says whether the child was ended.
                let terminated = match self.ports.sessions.terminate(child_session_id) {
                    Ok(()) => {
                        self.apply(run, &RunEvent::ChildSessionTerminated)?;
                        true
                    }
                    Err(error) => {
                        self.trace(
                            run,
                            "child_terminate_failed",
                            format_args!("{}", error.code),
                        );
                        false
                    }
                };
                self.publish(
                    run,
                    RunFact::ChildSessionDetected {
                        run,
                        attempt: attempt_id,
                        turn: turn_id,
                        child: child_session_id,
                        terminated,
                    },
                )?;
            }
            AgentEvent::TurnCompleted { turn_id } => {
                self.close_turn_approvals(run, turn_id)?;
                self.flush_tools(run, turn_id)?;
                let originator = self.attempt_mut(run)?.originator_of(turn_id);
                let registered =
                    self.ports
                        .registrations
                        .registered(run, attempt_id, Some(turn_id));
                let now = self.ports.clock.now_ms();
                for mut reference in registered.clone() {
                    if reference.registered_at_ms == 0 {
                        reference.registered_at_ms = now;
                    }
                    let state = self.open(run)?;
                    // A version the document already carries is one this
                    // device recorded before it was restarted: the port
                    // answers what is new to *it*, and after a restart
                    // everything is. The document is the record, so it
                    // decides.
                    if state
                        .doc
                        .document()
                        .header()?
                        .artifacts
                        .iter()
                        .any(|known| known.artifact == reference.artifact)
                    {
                        continue;
                    }
                    state
                        .doc
                        .commit(&mut |document| document.register_result(&reference))?;
                    self.step(
                        run,
                        StepKind::ArtifactRegistered,
                        TimelineRefs::new([
                            TimelineRef::Turn { turn: turn_id },
                            TimelineRef::Artifact {
                                artifact: reference.artifact,
                            },
                        ])
                        .expect("two refs"),
                        Some(RunFact::ArtifactRegistered {
                            run,
                            attempt: attempt_id,
                            artifact: reference.artifact,
                        }),
                    )?;
                }
                let artifacts_registered = u32::try_from(registered.len()).unwrap_or(u32::MAX);
                let status = self.apply(
                    run,
                    &RunEvent::TurnCompleted {
                        originator,
                        artifacts_registered,
                    },
                )?;
                self.step(
                    run,
                    StepKind::TurnCompleted {
                        originator,
                        artifacts_registered,
                    },
                    turn_refs(turn_id),
                    Some(RunFact::TurnCompleted {
                        run,
                        attempt: attempt_id,
                        turn: turn_id,
                        originator,
                        artifacts_registered,
                    }),
                )?;
                return self.turn_boundary(run, turn_id, status);
            }
            AgentEvent::TurnCancelled { turn_id, reason } => {
                self.close_turn_approvals(run, turn_id)?;
                self.flush_tools(run, turn_id)?;
                let originator = self.attempt_mut(run)?.originator_of(turn_id);
                let reason = match reason {
                    koru_session_protocol::CancelReason::Run => CancelReason::Run,
                    koru_session_protocol::CancelReason::TakeoverBoundary => {
                        CancelReason::TakeoverBoundary
                    }
                };
                let status = self.apply(run, &RunEvent::TurnCancelled { originator, reason })?;
                self.step(
                    run,
                    StepKind::TurnCancelled { originator, reason },
                    turn_refs(turn_id),
                    Some(RunFact::TurnCancelled {
                        run,
                        attempt: attempt_id,
                        turn: turn_id,
                        originator,
                        reason,
                    }),
                )?;
                return self.turn_boundary(run, turn_id, status);
            }
            AgentEvent::TurnFailed { turn_id, error } => {
                self.close_turn_approvals(run, turn_id)?;
                self.flush_tools(run, turn_id)?;
                let originator = self.attempt_mut(run)?.originator_of(turn_id);
                let stage = FailureStage::Turn;
                let reason = Some(koru_delegate_core::RunFailureReason::from_code(
                    error.code.as_str(),
                ));
                self.step(
                    run,
                    StepKind::TurnFailed { originator, stage },
                    turn_refs(turn_id),
                    None,
                )?;
                let status = self.apply_with_failure_reason(
                    run,
                    &RunEvent::TurnFailed { originator, stage },
                    reason,
                )?;
                self.publish(
                    run,
                    RunFact::TurnFailed {
                        run,
                        attempt: attempt_id,
                        turn: turn_id,
                        originator,
                        stage,
                    },
                )?;
                return self.turn_boundary(run, turn_id, status);
            }
            // Mirrored by the session summary; nothing for the run.
            AgentEvent::ProviderState { .. } => {}
            // A chat's approval mode (koru-ai D7); a delegated run sets none.
            AgentEvent::ModeChanged { .. } => {}
            // An agent's status line (koru-ai D69): the chat shows it; a run
            // keeps its reply and its steps, and nothing else.
            AgentEvent::Notice { .. } => {}
            // An agent's questions (koru-ai D108): a run's sessions are not
            // created to draw them, so its agent declines and none arrive.
            AgentEvent::QuestionsRequired { .. } | AgentEvent::QuestionsResolved { .. } => {}
            // Command listings belong to the interactive chat catalog.
            AgentEvent::CommandsUpdated { .. } => {}
        }
        Ok(HandleOutcome::Continue)
    }

    fn approval_closed(
        &mut self,
        run: RunId,
        turn: AgentTurnId,
        approval: koru_delegate_core::ApprovalId,
        outcome: ApprovalOutcome,
    ) -> Result<(), RunError> {
        let (attempt_id, last_known) = {
            let attempt = self.attempt_mut(run)?;
            let known = attempt.pending_approvals.get(&approval) == Some(&turn);
            if !known {
                // A duplicate, old-turn, or unidentifiable reply cannot close
                // the current turn's permission or write a second resolution.
                return Ok(());
            }
            attempt.pending_approvals.remove(&approval);
            (
                attempt.id,
                known && attempt.pending_approvals.is_empty() && attempt.approval_evidence_complete,
            )
        };
        if last_known
            && self.status(run)?.attention().is_some_and(|attention| {
                attention.contains(koru_delegate_core::Attention::ApprovalRequired)
            })
        {
            self.apply(run, &RunEvent::ApprovalResolved)?;
        }
        self.step(
            run,
            StepKind::ApprovalResolved { outcome },
            TimelineRefs::new([
                TimelineRef::Turn { turn },
                TimelineRef::Approval { approval },
            ])
            .expect("two refs"),
            Some(RunFact::ApprovalResolved {
                run,
                attempt: attempt_id,
                turn,
                approval,
                outcome,
            }),
        )
    }

    /// Write the turn's coalesced tool completions as one step, if any.
    fn flush_tools(&mut self, run: RunId, turn: AgentTurnId) -> Result<(), RunError> {
        let attempt = self.attempt_mut(run)?;
        let attempt_id = attempt.id;
        let Some(tally) = attempt.turns.get_mut(&turn) else {
            return Ok(());
        };
        if tally.tool_calls == 0 {
            return Ok(());
        }
        let (calls, failed) = (tally.tool_calls, tally.tool_failures);
        let failures = tally.failure_breakdown;
        tally.tool_calls = 0;
        tally.tool_failures = 0;
        tally.failure_breakdown = ToolFailures::default();
        debug_assert_eq!(failures.total(), failed, "the breakdown must add up");
        self.step(
            run,
            StepKind::ToolsCompleted {
                calls,
                failed,
                failures,
            },
            TimelineRefs::new([
                TimelineRef::Attempt {
                    attempt: attempt_id,
                },
                TimelineRef::Turn { turn },
            ])
            .expect("two refs"),
            Some(RunFact::ToolsCompleted {
                run,
                attempt: attempt_id,
                turn,
                calls,
                failed,
            }),
        )
    }

    /// The authoritative terminal of our single active turn closes its
    /// permissions even when recovery lost earlier resolution events.
    fn close_turn_approvals(&mut self, run: RunId, turn: AgentTurnId) -> Result<(), RunError> {
        let clear = {
            let attempt = self.attempt_mut(run)?;
            let own_terminal = attempt.orchestrator_turn == Some(turn);
            attempt
                .pending_approvals
                .retain(|_, pending| *pending != turn);
            if own_terminal && attempt.pending_approvals.is_empty() {
                attempt.approval_evidence_complete = true;
            }
            attempt.pending_approvals.is_empty() && attempt.approval_evidence_complete
        };
        if clear
            && self.status(run)?.attention().is_some_and(|attention| {
                attention.contains(koru_delegate_core::Attention::ApprovalRequired)
            })
        {
            self.apply(run, &RunEvent::ApprovalResolved)?;
        }
        Ok(())
    }

    /// A turn ended. If the run is terminal, the attempt ends with it; if a
    /// handover was waiting for this boundary, the lease is released now
    /// (§7.4); otherwise the run keeps running.
    fn turn_boundary(
        &mut self,
        run: RunId,
        turn: AgentTurnId,
        status: RunStatus,
    ) -> Result<HandleOutcome, RunError> {
        let ours = {
            let attempt = self.attempt_mut(run)?;
            attempt.turns.remove(&turn);
            attempt
                .pending_approvals
                .retain(|_, pending| *pending != turn);
            let ours = attempt.orchestrator_turn == Some(turn);
            if ours {
                attempt.orchestrator_turn = None;
            }
            ours
        };
        if status.is_terminal() {
            return self.terminal(run, &status);
        }
        let (release, session) = {
            let attempt = self.attempt_mut(run)?;
            (attempt.release_owed && ours, attempt.session)
        };
        if release && let Some(session) = session {
            self.ports.sessions.release_control(session)?;
            let attempt = self.attempt_mut(run)?;
            attempt.release_owed = false;
            attempt.holds_lease = false;
            self.trace(
                run,
                "control_released",
                format_args!("at the turn boundary"),
            );
        }
        Ok(HandleOutcome::Continue)
    }

    /// The run reached a terminal status: record the terminal step and end
    /// the attempt.
    pub(crate) fn terminal(
        &mut self,
        run: RunId,
        status: &RunStatus,
    ) -> Result<HandleOutcome, RunError> {
        let (attempt_id, generation) = {
            let attempt = self.attempt_mut(run)?;
            (attempt.id, attempt.generation)
        };
        let refs = TimelineRefs::new([TimelineRef::Attempt {
            attempt: attempt_id,
        }])
        .expect("one ref");
        let artifacts = u32::try_from(self.open(run)?.doc.document().header()?.artifacts.len())
            .unwrap_or(u32::MAX);
        let (kind, fact) = match status {
            RunStatus::Completed => (
                StepKind::Completed,
                RunFact::Completed {
                    run,
                    attempt: attempt_id,
                    generation,
                    artifacts,
                },
            ),
            RunStatus::Failed { stage } => (
                StepKind::Failed {
                    stage: *stage,
                    reason: self.open(run)?.doc.document().header()?.failure_reason,
                },
                RunFact::Failed {
                    run,
                    attempt: Some(attempt_id),
                    generation,
                    stage: *stage,
                },
            ),
            RunStatus::Cancelled => (
                StepKind::Cancelled {
                    reason: CancelReason::Run,
                },
                RunFact::Cancelled {
                    run,
                    attempt: Some(attempt_id),
                    generation,
                    reason: CancelReason::Run,
                },
            ),
            _ => return Ok(HandleOutcome::Continue),
        };
        if matches!(status, RunStatus::Failed { .. }) {
            // The Failed step was retained with status and reason by apply.
            self.publish(run, fact)?;
        } else {
            self.step(run, kind, refs, Some(fact))?;
        }
        let now = self.ports.clock.now_ms();
        let state = self.open(run)?;
        state
            .doc
            .commit(&mut |document| document.refresh_economics(now).map(|_| ()))?;
        self.end_attempt(run, AttemptPhase::Ended, true)?;
        Ok(HandleOutcome::Ended)
    }

    fn session_event(
        &mut self,
        run: RunId,
        event: SessionEvent,
    ) -> Result<HandleOutcome, RunError> {
        if self.status(run)?.is_terminal() || self.open(run)?.attempt.is_none() {
            return Ok(HandleOutcome::Ended);
        }
        match event {
            SessionEvent::HandoverChanged { handover } => self.handover(run, handover),
            SessionEvent::Exited { .. } | SessionEvent::Failed { .. } => self.session_lost(run),
            _ => Ok(HandleOutcome::Continue),
        }
    }

    /// §7.4, as the session reports it. Release, not preemption: the
    /// orchestrator gives the lease up at a boundary of its choosing,
    /// records the takeover only when the person holds the lease, and the
    /// return only when it reacquired the controller.
    fn handover(&mut self, run: RunId, handover: HandoverState) -> Result<HandleOutcome, RunError> {
        let (attempt_id, session, active_turn) = {
            let attempt = self.attempt_mut(run)?;
            (attempt.id, attempt.session, attempt.orchestrator_turn)
        };
        let Some(session) = session else {
            return Ok(HandleOutcome::Continue);
        };
        match handover {
            HandoverState::Requested => {
                if self.status(run)?.control()
                    != Some(koru_delegate_core::ControlState::Orchestrator)
                {
                    return Ok(HandleOutcome::Continue);
                }
                self.apply(run, &RunEvent::HandoverRequested)?;
                if active_turn.is_some() {
                    self.attempt_mut(run)?.release_owed = true;
                    self.release_at_approval(run)?;
                } else {
                    self.ports.sessions.release_control(session)?;
                    let attempt = self.attempt_mut(run)?;
                    attempt.holds_lease = false;
                    attempt.release_owed = false;
                }
            }
            HandoverState::Taken => {
                if matches!(
                    self.status(run)?.control(),
                    Some(koru_delegate_core::ControlState::Human { .. })
                ) {
                    return Ok(HandleOutcome::Continue);
                }
                self.apply(run, &RunEvent::ControlTaken)?;
                let attempt = self.attempt_mut(run)?;
                attempt.holds_lease = false;
                attempt.release_owed = false;
                self.step(
                    run,
                    StepKind::ControlTaken { discovered: false },
                    TimelineRefs::new([TimelineRef::Attempt {
                        attempt: attempt_id,
                    }])
                    .expect("one ref"),
                    Some(RunFact::ControlTaken {
                        run,
                        attempt: attempt_id,
                        discovered: false,
                    }),
                )?;
            }
            HandoverState::Returned => {
                if !matches!(
                    self.status(run)?.control(),
                    Some(koru_delegate_core::ControlState::Human { .. })
                ) {
                    return Ok(HandleOutcome::Continue);
                }
                if let Err(error) = self.ports.sessions.attach(session) {
                    // Failed attachment: nothing is recorded, the host
                    // re-attaches after a bounded wait (§7.4).
                    self.trace(run, "reattach_failed", format_args!("{}", error.code));
                    return Ok(HandleOutcome::Continue);
                }
                self.attempt_mut(run)?.holds_lease = true;
                let status = self.apply(run, &RunEvent::ControlReturned)?;
                self.step(
                    run,
                    StepKind::ControlReturned,
                    TimelineRefs::new([TimelineRef::Attempt {
                        attempt: attempt_id,
                    }])
                    .expect("one ref"),
                    Some(RunFact::ControlReturned {
                        run,
                        attempt: attempt_id,
                    }),
                )?;
                if status.is_terminal() {
                    return self.terminal(run, &status);
                }
                if active_turn.is_some() || self.attempt_mut(run)?.recovered_unowned_turn.is_some()
                {
                    // A person answered or inspected a paused approval. The
                    // original turn continues; submitting again would duplicate it.
                    return Ok(HandleOutcome::Continue);
                }
                // The objective was not satisfied at the fenced boundary:
                // the orchestrator resumes with the next turn.
                let objective = self.open(run)?.doc.document().header()?.objective;
                let turn = self.ports.sessions.submit(
                    session,
                    format!("Continue with the objective.\n\n{}", objective.as_str()),
                )?;
                self.attempt_mut(run)?.orchestrator_turn = Some(turn);
            }
            HandoverState::Idle | HandoverState::Released => {}
        }
        Ok(HandleOutcome::Continue)
    }

    /// Transfer only the existing lease, never cancel or relabel the paused turn.
    pub(crate) fn release_at_approval(&mut self, run: RunId) -> Result<(), RunError> {
        let session = {
            let attempt = self.attempt_mut(run)?;
            if !(attempt.holds_lease && attempt.release_owed && attempt.can_release_for_approval())
            {
                return Ok(());
            }
            attempt.session
        };
        if let Some(session) = session {
            self.ports.sessions.release_control(session)?;
            let attempt = self.attempt_mut(run)?;
            attempt.holds_lease = false;
            attempt.release_owed = false;
        }
        Ok(())
    }

    /// The provider is gone — its process exited, or the stream ended —
    /// with the run not finished. A restart, or the provider's own end,
    /// ends the attempt: `Interrupted`, never `Failed` (D10).
    pub(crate) fn session_lost(&mut self, run: RunId) -> Result<HandleOutcome, RunError> {
        self.session_lost_as(run, AttemptPhase::Interrupted)
    }

    /// [`session_lost`](Self::session_lost) with the phase the ledger
    /// records: `Interrupted` when nothing proved the session's end,
    /// `Ended` when the host retained it (§7.6 *known-ended*).
    pub(crate) fn session_lost_as(
        &mut self,
        run: RunId,
        phase: AttemptPhase,
    ) -> Result<HandleOutcome, RunError> {
        let status = self.status(run)?;
        if status.is_terminal() {
            return Ok(HandleOutcome::Ended);
        }
        let Some((attempt_id, generation)) = self.open(run)?.attempt_ids() else {
            return Ok(HandleOutcome::Ended);
        };
        if self.open(run)?.attempt.is_none() {
            return Ok(HandleOutcome::Ended);
        }
        let prior_attempt = PriorAttempt::Ended;
        self.apply(run, &RunEvent::Interrupt { prior_attempt })?;
        self.step(
            run,
            StepKind::Interrupted { prior_attempt },
            TimelineRefs::new([TimelineRef::Attempt {
                attempt: attempt_id,
            }])
            .expect("one ref"),
            Some(RunFact::Interrupted {
                run,
                attempt: attempt_id,
                generation,
                prior_attempt,
            }),
        )?;
        self.end_attempt(run, phase, false)?;
        Ok(HandleOutcome::Ended)
    }

    pub(crate) fn attempt_mut(
        &mut self,
        run: RunId,
    ) -> Result<&mut crate::orchestrator::state::Attempt, RunError> {
        self.open(run)?
            .attempt
            .as_mut()
            .ok_or(RunError::WrongStatus { status: "idle" })
    }
}
