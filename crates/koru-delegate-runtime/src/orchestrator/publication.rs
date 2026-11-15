// SPDX-License-Identifier: MPL-2.0

//! The actor retains execution metadata while its sole document handle is away.
//! SQL owns backlog and prepared bytes; this state only describes physical
//! ownership. Missing or late completions cannot manufacture durability.

use super::*;
use crate::{
    PortResult, RunDocumentHandle, RunPublicationOperation, RunPublicationStart,
    RunPublicationTicket, publication_pending,
};
use koru_delegate_document::{RunDocument, RunDocumentError};

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
        let state = self.open(run)?;
        if identity.run != run
            || identity.workspace != state.workspace
            || identity.document != state.document
        {
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
        let handle = state.doc.transfer(ticket)?;
        self.publication_ticket = Some(ticket);
        Ok(RunPublicationStart {
            ticket,
            handle,
            operation,
        })
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
