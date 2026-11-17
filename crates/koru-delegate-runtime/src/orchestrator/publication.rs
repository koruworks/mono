// SPDX-License-Identifier: MPL-2.0

//! The actor retains execution metadata while its sole document handle is away.
//! SQL owns backlog and prepared bytes; this state only describes physical
//! ownership. Missing or late completions cannot manufacture durability.

use super::*;
use crate::{
    PortResult, RunDocumentHandle, RunPublicationDocument, RunPublicationOperation,
    RunPublicationStart, RunPublicationTicket, publication_pending,
};
use koru_delegate_core::decisions::{
    DecisionPreparedProjection, DecisionProjectionAcknowledgment, DecisionProjectionState,
};
use koru_delegate_document::{RunDocument, RunDocumentError};
mod cleanup;
pub(crate) mod source;

/// Only this module constructs this value, after actual handle installation.
/// An acknowledgment DTO alone never proves physical document ownership.
pub(crate) struct ReturnedRunProjection {
    proof: crate::AppliedRunProjection,
}

pub(crate) enum RunDocumentOwner {
    /// Retained placement is known, but no document handle has been opened.
    /// Unlike ReopenOwed this is not evidence of a lost physical operation.
    Unopened,
    Attached {
        handle: Box<dyn RunDocumentHandle>,
        exclusive: bool,
    },
    PublicationInFlight {
        ticket: RunPublicationTicket,
        previous_loss: Option<RunPublicationTicket>,
    },
    ReopenOwed {
        ticket: RunPublicationTicket,
    },
}

impl RunDocumentOwner {
    pub(super) fn attached(handle: Box<dyn RunDocumentHandle>) -> Self {
        Self::Attached {
            handle,
            exclusive: false,
        }
    }
    pub(crate) fn document(&self) -> PortResult<&RunDocument> {
        match self {
            Self::Attached { handle, .. } => Ok(handle.document()),
            _ => Err(publication_pending()),
        }
    }
    pub(crate) fn commit(
        &mut self,
        write: &mut dyn FnMut(&RunDocument) -> Result<(), RunDocumentError>,
    ) -> PortResult<()> {
        match self {
            Self::Attached {
                handle,
                exclusive: false,
            } => handle.commit(write),
            _ => Err(publication_pending()),
        }
    }
    pub(super) fn absorb(&mut self) -> PortResult<bool> {
        match self {
            Self::Attached { handle, .. } => handle.absorb(),
            _ => Err(publication_pending()),
        }
    }
    pub(super) fn settle(&mut self) -> PortResult<()> {
        match self {
            Self::Attached { handle, .. } => handle.settle(),
            _ => Err(publication_pending()),
        }
    }
    pub(super) fn durable(&self) -> bool {
        matches!(self, Self::Attached { handle, exclusive: false } if handle.durable())
    }
    pub(super) fn attached_unsettled(&self) -> bool {
        matches!(self, Self::Attached { handle, .. } if !handle.durable())
    }
    pub(super) fn exclusive(&self) -> bool {
        !matches!(
            self,
            Self::Attached {
                exclusive: false,
                ..
            }
        )
    }
    fn transfer(&mut self, ticket: RunPublicationTicket) -> PortResult<Box<dyn RunDocumentHandle>> {
        if !matches!(self, Self::Attached { .. }) {
            return Err(publication_pending());
        }
        let old = std::mem::replace(
            self,
            Self::PublicationInFlight {
                ticket,
                previous_loss: None,
            },
        );
        let Self::Attached { handle, .. } = old else {
            unreachable!()
        };
        Ok(handle)
    }
    fn receive(
        &mut self,
        ticket: RunPublicationTicket,
        handle: Box<dyn RunDocumentHandle>,
    ) -> Result<(), Box<dyn RunDocumentHandle>> {
        if !matches!(self, Self::PublicationInFlight { ticket: current, .. } if *current == ticket)
        {
            return Err(handle);
        }
        *self = Self::Attached {
            handle,
            exclusive: true,
        };
        Ok(())
    }
}

impl Orchestrator {
    /// Select through the sole retained decoder. No caller-authored target or
    /// delta reaches the physical worker through this production entry point.
    pub fn next_run_publication(
        &mut self,
        run: RunId,
    ) -> Result<Option<RunPublicationStart>, RunError> {
        if self.publication_ticket.is_some() {
            return Err(publication_pending().into());
        }
        // An Applied Source installs its memory while its marker is current.
        // A stale-producer discard is reported by the retry, not here.
        let _ = self.retry_decision_source(run);
        let retained = self.retained_source_transition(run);
        let owner = self.publication_owner(run)?;
        let Some(record) = self.bot_ledger()?.next_decision_projection(owner, run)? else {
            if retained.is_some() {
                // Applied but not yet installed: nothing may replace the marker.
                return Err(publication_pending().into());
            }
            return match self.next_source_observation(run, owner)? {
                Some(operation) => self.transfer_run_publication(run, operation).map(Some),
                None => Ok(None),
            };
        };
        if retained.is_some_and(|transition| transition != record.identity.transition) {
            return Err(publication_pending().into());
        }
        let operation = match record.state {
            DecisionProjectionState::Pending => {
                RunPublicationOperation::Prepare(crate::RunProjectionIntent {
                    identity: record.identity,
                    target: record.target,
                })
            }
            DecisionProjectionState::Prepared { prepared } => {
                RunPublicationOperation::Apply(prepared)
            }
            _ => return Err(publication_pending().into()),
        };
        self.transfer_run_publication(run, operation).map(Some)
    }

    fn publication_owner(&mut self, run: RunId) -> Result<koru_delegate_core::BotOwner, RunError> {
        self.bot_ledger()?
            .bot_execution(run)?
            .map(|record| record.owner)
            .ok_or_else(|| publication_pending().into())
    }

    /// The physical worker retains the same handle while this transaction
    /// records its exact bytes. An unknown writer result never permits apply.
    pub fn record_run_preparation(
        &mut self,
        ticket: RunPublicationTicket,
        prepared: &DecisionPreparedProjection,
    ) -> Result<DecisionPreparedProjection, RunError> {
        if self.publication_ticket != Some(ticket) || prepared.identity.run != ticket.run {
            return Err(publication_pending().into());
        }
        let owner = self.publication_owner(ticket.run)?;
        let retained = self
            .bot_ledger()?
            .record_decision_preparation(owner, prepared)?;
        match retained.state {
            DecisionProjectionState::Prepared { prepared: exact } if exact == *prepared => {
                Ok(exact)
            }
            _ => Err(publication_pending().into()),
        }
    }

    /// A checked document result supplies causal evidence; SQL supplies the
    /// retained delta identity and exact target-derived publication envelopes.
    pub fn acknowledge_run_projection(
        &mut self,
        proof: &crate::AppliedRunProjection,
    ) -> Result<(), RunError> {
        let identity = proof.identity();
        let owner = self.publication_owner(identity.run)?;
        let retained = self
            .bot_ledger()?
            .decision_projection(owner, identity.run, identity.transition)?
            .ok_or_else(publication_pending)?;
        if retained.identity != *identity {
            return Err(publication_pending().into());
        }
        let prepared = retained.prepared().ok_or_else(publication_pending)?;
        // SQL must not acknowledge a worker result while that worker still
        // owns the handle. This also rechecks the marker after physical return.
        let state = self
            .runs
            .get_mut(&identity.run)
            .ok_or_else(publication_pending)?;
        match &state.doc {
            RunDocumentOwner::Attached {
                handle,
                exclusive: true,
            } if handle.durable() => {
                handle
                    .document()
                    .check_returned_projection(prepared, proof)?;
            }
            _ => return Err(publication_pending().into()),
        }
        state.publication_returned = Some(ReturnedRunProjection {
            proof: proof.clone(),
        });
        let ack = DecisionProjectionAcknowledgment {
            identity: identity.clone(),
            delta_digest: prepared.delta_digest,
            observed_version: proof.observed_version().clone(),
        };
        let applied = self
            .bot_ledger()?
            .acknowledge_decision_projection(owner, &ack)?;
        if !matches!(applied.state, DecisionProjectionState::Applied { .. }) {
            return Err(publication_pending().into());
        }
        // Install this Source's bookkeeping while its exact returned marker is
        // still current, before the next carried Control can replace it.
        self.retry_decision_source(identity.run)?;
        Ok(())
    }

    /// Host-only handoff after the sole ledger has selected/validated a target.
    /// This does not create a decision, accept a source frontier or write SQL.
    pub fn transfer_run_publication(
        &mut self,
        run: RunId,
        operation: RunPublicationOperation,
    ) -> Result<RunPublicationStart, RunError> {
        if self.publication_ticket.is_some() {
            return Err(publication_pending().into());
        }
        let identity = match &operation {
            RunPublicationOperation::Prepare(intent) => {
                intent.validate()?;
                &intent.identity
            }
            RunPublicationOperation::Apply(prepared) => &prepared.identity,
            RunPublicationOperation::ObserveSource(observation) => &observation.projection,
        };
        if identity.run != run {
            return Err(PortError::new(
                crate::Port::Documents,
                "projection_subject",
                "projection placement differs from retained run placement",
            )
            .into());
        }
        let ticket = RunPublicationTicket {
            run,
            epoch: uuid::Uuid::new_v4(),
        };
        if let Some(state) = self.runs.get_mut(&run) {
            state.publication_returned = None;
            // Usage evidence is exact only within one exclusive window: an
            // ordinary writer may have amended facts while the handle was not
            // exclusively ours, and a lost handle may or may not have applied.
            if !matches!(
                state.doc,
                RunDocumentOwner::Attached {
                    exclusive: true,
                    ..
                }
            ) {
                state.source_usage.clear();
            }
        }
        if let Some(state) = self.runs.get(&run) {
            if identity.workspace != state.workspace || identity.document != state.document {
                return Err(publication_pending().into());
            }
        }
        let cold = self.runs.get(&run).is_none_or(|state| {
            matches!(
                state.doc,
                RunDocumentOwner::Unopened | RunDocumentOwner::ReopenOwed { .. }
            )
        });
        let document = if cold {
            // Only the validated SQL placement is used. Freeze a specification
            // without opening or joining anything on this actor thread.
            let job = self
                .ports
                .documents
                .publication_open(identity.workspace, identity.document)?;
            if let Some(state) = self.runs.get_mut(&run) {
                let previous_loss = match state.doc {
                    RunDocumentOwner::ReopenOwed { ticket } => Some(ticket),
                    _ => None,
                };
                state.doc = RunDocumentOwner::PublicationInFlight {
                    ticket,
                    previous_loss,
                };
            } else {
                self.runs.insert(
                    run,
                    RunState::with_document_owner(
                        identity.workspace,
                        identity.document,
                        RunDocumentOwner::PublicationInFlight {
                            ticket,
                            previous_loss: None,
                        },
                    ),
                );
            }
            RunPublicationDocument::Cold(job)
        } else {
            RunPublicationDocument::Attached(self.runs.get_mut(&run).unwrap().doc.transfer(ticket)?)
        };
        self.publication_ticket = Some(ticket);
        Ok(RunPublicationStart {
            ticket,
            document,
            operation,
        })
    }

    /// A request which never reached the worker still owns either its original
    /// handle or an unexecuted cold-open specification. Neither may be lost in
    /// a full/disconnected request channel.
    pub fn cancel_unstarted_publication(&mut self, start: RunPublicationStart) {
        match start.document {
            RunPublicationDocument::Attached(handle) => {
                if let Err(handle) = self.return_run_publication(start.ticket, handle) {
                    drop(handle);
                }
            }
            RunPublicationDocument::Cold(job) => {
                drop(job);
                if self.publication_ticket == Some(start.ticket)
                    && let Some(state) = self.runs.get_mut(&start.ticket.run)
                    && let RunDocumentOwner::PublicationInFlight {
                        ticket,
                        previous_loss,
                    } = state.doc
                    && ticket == start.ticket
                {
                    // This specification never opened a handle. Preserve the
                    // metadata fence without manufacturing a physical loss.
                    state.doc = previous_loss.map_or(RunDocumentOwner::Unopened, |ticket| {
                        RunDocumentOwner::ReopenOwed { ticket }
                    });
                    self.publication_ticket = None;
                }
            }
        }
    }

    /// An actual physical return, not a logical timeout. A mismatched completion
    /// retains its handle at the caller for disposal; it never replaces ours.
    pub fn return_run_publication(
        &mut self,
        ticket: RunPublicationTicket,
        handle: Box<dyn RunDocumentHandle>,
    ) -> Result<(), Box<dyn RunDocumentHandle>> {
        if self.publication_ticket != Some(ticket) {
            return Err(handle);
        }
        let Some(state) = self.runs.get_mut(&ticket.run) else {
            return Err(handle);
        };
        state.doc.receive(ticket, handle)?;
        self.publication_ticket = None;
        Ok(())
    }

    /// The worker has ended the physical operation and lost/dropped the handle.
    /// Reopen is owed; no reader may open a concurrent replacement connection.
    pub fn lost_run_publication(&mut self, ticket: RunPublicationTicket) -> bool {
        if self.publication_ticket != Some(ticket) {
            return false;
        }
        let Some(state) = self.runs.get_mut(&ticket.run) else {
            return false;
        };
        if !matches!(state.doc, RunDocumentOwner::PublicationInFlight { ticket: current, .. } if current == ticket)
        {
            return false;
        }
        state.doc = RunDocumentOwner::ReopenOwed { ticket };
        self.publication_ticket = None;
        true
    }

    /// Final wait settlement and the end of exclusive publication ownership.
    /// D holds the private proof C requires: the sole handle physically
    /// returned (`publication_returned`, installed only after Attached
    /// installation), it is durable and still carries the exact latest
    /// Applied marker, the journal is empty and no Source retry is staged.
    /// C settles only the wait, never admission or capacity: a normally
    /// continuing Run stays Executing. Returns the ordinary stream to pump
    /// from the accepted Agent frontier once the fence has ended.
    pub fn settle_publication_wait(&mut self, run: RunId) -> Result<Option<Started>, RunError> {
        use koru_delegate_core::decisions::{ConsumerCleanup, DecisionPhase};
        if self
            .publication_ticket
            .is_some_and(|ticket| ticket.run == run)
        {
            return Ok(None);
        }
        let Some(state) = self.runs.get(&run) else {
            return Ok(None);
        };
        if state.decision_source.is_some() {
            return Ok(None);
        }
        let Some(proof) = state
            .publication_returned
            .as_ref()
            .map(|returned| returned.proof.clone())
        else {
            return Ok(None);
        };
        let owner = self.publication_owner(run)?;
        if self
            .bot_ledger()?
            .next_decision_projection(owner, run)?
            .is_some()
        {
            return Ok(None);
        }
        let Some(execution) = self.bot_ledger()?.decision_wait_execution(owner, run)? else {
            return Ok(None);
        };
        // Avoid futile writer transactions; the C writer remains decisive.
        let eligible = match &execution.decision_state {
            Some(decision) => decision.phase == DecisionPhase::Settled,
            None => {
                matches!(
                    execution.wait.cleanup,
                    Some(ConsumerCleanup::Confirmed { .. })
                ) || self.child_wait_resumed(owner, run)
            }
        };
        if !eligible {
            return Ok(None);
        }
        let latest = self
            .bot_ledger()?
            .latest_applied_decision_projection(owner, run)?
            .ok_or_else(publication_pending)?;
        if latest.identity != *proof.identity() {
            return Ok(None);
        }
        let prepared = latest.prepared().ok_or_else(publication_pending)?;
        let state = self.runs.get(&run).ok_or_else(publication_pending)?;
        match &state.doc {
            RunDocumentOwner::Attached {
                handle,
                exclusive: true,
            } if handle.durable() => {
                handle
                    .document()
                    .check_returned_projection(prepared, &proof)?;
            }
            _ => return Ok(None),
        }
        let ack = DecisionProjectionAcknowledgment {
            identity: latest.identity.clone(),
            delta_digest: prepared.delta_digest,
            observed_version: proof.observed_version().clone(),
        };
        self.bot_ledger()?
            .settle_decision_wait(&execution.expected, &ack)?;
        // Later ordinary commits on this same handle causally include every
        // applied projection; a new decision re-enters from a durable base.
        let state = self.runs.get_mut(&run).ok_or_else(publication_pending)?;
        if let RunDocumentOwner::Attached { exclusive, .. } = &mut state.doc {
            *exclusive = false;
        }
        // A retired or natively ended consumer has no stream to resume.
        let ended = state.source_obligation == Some(source::SourceObligation::ProcessEnded);
        state.publication_returned = None;
        state.source_epoch = None;
        state.source_obligation = None;
        state.source_usage.clear();
        state.delivery_retry = None;
        state.decision_cleanup = None;
        // A page result confirmed while the decision held the handle is
        // durable in the native journal but still owes its registration in
        // the Run document, which only an ordinary commit can write. That is
        // possible now: register it (an approved edit Koru saved during the
        // wait, O150), so delivery and cleanup do not wait on it. A failure
        // keeps it Verified; the existing terminal cleanup retries it.
        if let Err(error) = self.reconcile_verified_native_for_run(run) {
            self.trace(run, "verified_native_unregistered", format_args!("{error}"));
        }
        let state = self.runs.get_mut(&run).ok_or_else(publication_pending)?;
        let Some(attempt) = state.attempt.as_ref().filter(|_| !ended) else {
            return Ok(None);
        };
        let (attempt_id, generation) = (attempt.id, attempt.generation);
        let Some(session) = attempt.session else {
            return Ok(None);
        };
        let after = state.source_frontiers.agent_after;
        let status = state.doc.document()?.header()?.status;
        let events = self.ports.sessions.subscribe(session, after)?;
        Ok(Some(Started {
            run,
            attempt: attempt_id,
            generation,
            status,
            events: Some(events),
        }))
    }

    /// A child wait may settle without a cleanup once it has no open request
    /// and its successor's continuation input is acknowledged (M6): the
    /// Control that cleared the waiting marker is then the last one owed.
    /// The ledger remains decisive.
    fn child_wait_resumed(&mut self, owner: koru_delegate_core::BotOwner, run: RunId) -> bool {
        use koru_delegate_core::decisions::{BotWaitPhase, DecisionResumeState};
        let Ok(Some(child)) = self
            .bot_ledger()
            .and_then(|ledger| Ok(ledger.child_wait_execution(owner, run)))
            .and_then(|read| Ok(read?))
        else {
            return false;
        };
        child.open_requests == 0
            && child.wait.phase == BotWaitPhase::ResumeReserved
            && child.wait.cleanup.is_none()
            && child.resume.as_ref().is_some_and(|resume| {
                matches!(resume.state, DecisionResumeState::InputAcknowledged { .. })
            })
    }

    /// Runs to offer the single physical ticket, rotating after `after`: one
    /// bounded keyset page over the approved open-projection indexes, one
    /// wrapped page at or before the cursor, then read-only observations
    /// (which have no SQL row) from the bounded in-memory inventory. Durable
    /// rows are found even when no RunState exists, as after a restart.
    pub fn publication_runs(&mut self, after: Option<RunId>) -> Result<Vec<RunId>, RunError> {
        use koru_delegate_sqlite::DECISION_PROJECTION_RUN_PAGE as PAGE;
        let mut runs = self.ports.store.decision_projection_runs(after, PAGE)?;
        if let Some(cursor) = after {
            for run in self.ports.store.decision_projection_runs(None, PAGE)? {
                if run <= cursor && !runs.contains(&run) {
                    runs.push(run);
                }
            }
        }
        let mut observing: Vec<RunId> = self
            .runs
            .iter()
            .filter(|(run, state)| {
                matches!(
                    state.source_obligation,
                    Some(source::SourceObligation::Usage(_))
                ) && !runs.contains(run)
            })
            .map(|(run, _)| *run)
            .collect();
        observing.sort();
        runs.extend(observing);
        Ok(runs)
    }

    /// Runs whose publication is owned exclusively here, in RunId order. This
    /// is the retained in-memory inventory (bounded by the occupancy owner's
    /// 50 waits plus 2 occupied Runs), not a history scan. The host rotates a
    /// cursor through it so one failing Run cannot starve the others.
    pub fn publication_candidates(&self) -> Vec<RunId> {
        let mut runs: Vec<RunId> = self
            .runs
            .iter()
            .filter(|(_, state)| state.doc.exclusive())
            .map(|(run, _)| *run)
            .collect();
        runs.sort();
        runs
    }

    pub fn run_publication_pending(&self, run: RunId) -> bool {
        self.runs
            .get(&run)
            .is_some_and(|state| state.doc.exclusive())
    }
}
