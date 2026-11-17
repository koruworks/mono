// SPDX-License-Identifier: MPL-2.0

//! Port values of child requests (M6, delegate IPC 19): what a child wait
//! publishes for its parent Run's tool classification, and the content-free
//! fact behind `delegate.child.requested` and `delegate.child.changed`.

use koru_delegate_core::children::{
    ChildClock, ChildRequestReceipt, ChildRequestState, ChildSettledBy,
};
use koru_delegate_core::decisions::{BotWaitId, BotWaitPhase};
use koru_delegate_core::{BotId, ChildRequestId, RunId};

/// A parent Run's open child wait as its tool classification carries it:
/// the wait, its phase and how many of its requests are still open. Published
/// from the ask until the successor's continuation input is acknowledged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildWaitMark {
    pub wait: BotWaitId,
    pub phase: BotWaitPhase,
    pub open_requests: u8,
}

/// One committed revision of one child request, as its event carries it.
/// Identifiers, closed tokens and a revision: no task text, title, name,
/// path, page id, version id, elapsed time or deadline can be held here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildRequestFact {
    pub request: ChildRequestId,
    pub parent_run: RunId,
    pub child_run: RunId,
    pub parent_bot: BotId,
    pub recipient_bot: BotId,
    pub revision: u64,
    pub state: ChildRequestState,
    pub clock: ChildClock,
    pub settled_by: Option<ChildSettledBy>,
    /// Whether this is the request's first committed revision (the ask).
    pub requested: bool,
}

impl ChildRequestFact {
    /// The fact of the revision `receipt` shows. `requested` is true only for
    /// the ask's own commit.
    pub fn of(receipt: &ChildRequestReceipt, requested: bool) -> Self {
        Self {
            request: receipt.child_request,
            parent_run: receipt.parent.run,
            child_run: receipt.child_run,
            parent_bot: receipt.parent.bot,
            recipient_bot: receipt.recipient,
            revision: receipt.revision.get(),
            state: receipt.state,
            clock: receipt.clock,
            settled_by: receipt.settled_by,
            requested,
        }
    }

    /// The one event id of this revision of this request. Derived from the
    /// request and the revision, so a retry after a lost answer and a later
    /// process publish the same id and the journal keeps one row.
    pub fn event_id(&self) -> koru_delegate_core::EventId {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"koru.bot.child.request.event.v1");
        hasher.update(self.request.as_uuid().as_bytes());
        hasher.update(&self.revision.to_be_bytes());
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
        koru_delegate_core::EventId(uuid::Builder::from_random_bytes(bytes).into_uuid())
    }
}
