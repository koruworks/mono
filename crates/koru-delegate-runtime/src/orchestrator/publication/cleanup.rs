// SPDX-License-Identifier: MPL-2.0

//! Physical cleanup uses the retained current consumer, independently of Run
//! document availability. The canonical terminal projection remains owed.

use super::*;
use crate::orchestrator::state::{DecisionCleanupRequest, DecisionCleanupRetry};
use koru_delegate_core::decisions::{
    BotWaitPhase, DecisionClosureReason, DecisionTransitionId, ObservedCreationRetirement,
    ObservedRetirementResult,
};
use koru_session_protocol::{CreationLookup, ProcessState};
use std::time::{Duration, Instant};

impl Orchestrator {
    /// Restore the exclusive owner before an ordinary cold open. Placement and
    /// open obligations come from the sole retained decoders, never a header or
    /// a fresh Store connection. Existing working memory is left in place.
    pub(crate) fn restore_publication_owner(&mut self, run: RunId) -> Result<(), RunError> {
        if self.runs.contains_key(&run) {
            return Ok(());
        }
        let Some(ledger) = self.ports.store.bots() else {
            return Ok(());
        };
        let Some(admission) = ledger.bot_execution(run)? else {
            return Ok(());
        };
        let execution = ledger.decision_wait_execution(admission.owner, run)?;
        let pending = ledger.next_decision_projection(admission.owner, run)?;
        if execution.is_none() && pending.is_none() {
            return Ok(());
        }
        let retained = ledger
            .latest_decision_projection(admission.owner, run)?
            .ok_or_else(publication_pending)?;
        // The sole C owner decides cleanup ownership: a Requested Stop/expiry/
        // authority change, a Koru Deny answer (O109), or a no-decision
        // obligation. Placement still comes from the retained projection.
        let owned_cleanup = execution.is_some()
            && ledger
                .decision_cleanup_ownership(admission.owner, run)?
                .is_some();
        let intent = self
            .ports
            .store
            .intent(run)?
            .ok_or_else(publication_pending)?;
        if intent.state != koru_delegate_sqlite::IntentState::Satisfied
            || intent.workspace != retained.identity.workspace
            || intent.run_document != Some(retained.identity.document)
            || retained.identity.run != run
            || retained.target.fence().owner != admission.owner
            || retained.target.fence().bot != admission.bot
            || execution
                .as_ref()
                .is_some_and(|execution| execution.wait.wait != retained.identity.wait)
        {
            return Err(publication_pending().into());
        }
        let mut state = RunState::with_document_owner(
            retained.identity.workspace,
            retained.identity.document,
            RunDocumentOwner::Unopened,
        );
        if owned_cleanup {
            state.decision_cleanup = Some(DecisionCleanupRetry {
                request: None,
                due: Instant::now(),
                round: 0,
            });
        }
        self.runs.insert(run, state);
        Ok(())
    }

    /// Retain one exact cleanup mutation before entering the writer. A lost SQL
    /// reply retries the same transition and expected revision; a cold restart
    /// adopts the already committed CleanupRequested row instead of minting one.
    pub(crate) fn request_publication_cleanup(
        &mut self,
        run: RunId,
        cause: DecisionClosureReason,
    ) -> Result<bool, RunError> {
        self.restore_publication_owner(run)?;
        let owner = self.publication_owner(run)?;
        let Some(execution) = self.bot_ledger()?.decision_wait_execution(owner, run)? else {
            return Ok(false);
        };
        let retained = self
            .bot_ledger()?
            .latest_decision_projection(owner, run)?
            .ok_or_else(publication_pending)?;
        let state = self.runs.get_mut(&run).ok_or_else(publication_pending)?;
        if state.workspace != retained.identity.workspace
            || state.document != retained.identity.document
            || execution.wait.wait != retained.identity.wait
        {
            return Err(publication_pending().into());
        }
        if let RunDocumentOwner::Attached { exclusive, .. } = &mut state.doc {
            *exclusive = true;
        }
        // The first retained cleanup owns its cause. A later budget/Stop does
        // not replace that immutable request, a Koru Deny answer's ownership
        // or a no-decision obligation, nor its absorbing terminal outcome.
        if self
            .bot_ledger()?
            .decision_cleanup_ownership(owner, run)?
            .is_some()
        {
            let state = self.runs.get_mut(&run).ok_or_else(publication_pending)?;
            state
                .decision_cleanup
                .get_or_insert(DecisionCleanupRetry {
                    request: None,
                    due: Instant::now(),
                    round: 0,
                })
                .request = None;
            return Ok(true);
        }
        let state = self.runs.get_mut(&run).ok_or_else(publication_pending)?;
        let retry = state
            .decision_cleanup
            .get_or_insert_with(|| DecisionCleanupRetry {
                request: Some(DecisionCleanupRequest {
                    expected: execution.expected.clone(),
                    transition: DecisionTransitionId::mint(),
                    cause,
                }),
                due: Instant::now(),
                round: 0,
            });
        let request = retry.request.clone().ok_or_else(publication_pending)?;
        self.bot_ledger()?.request_decision_cleanup(
            &request.expected,
            request.transition,
            request.cause,
        )?;
        // Read current ownership after any unknown/replayed write. Physical end
        // is still observed separately and neither this row nor a timeout frees
        // a Run claim, admission, or the publication worker's physical ticket.
        let current = self
            .bot_ledger()?
            .decision_wait_execution(owner, run)?
            .ok_or_else(publication_pending)?;
        // Ownership is re-read after any unknown/replayed write. Another owner
        // that committed first (a Deny answer, an earlier request) is equally
        // a durable fence; a rolled-back request is not.
        let ownership = self
            .bot_ledger()?
            .decision_cleanup_ownership(owner, run)?
            .ok_or_else(publication_pending)?;
        if ownership.expected.wait != current.wait.wait {
            return Err(publication_pending().into());
        }
        self.runs
            .get_mut(&run)
            .unwrap()
            .decision_cleanup
            .as_mut()
            .unwrap()
            .request = None;
        Ok(true)
    }

    pub(crate) fn publication_cleanup_tick(
        &mut self,
        run: RunId,
    ) -> Result<Option<bool>, RunError> {
        self.restore_publication_owner(run)?;
        let Some(retry) = self
            .runs
            .get(&run)
            .and_then(|state| state.decision_cleanup.as_ref())
        else {
            return Ok(None);
        };
        let now = Instant::now();
        if retry.due > now {
            return Ok(Some(false));
        }
        let cause = retry
            .request
            .as_ref()
            .map(|request| request.cause)
            .unwrap_or(DecisionClosureReason::Canceled);
        let requested = self.request_publication_cleanup(run, cause);
        // A writer may have committed despite returning an error. Physical
        // cleanup independently re-reads the retained current consumer.
        let observed = self.stop_decision_consumer(run);
        if let Some(retry) = self
            .runs
            .get_mut(&run)
            .and_then(|state| state.decision_cleanup.as_mut())
        {
            retry.due = now + Duration::from_secs(1u64 << retry.round.min(5));
            retry.round = (retry.round + 1).min(5);
        }
        requested?;
        observed?;
        // Observation alone cannot settle publication or release capacity.
        Ok(Some(false))
    }

    /// Permission to reconcile an existing Held effect while canonical status
    /// is still owed. This is not proof that cleanup or release has completed.
    pub(crate) fn publication_cleanup_requested(
        &mut self,
        run: RunId,
        owner_stop: bool,
    ) -> Result<bool, RunError> {
        let owner = self.publication_owner(run)?;
        let Some(execution) = self.bot_ledger()?.decision_wait_execution(owner, run)? else {
            return Err(publication_pending().into());
        };
        if execution.wait.phase != BotWaitPhase::CleanupPending {
            return Ok(false);
        }
        if !owner_stop {
            return Ok(true);
        }
        // Only the sole C owner says who owns this cleanup. A person's Stop or
        // execution-budget cancellation is a Requested/Canceled origin; a Koru
        // Deny or no-decision obligation is not an owner Stop (O109).
        let Some(ownership) = self.bot_ledger()?.decision_cleanup_ownership(owner, run)? else {
            return Err(publication_pending().into());
        };
        if ownership.expected.wait != execution.wait.wait {
            return Err(publication_pending().into());
        }
        Ok(matches!(
            ownership.origin,
            koru_delegate_sqlite::DecisionCleanupOrigin::Requested {
                cause: DecisionClosureReason::Canceled,
                ..
            }
        ))
    }

    /// Retained identity only while the sole document is unavailable/exclusive.
    /// This must never grant a lease, fresh write or positive native absence.
    pub(crate) fn publication_retained_subject(
        &mut self,
        admission: &koru_delegate_sqlite::BotAdmissionRecord,
    ) -> Result<(), RunError> {
        let submission = &admission.submission;
        let provenance = &submission.provenance;
        let run = submission.request.run;
        if !self.run_publication_pending(run) {
            return Err(publication_pending().into());
        }
        let execution = self
            .bot_ledger()?
            .decision_wait_execution(provenance.owner, run)?
            .ok_or_else(publication_pending)?;
        let retained = self
            .bot_ledger()?
            .latest_decision_projection(provenance.owner, run)?
            .ok_or_else(publication_pending)?;
        let intent = self
            .ports
            .store
            .intent(run)?
            .ok_or_else(publication_pending)?;
        let state = self.runs.get(&run).ok_or_else(publication_pending)?;
        if execution.wait.origin.owner != provenance.owner
            || execution.wait.origin.bot != provenance.bot
            || execution.wait.origin.run != run
            || intent.state != koru_delegate_sqlite::IntentState::Satisfied
            || intent.workspace != submission.workspace
            || intent.run_document != Some(retained.identity.document)
            || retained.identity.workspace != intent.workspace
            || retained.identity.run != run
            || retained.identity.wait != execution.wait.wait
            || retained.target.fence().owner != provenance.owner
            || retained.target.fence().bot != provenance.bot
            || retained.target.fence().run != run
            || state.workspace != intent.workspace
            || Some(state.document) != intent.run_document
        {
            return Err(super::super::bots::refused("bot_provenance_mismatch"));
        }
        // The sole C decoder validates full retained admission/decision/Routine
        // association and current consumer. An original effect can still name
        // the origin attempt after that consumer has moved to a successor.
        Ok(())
    }

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
        self.retire_decision_consumer(run, owner, execution)
    }

    /// A lost writer reply cannot suppress a committed cleanup fence. Re-read
    /// CleanupPending before physical Stop; a rolled-back request is not a
    /// durable fence against successor admission, input or consumption.
    pub(crate) fn stop_decision_consumer(
        &mut self,
        run: RunId,
    ) -> Result<Option<ObservedCreationRetirement>, RunError> {
        self.observe_decision_cleanup(run)
    }

    fn retire_decision_consumer(
        &mut self,
        run: RunId,
        owner: koru_delegate_core::BotOwner,
        execution: koru_delegate_sqlite::DecisionWaitExecution,
    ) -> Result<Option<ObservedCreationRetirement>, RunError> {
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
            || current.wait.phase != execution.wait.phase
            || current.cleanup_target != execution.cleanup_target
            || current.current_attempt != execution.current_attempt
        {
            return Ok(None);
        }
        Ok(Some(observed))
    }
}
