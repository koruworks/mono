// SPDX-License-Identifier: MPL-2.0

//! The child-request rows (M6): one Bot Run asked another Bot, and one
//! committed change of that request.
//!
//! Content-free by construction. A row holds identifiers, a revision and
//! closed tokens; no field has a type that can hold task text, a title, a
//! name, a path, a page id, a version id, elapsed time or a deadline. Both
//! rows are wakes: a reader reads the requests from the delegate, and no
//! subscriber starts work from a row. The chain is the parent Run's, so
//! `run` is the parent Run.

use koru_delegate_core::children::{ChildClock, ChildRequestState, ChildSettledBy};
use koru_delegate_core::{BotId, ChildRequestId, RunId};
use koru_events::{Envelope, event_name};

use crate::chain::{DraftExt, RunChain, Stamp, count, id, token};
use crate::tokens::RowToken;

/// One request, as both rows name it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ChildRequestRef {
    pub request: ChildRequestId,
    pub child_run: RunId,
    pub parent_bot: BotId,
    pub recipient_bot: BotId,
    /// The request's committed revision this row reports.
    pub revision: u64,
}

/// What a later revision of a request shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ChildRequestChanged {
    pub request: ChildRequestRef,
    pub state: ChildRequestState,
    pub clock: ChildClock,
    /// Who settled it; present exactly for a settled request.
    pub settled_by: Option<ChildSettledBy>,
}

impl RunChain {
    fn child_row(&self, name: koru_events::EventName, fact: ChildRequestRef) -> koru_events::Draft {
        self.row(name, self.orchestrator())
            .attr("request", id(fact.request.as_uuid()))
            .attr("child_run", id(fact.child_run.as_uuid()))
            .attr("parent_bot", id(fact.parent_bot.as_uuid()))
            .attr("recipient_bot", id(fact.recipient_bot.as_uuid()))
            .attr("revision", count(fact.revision))
    }

    /// `delegate.child.requested` — published after the transaction that
    /// admitted the ask, once per request.
    pub fn child_requested(&self, stamp: Stamp, fact: ChildRequestRef) -> Envelope {
        let draft = self.child_row(event_name!("delegate.child.requested"), fact);
        self.finish(draft, stamp)
    }

    /// `delegate.child.changed` — published after every later committed
    /// change of a request: its clock paused or restarted, or it settled.
    pub fn child_changed(&self, stamp: Stamp, fact: ChildRequestChanged) -> Envelope {
        let draft = self
            .child_row(event_name!("delegate.child.changed"), fact.request)
            .attr("state", token(fact.state.token()))
            .attr("clock", token(fact.clock.token()))
            .attr_if(
                "settled_by",
                fact.settled_by.map(|settled| token(settled.token())),
            );
        self.finish(draft, stamp)
    }
}
