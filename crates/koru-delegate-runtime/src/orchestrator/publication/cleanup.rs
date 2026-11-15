// SPDX-License-Identifier: MPL-2.0

//! Physical cleanup uses the retained current consumer, independently of Run
//! document availability. The canonical terminal projection remains owed.

use super::*;
use koru_delegate_core::decisions::{
    BotWaitPhase, ObservedCreationRetirement, ObservedRetirementResult,
};
use koru_session_protocol::{CreationLookup, ProcessState};

impl Orchestrator {
    /// This observation is not a released slot or a settled admission. It is
    /// available only after the sole writer retained CleanupPending; neither
    /// detached RunState.attempt=None nor an old retired attempt proves absence.
    pub fn observe_decision_cleanup(
        &mut self,
        run: RunId,
    ) -> Result<Option<ObservedCreationRetirement>, RunError> {
        let owner = self.publication_owner(run)?;
        let Some(execution) = self.bot_ledger()?.decision_wait_execution(owner, run)? else {
            return Ok(None);
        };
        if execution.wait.phase != BotWaitPhase::CleanupPending {
            return Err(publication_pending().into());
        }
        let target = &execution.cleanup_target;
        let key = target.creation_key();
        let row = &execution.current_attempt;
        if row.attempt != target.attempt().0
            || row.generation != target.attempt().1
            || !row
                .provider_session
                .is_some_and(|p| p.idempotency == key && p.session == target.session())
        {
            return Err(publication_pending().into());
        }
        self.ports.mcp.revoke(run);
        let session = match target.session() {
            Some(session) => Some(session),
            None => match self.ports.sessions.lookup(key)?.1 {
                CreationLookup::Known { session } | CreationLookup::KnownEnded { session } => {
                    Some(session.id)
                }
                CreationLookup::Creating | CreationLookup::NoEvidence => None,
            },
        };
        if let Some(session) = session {
            if let Err(error) = self.ports.sessions.terminate(session) {
                // A reaped session can refuse termination while its exact
                // retire call positively fences the creation. Never equate the
                // termination return value with physical end evidence.
                self.trace(
                    run,
                    "decision_terminate_refused",
                    format_args!("{}", error.code),
                );
            }
        }
        let (host, retired) = self.ports.sessions.retire(key)?;
        let result = match retired {
            CreationLookup::NoEvidence => ObservedRetirementResult::NoEvidence {},
            CreationLookup::KnownEnded { session } => ObservedRetirementResult::KnownEnded {
                session: session.id,
            },
            CreationLookup::Known { session } => match session.process {
                ProcessState::Exited { code } => ObservedRetirementResult::KnownExited {
                    session: session.id,
                    code,
                },
                ProcessState::Failed { error } => ObservedRetirementResult::KnownFailed {
                    session: session.id,
                    error: error.code,
                },
                ProcessState::Starting | ProcessState::Running => return Ok(None),
            },
            CreationLookup::Creating => return Ok(None),
        };
        let observed = ObservedCreationRetirement {
            creation_key: key,
            host,
            result,
            observed_at_ms: self.ports.clock.now_ms(),
        };
        if !observed.matches_target(target) {
            // A previously unknown successor session needs the C2 recovered
            // creation binding before its exact positive result can qualify.
            return Ok(None);
        }
        let Some(current) = self.bot_ledger()?.decision_wait_execution(owner, run)? else {
            return Ok(None);
        };
        if current.wait.wait != execution.wait.wait
            || current.wait.revision != execution.wait.revision
            || current.wait.phase != BotWaitPhase::CleanupPending
            || current.cleanup_target != execution.cleanup_target
            || current.current_attempt != execution.current_attempt
        {
            return Ok(None);
        }
        Ok(Some(observed))
    }
}
