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
}

/// Stable refusal used by full readers and fresh work while a handle is away.
pub fn publication_pending() -> PortError {
    PortError::new(
        Port::Documents,
        "publication_pending",
        "the run publication is awaiting its owning document handle",
    )
}
