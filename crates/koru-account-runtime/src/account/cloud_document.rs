// SPDX-License-Identifier: MPL-2.0

//! The existing authenticated native Cloud carrier, supplied by each host.
use super::Cancel;
use koru_store_wire::cloud_document::{
    CloudDocumentAction, CloudDocumentFailure, CloudDocumentOutcome, CloudDocumentSelection,
};

/// The account service admits only a bounded exact selector from its current
/// trusted inventory page. Hosts independently bind it to their current
/// account, Workspace/incarnation and enabled ordinary content session.
pub trait CloudDocumentPort: Send + Sync {
    /// `can_write` is a current account entitlement gate, combined with the
    /// host's live content permit; it is never read from a caller's DTO.
    /// Cancellation closes the inspection carrier. A possibly submitted
    /// removal returns OutcomeUnknown and must be followed by a Cloud reread.
    fn run(
        &self,
        selection: CloudDocumentSelection,
        action: CloudDocumentAction,
        can_write: bool,
        cancel: &Cancel,
    ) -> Result<CloudDocumentOutcome, CloudDocumentFailure>;
}
