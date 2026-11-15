// SPDX-License-Identifier: MPL-2.0

//! Physical cleanup uses the retained current consumer, independently of Run
//! document availability. The canonical terminal projection remains owed.

use super::*;
use crate::orchestrator::state::{DecisionCleanupRequest, DecisionCleanupRetry};
use koru_delegate_core::decisions::{
    BotWaitPhase, DecisionClosureReason, DecisionTransitionId, ObservedCreationRetirement,
    ObservedRetirementResult,
};
use koru_delegate_sqlite::BotLedgerError;
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
                confirm: None,
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
                    confirm: None,
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
                confirm: None,
                due: Instant::now(),
                round: 0,
            });
        // No owner exists (checked above): an armed retry without a request,
        // such as an accepted native end, mints its one exact request now.
        let request = retry
            .request
            .get_or_insert_with(|| DecisionCleanupRequest {
                expected: execution.expected.clone(),
                transition: DecisionTransitionId::mint(),
                cause,
            })
            .clone();
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
        // An accepted positive native end with no person's request is not an
        // owner Stop: it changes what the open decision can still authorize.
        let process_ended = self.runs.get(&run).is_some_and(|state| {
            state.source_obligation == Some(super::source::SourceObligation::ProcessEnded)
        });
        let cause = retry
            .request
            .as_ref()
            .map(|request| request.cause)
            .unwrap_or(if process_ended {
                DecisionClosureReason::AuthorityChanged
            } else {
                DecisionClosureReason::Canceled
            });
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
        // Observation alone cannot settle publication or release capacity.
        // Only a positive exact observation may become final cleanup evidence.
        if let Some(observed) = observed? {
            self.confirm_decision_cleanup(run, observed)?;
        }
        Ok(Some(false))
    }

    /// Final Confirmed cleanup for the exact retained current consumer, from
    /// positive proof only: the C cleanup owner, CleanupPending, a qualified
    /// creation retirement, no held or pending native effect, the attempt row
    /// actually retired here, and a complete native drain. Anything else keeps
    /// the wait, admission and capacity occupied. Admission settlement and the
    /// folder claim release remain later and separate.
    pub(crate) fn confirm_decision_cleanup(
        &mut self,
        run: RunId,
        observed: ObservedCreationRetirement,
    ) -> Result<bool, RunError> {
        use koru_delegate_core::decisions::{ConsumerCleanup, DecisionAttemptRetirement};
        use koru_delegate_sqlite::AttemptPhase;
        let owner = self.publication_owner(run)?;
        let Some(ownership) = self.bot_ledger()?.decision_cleanup_ownership(owner, run)? else {
            return Ok(false);
        };
        let Some(execution) = self.bot_ledger()?.decision_wait_execution(owner, run)? else {
            return Ok(false);
        };
        if execution.wait.phase != BotWaitPhase::CleanupPending
            || ownership.expected.wait != execution.wait.wait
            || !observed.matches_target(&execution.cleanup_target)
        {
            return Ok(false);
        }
        let retained = self
            .runs
            .get(&run)
            .and_then(|state| state.decision_cleanup.as_ref())
            .and_then(|retry| retry.confirm.clone());
        if execution.wait.cleanup.is_some() && retained.is_none() {
            // Recorded by an earlier process; its Control is the owed
            // projection. Never mint a second final record.
            return Ok(false);
        }
        let (transition, cleanup) = match retained {
            Some(retained) => retained,
            None => {
                // A held lease may still commit; pending verified evidence
                // still needs its owner. Neither is drained absence.
                if self.native_effects_pending(run)? {
                    return Ok(false);
                }
                let (attempt, generation) = execution.cleanup_target.attempt();
                // The effect proof is gathered before anything is retired, so
                // an unresolved effect changes nothing and occupancy stays held.
                let Some(effect) = self.decision_retained_effect(owner, &execution)? else {
                    return Ok(false);
                };
                let row = &execution.current_attempt;
                if row.attempt != attempt
                    || row.generation != generation
                    || row.host_instance != Some(observed.host)
                {
                    return Ok(false);
                }
                if !row.retired {
                    if !row.phase.is_terminal() {
                        // Unknown host evidence is not a clean provider end.
                        let phase = match observed.result {
                            ObservedRetirementResult::NoEvidence {} => AttemptPhase::Unknown,
                            _ => AttemptPhase::Ended,
                        };
                        self.ports.store.advance(run, generation, phase)?;
                    }
                    self.ports.store.retire(run, generation)?;
                }
                // Re-read the retained consumer after the actual retirement.
                let Some(current) = self.bot_ledger()?.decision_wait_execution(owner, run)? else {
                    return Ok(false);
                };
                let row = &current.current_attempt;
                if current.wait.wait != execution.wait.wait
                    || current.cleanup_target != execution.cleanup_target
                    || row.attempt != attempt
                    || row.generation != generation
                    || !row.retired
                    || !row.phase.is_terminal()
                {
                    return Ok(false);
                }
                let now = self.ports.clock.now_ms();
                // The drain reader requires the retired terminal attempt. An
                // unavailable drain records nothing; the retired row is only
                // the physical part, which may legitimately finish first.
                let Some(native_drain) =
                    self.decision_native_drain(owner, run, attempt, generation, now)?
                else {
                    return Ok(false);
                };
                let cleanup = ConsumerCleanup::Confirmed {
                    cleanup_revision: current.expected.revision,
                    target: current.cleanup_target.clone(),
                    creation_retirement: observed,
                    attempt_retirement: DecisionAttemptRetirement {
                        run,
                        attempt,
                        generation,
                        retired_at_ms: row.updated_at_ms,
                    },
                    native_drain,
                    retained_effect_outcome: effect,
                };
                let transition = DecisionTransitionId::mint();
                if let Some(retry) = self
                    .runs
                    .get_mut(&run)
                    .and_then(|state| state.decision_cleanup.as_mut())
                {
                    retry.confirm = Some((transition, cleanup.clone()));
                }
                (transition, cleanup)
            }
        };
        let current = self
            .bot_ledger()?
            .decision_wait_execution(owner, run)?
            .ok_or_else(publication_pending)?;
        let written = if current.expected.decision.is_some() {
            self.bot_ledger()?
                .record_decision_consumer_cleanup(
                    &current.expected,
                    ownership.projection,
                    transition,
                    &cleanup,
                )
                .map(|_| ())
        } else {
            self.bot_ledger()?
                .record_nondecision_cleanup(
                    &current.expected,
                    ownership.projection,
                    transition,
                    &cleanup,
                )
                .map(|_| ())
        };
        if let Err(error) = written {
            // A lost reply is resolved by the exact retained Control, never
            // by a second record or recomputed evidence.
            if self
                .bot_ledger()?
                .decision_projection(owner, run, transition)?
                .is_none()
            {
                return Err(error.into());
            }
        }
        if let Some(state) = self.runs.get_mut(&run) {
            if let Some(retry) = state.decision_cleanup.as_mut() {
                retry.confirm = None;
            }
            // The consumer is retired: no producer reconnects for it.
            state.source_obligation = Some(super::source::SourceObligation::ProcessEnded);
        }
        Ok(true)
    }

    /// The complete drained native receipts of exactly this retired attempt
    /// from the C bounded reader, or None while that proof is unavailable
    /// (a held lease, an unretired attempt, or evidence over its bound).
    fn decision_native_drain(
        &mut self,
        owner: koru_delegate_core::BotOwner,
        run: RunId,
        attempt: koru_delegate_core::RunAttemptId,
        generation: koru_delegate_core::AttemptGeneration,
        observed_at_ms: i64,
    ) -> Result<Option<koru_delegate_core::decisions::DecisionNativeDrain>, RunError> {
        match self.bot_ledger()?.decision_native_drain(
            owner,
            run,
            attempt,
            generation,
            observed_at_ms,
        ) {
            Ok(drain) => Ok(Some(drain)),
            Err(BotLedgerError::Fenced | BotLedgerError::Capacity) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// The retained effect outcome the final record must carry. `None` means
    /// the proof is unavailable (the effect is unresolved) and occupancy stays
    /// held; `Some(None)` means no outcome is owed. Never invents NotApplied.
    fn decision_retained_effect(
        &mut self,
        owner: koru_delegate_core::BotOwner,
        execution: &koru_delegate_sqlite::DecisionWaitExecution,
    ) -> Result<Option<Option<koru_delegate_core::decisions::DecisionRetainedEffect>>, RunError>
    {
        let Some(decision) = execution.expected.decision.as_ref() else {
            return Ok(Some(None));
        };
        match self.bot_ledger()?.decision_retained_effect(
            owner,
            execution.expected.run,
            decision.decision,
        ) {
            Ok(effect) => Ok(Some(effect)),
            Err(BotLedgerError::Fenced | BotLedgerError::Capacity) => Ok(None),
            Err(error) => Err(error.into()),
        }
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
