// SPDX-License-Identifier: MPL-2.0

//! The private-operation row (M5): one private effect reached a final stage.
//!
//! Content-free by construction. The fact holds identifiers, closed tokens, a
//! flag and a size; no field has a type that can hold an account label, a
//! connection name, an item title, a vendor reference, a link, a token, a
//! vendor message or content that was read. An open stage has no spelling
//! here at all: [`PrivateOutcome`] has no `prepared` or `dispatched` variant,
//! so the row can only say something that is final.

use koru_delegate_core::decisions::DecisionId;
use koru_delegate_core::{BotId, ConnectionId, PrivateEffectRefusal, PrivateOperationKind};
use koru_events::{Envelope, event_name};
use uuid::Uuid;

use crate::chain::{DraftExt, RunChain, Stamp, count, flag, id, token};
use crate::tokens::RowToken;

/// How one private effect ended. Each variant carries exactly what its stage
/// records, so a refusal without a code or a size on a refusal cannot be
/// built.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrivateOutcome {
    /// The vendor answered with the item. `withheld` means the content was
    /// dropped because access was revoked while the call was in flight.
    Confirmed { outcome_bytes: u32, withheld: bool },
    /// The operation was not performed, or the vendor positively rejected it.
    Refused { code: PrivateEffectRefusal },
    /// The call may have reached the vendor and no trustworthy answer was
    /// recorded. Final, and never resent.
    Unknown,
}

/// One private effect, settled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PrivateOperationSettled {
    pub bot: BotId,
    /// The effect's own id: the operation id its tool call carried.
    pub operation: Uuid,
    pub connection: ConnectionId,
    pub kind: PrivateOperationKind,
    pub outcome: PrivateOutcome,
    /// The decision that allowed it, when a person was asked.
    pub decision: Option<DecisionId>,
}

impl RunChain {
    /// `delegate.private_operation.settled` — published after the ledger
    /// committed the final stage. The orchestrator is the actor: it records
    /// what the row's dispatcher or resolution told the ledger. Every final
    /// stage is final, so one effect publishes this row once; a retry reuses
    /// the stamp's id and the journal deduplicates it.
    pub fn private_operation_settled(
        &self,
        stamp: Stamp,
        fact: PrivateOperationSettled,
    ) -> Envelope {
        let (refusal, withheld, outcome_bytes) = match fact.outcome {
            PrivateOutcome::Confirmed {
                outcome_bytes,
                withheld,
            } => (None, Some(withheld), Some(outcome_bytes)),
            PrivateOutcome::Refused { code } => (Some(code), None, None),
            PrivateOutcome::Unknown => (None, None, None),
        };
        let draft = self
            .row(
                event_name!("delegate.private_operation.settled"),
                self.orchestrator(),
            )
            .attr("bot", id(fact.bot.as_uuid()))
            .attr("operation", id(fact.operation))
            .attr("connection", id(fact.connection.as_uuid()))
            .attr("kind", token(fact.kind.token()))
            .attr("stage", token(fact.outcome.token()))
            .attr_if("refusal", refusal.map(|code| token(code.token())))
            .attr_if("withheld", withheld.map(flag))
            .attr_if(
                "decision",
                fact.decision.map(|decision| id(decision.as_uuid())),
            )
            .attr_if("outcome_bytes", outcome_bytes.map(count));
        self.finish(draft, stamp)
    }
}
