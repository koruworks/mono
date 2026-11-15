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

pub(crate) enum RunDocumentOwner {
    Attached {
        handle: Box<dyn RunDocumentHandle>,
        exclusive: bool,
    },
    PublicationInFlight {
        ticket: RunPublicationTicket,
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
        let old = std::mem::replace(self, Self::PublicationInFlight { ticket });
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
        if !matches!(self, Self::PublicationInFlight { ticket: current } if *current == ticket) {
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
        let owner = self.publication_owner(run)?;
        let Some(record) = self.bot_ledger()?.next_decision_projection(owner, run)? else {
            return Ok(None);
        };
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
        if let Some(state) = self.runs.get(&run) {
            if identity.workspace != state.workspace || identity.document != state.document {
                return Err(publication_pending().into());
            }
        }
        let cold = self
            .runs
            .get(&run)
            .is_none_or(|state| matches!(state.doc, RunDocumentOwner::ReopenOwed { .. }));
        let document = if cold {
            // Only the validated SQL placement is used. Freeze a specification
            // without opening or joining anything on this actor thread.
            let job = self
                .ports
                .documents
                .publication_open(identity.workspace, identity.document)?;
            if let Some(state) = self.runs.get_mut(&run) {
                state.doc = RunDocumentOwner::PublicationInFlight { ticket };
            } else {
                self.runs.insert(
                    run,
                    RunState::with_document_owner(
                        identity.workspace,
                        identity.document,
                        RunDocumentOwner::PublicationInFlight { ticket },
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
                self.lost_run_publication(start.ticket);
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
        if !matches!(state.doc, RunDocumentOwner::PublicationInFlight { ticket: current } if current == ticket)
        {
            return false;
        }
        state.doc = RunDocumentOwner::ReopenOwed { ticket };
        self.publication_ticket = None;
        true
    }

    pub fn run_publication_pending(&self, run: RunId) -> bool {
        self.runs
            .get(&run)
            .is_some_and(|state| state.doc.exclusive())
    }
}
