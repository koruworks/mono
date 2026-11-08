// SPDX-License-Identifier: MPL-2.0

//! The claim reference on the wire (`cloud-sync-admission`, the app
//! connection amendment; TECHNICAL §5.4 items 4 and 6).
//!
//! An app connection's upload of a claimed document carries the reference of
//! the claim its call presented, `SHA-256` of the handle, in
//! [`Start::claim`](super::Start::claim). The worker hands it on in
//! [`Completed::claim`](super::Completed::claim) and compares it with the
//! claim row when it publishes. Nothing here judges a claim: this is only
//! which sessions may carry a reference, and the value an upload carries.

use crate::cloud::SessionPrincipal;

/// Which way a claim reference may cross a session, read from the principal
/// the session was admitted under. A [`Stream`](super::Stream) refuses a
/// reference that would cross any other way as `sync_transfer_invalid`.
///
/// Every device session is [`ClaimReferences::None`] at both ends, so a
/// device's frames are what they were before the amendment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimReferences {
    /// Neither end sends or receives one: every device session, and any
    /// session not admitted through a handshake.
    None,
    /// An app connection's client: its uploads may carry one, and a start
    /// the worker sends never does.
    Sent,
    /// The worker serving an app connection's session: the starts it
    /// receives may carry one, and its own never do.
    Received,
}

impl ClaimReferences {
    /// The worker's end of a session it admitted under `principal`.
    pub fn worker(principal: &SessionPrincipal) -> Self {
        match principal {
            SessionPrincipal::Device { .. } => Self::None,
            SessionPrincipal::AppConnection { .. } => Self::Received,
        }
    }

    /// The client's end of a session its own credential admitted as
    /// `principal`.
    pub fn client(principal: &SessionPrincipal) -> Self {
        match principal {
            SessionPrincipal::Device { .. } => Self::None,
            SessionPrincipal::AppConnection { .. } => Self::Sent,
        }
    }
}

/// What the Store port says about one upload besides its bytes (TECHNICAL
/// §5.4 item 4). A Store's export answer may carry one for its unit; the
/// Reconciler passes it to the carrier, which writes `claim` into
/// `TransferStart.claim`. A device's Store never sets it, so its uploads
/// carry none.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct UploadContext {
    /// `SHA-256` of the claim handle the call presented for this unit.
    pub claim: Option<[u8; 32]>,
}

/// The reference is not a bearer, but it is never logged either: this says
/// only whether there is one.
impl std::fmt::Debug for UploadContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadContext")
            .field("claim", &self.claim.map(|_| "<present>"))
            .finish()
    }
}
