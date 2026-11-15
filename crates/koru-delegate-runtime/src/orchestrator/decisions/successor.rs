// SPDX-License-Identifier: MPL-2.0

//! The recorded successor's physical identity. A reserved creation whose
//! reply was lost is recovered by the exact creation key, never by a second
//! creation; working memory mirrors the retained consumer only after C has
//! recorded it, and never invents a turn before acknowledgment.

use super::*;
use crate::orchestrator::state::Attempt;
use koru_delegate_core::decisions::DecisionResumeState;
use koru_session_protocol::CreationLookup;

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
    /// at its generation on its bound session, holding the controller, with
    /// the acknowledged turn only once C recorded the acknowledgment.
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
            || row.provider_session.and_then(|key| key.session) != Some(session)
        {
            return Err(refused("bot_fenced"));
        }
        let turn = match &resume.state {
            DecisionResumeState::InputAcknowledged { acknowledgment }
            | DecisionResumeState::Closed {
                acknowledgment: Some(acknowledgment),
            } => Some(acknowledgment.turn),
            _ => None,
        };
        let state = self.open(run)?;
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
        successor.run_workspace = run_workspace;
        successor.captured = captured;
        state.attempt = Some(successor);
        Ok(())
    }
}
