// SPDX-License-Identifier: MPL-2.0

//! In-process ownership transfer. None of these values is a wire capability.

use super::*;

/// One physical workflow, retained across preparation and its writer receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunPublicationTicket {
    pub run: RunId,
    pub epoch: uuid::Uuid,
}

/// A single handle leaves the actor. The host must return it or report actual
/// physical loss before another workflow can start; a timeout is neither.
pub struct RunPublicationStart {
    pub ticket: RunPublicationTicket,
    pub document: RunPublicationDocument,
    pub operation: RunPublicationOperation,
}

pub enum RunPublicationDocument {
    Attached(Box<dyn RunDocumentHandle>),
    Cold(Box<dyn RunDocumentOpen>),
}

/// A single-use specification. Constructing it performs no Store I/O; the
/// fixed publication worker opens only after the actor has reserved ownership.
pub trait RunDocumentOpen: Send {
    fn open(self: Box<Self>) -> PortResult<Box<dyn RunDocumentHandle>>;
}

pub enum RunPublicationOperation {
    Prepare(RunProjectionIntent),
    /// Cold recovery imports only the bytes retained by the sole SQL owner.
    Apply(DecisionPreparedProjection),
    /// Read-only source recovery for one exact cold key on the same physical
    /// ticket. It never prepares, imports, writes SQL or consumes events.
    ObserveSource(RunSourceObservation),
}

/// One needed usage key, checked against the latest applied marker. The
/// worker reads current timeline items one at a time and retains only this
/// key's bounded evidence; the actor caches the answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSourceObservation {
    pub projection: koru_delegate_core::decisions::DecisionProjectionIdentity,
    pub lookup: DecisionUsageLookup,
}

impl RunSourceObservation {
    /// The only document access this operation is allowed: a bounded read of
    /// the attached handle's local state. No settle, commit or staging use.
    pub fn observe(&self, handle: &dyn RunDocumentHandle) -> PortResult<DecisionUsageObservation> {
        handle
            .document()
            .observe_decision_usage(&self.projection, &self.lookup)
            .map_err(Into::into)
    }
}

/// Stable refusal used by full readers and fresh work while a handle is away.
pub fn publication_pending() -> PortError {
    PortError::new(
        Port::Documents,
        "publication_pending",
        "the run publication is awaiting its owning document handle",
    )
}
