// SPDX-License-Identifier: MPL-2.0

//! The recorded successor's physical identity. A reserved creation whose
//! reply was lost is recovered by the exact creation key, never by a second
//! creation; working memory mirrors the retained consumer only after C has
//! recorded it, and never invents a turn before acknowledgment.

use super::*;
use crate::orchestrator::state::Attempt;
use koru_delegate_core::decisions::DecisionResumeState;
use koru_delegate_core::{AgentTurnId, AttemptGeneration, RunAttemptId, SessionId};
use koru_session_protocol::CreationLookup;

/// The acknowledged continuation turn a retained resume carries, if any.
pub(super) fn acknowledged_turn(
    resume: &koru_delegate_core::decisions::DecisionResume,
) -> Option<AgentTurnId> {
    match &resume.state {
        DecisionResumeState::InputAcknowledged { acknowledgment }
        | DecisionResumeState::Closed {
            acknowledgment: Some(acknowledgment),
        } => Some(acknowledgment.turn),
        _ => None,
    }
}

impl Orchestrator {
    /// Recover a reserved successor creation by its exact key. A known
    /// session is bound through C; Creating or no evidence leaves the
    /// reservation as it is (retry later, or the cleanup owner decides).
    pub fn bot_recover_decision_successor(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<DecisionRecord>, RunError> {
        let execution = self.decision_ticket(owner, run)?;
        let resume = execution
            .resume
            .clone()
            .ok_or_else(|| refused("bot_fenced"))?;
        if !matches!(resume.state, DecisionResumeState::Prepared {}) {
            return Ok(None);
        }
        let (_, _, _, creation) = resume.consumer_identity();
        let (host, lookup) = self.ports.sessions.lookup(creation)?;
        let session = match lookup {
            CreationLookup::Known { session } => session.id,
            // Ended before binding, still creating, or no evidence: never a
            // fabricated live consumer.
            CreationLookup::KnownEnded { .. }
            | CreationLookup::Creating
            | CreationLookup::NoEvidence => return Ok(None),
        };
        self.bot_bind_decision_successor(owner, run, creation, host, session)
            .map(Some)
    }

    /// Mirror the retained successor into working memory: the new attempt
    /// at its generation on its bound session, holding the controller only
    /// after the host granted it, with the acknowledged turn only once C
    /// recorded the acknowledgment. Re-running it for the same consumer keeps
    /// what working memory already knows (turn tallies, an ended turn) and
    /// adds only the newly recorded acknowledgment.
    pub fn bot_install_decision_successor(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<(), RunError> {
        let execution = self.decision_ticket(owner, run)?;
        let resume = execution
            .resume
            .clone()
            .ok_or_else(|| refused("bot_fenced"))?;
        let (attempt, generation, session, _) = resume.consumer_identity();
        let session = session.ok_or_else(|| refused("bot_successor_unbound"))?;
        let row = &execution.current_attempt;
        if row.attempt != attempt
            || row.generation != generation
            || row.retired
            || row.phase.is_terminal()
            || row.provider_session.and_then(|key| key.session) != Some(session)
        {
            return Err(refused("bot_fenced"));
        }
        let turn = acknowledged_turn(&resume);
        let (same, holds) = self
            .runs
            .get(&run)
            .and_then(|state| state.attempt.as_ref())
            .filter(|memory| {
                memory.id == attempt
                    && memory.generation == generation
                    && memory.session == Some(session)
            })
            .map_or((false, false), |memory| (true, memory.holds_lease));
        if !same {
            // A consumer recovered by lookup (or after a restart) takes the
            // Run's folder claim here; a launch made here already moved it.
            let admission = self
                .bot_ledger()?
                .bot_admission(run)?
                .ok_or(RunError::UnknownRun)?;
            let submission = admission.submission;
            self.decision_transfer_claim(
                run,
                execution.wait.origin.attempt,
                attempt,
                submission.provenance.bot,
                &submission.request.scope,
            )?;
        }
        if !holds {
            // The controller is positive evidence only from the host's grant.
            self.ports.sessions.attach(session)?;
        }
        let state = self.open(run)?;
        state.decision_launch = None;
        if same {
            let memory = state.attempt.as_mut().expect("matched above");
            memory.holds_lease = true;
            if let Some(turn) = turn
                && memory.resume_turn.is_none()
            {
                memory.resume_turn = Some(turn);
                if memory.orchestrator_turn.is_none() && memory.bot_terminal_turn != Some(turn) {
                    memory.orchestrator_turn = Some(turn);
                }
            }
            return Ok(());
        }
        let (run_workspace, captured, is_bot) = match state.attempt.take() {
            Some(old) => {
                if old.id != attempt {
                    state.last_attempt = Some((old.id, old.generation));
                }
                (old.run_workspace, old.captured, old.is_bot)
            }
            None => (Default::default(), Default::default(), true),
        };
        let mut successor = Attempt::new(attempt, generation);
        successor.session = Some(session);
        successor.is_bot = is_bot;
        successor.holds_lease = true;
        successor.bot_observed = true;
        successor.orchestrator_turn = turn;
        successor.resume_turn = turn;
        successor.run_workspace = run_workspace;
        successor.captured = captured;
        state.attempt = Some(successor);
        Ok(())
    }

    /// The durably acknowledged continuation turn of this exact recorded
    /// successor (attempt, generation, bound session), read from C even after
    /// the wait settled or the process restarted. `None` is "no positive
    /// evidence", never permission: the original attempt, another consumer,
    /// or an input that was never acknowledged.
    pub(in crate::orchestrator) fn decision_resume_turn(
        &mut self,
        run: RunId,
        attempt: RunAttemptId,
        generation: AttemptGeneration,
        session: SessionId,
    ) -> Result<Option<AgentTurnId>, RunError> {
        let Some(admission) = self.bot_ledger()?.bot_admission(run)? else {
            return Ok(None);
        };
        let owner = admission.submission.provenance.owner;
        let Some(continuation) = self
            .bot_ledger()?
            .decision_acknowledged_continuation(owner, run)?
        else {
            return Ok(None);
        };
        Ok((continuation.attempt == attempt
            && continuation.generation == generation
            && continuation.session == session)
            .then_some(continuation.turn))
    }
}
