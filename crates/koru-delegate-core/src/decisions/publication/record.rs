// SPDX-License-Identifier: MPL-2.0

//! Internal owner records shared by the ledger and publication worker. These
//! are deliberately not wire inputs. An observed version is not a causal proof:
//! the document adapter must obtain it from its checked application result.

use super::*;
use crate::BotOwner;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionProjectionRecord {
    pub identity: DecisionProjectionIdentity,
    pub target: RunProjectionTargetV1,
    pub state: DecisionProjectionState,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecisionProjectionState {
    Pending,
    Prepared {
        prepared: DecisionPreparedProjection,
    },
    Applied {
        prepared: DecisionPreparedProjection,
        observed_version: DecisionVersionVector,
        completed_at_ms: i64,
    },
    Superseded {
        by_sequence: DecisionRevision,
        completed_at_ms: i64,
    },
}

impl DecisionProjectionRecord {
    pub fn phase(&self) -> DecisionProjectionPhase {
        match self.state {
            DecisionProjectionState::Pending => DecisionProjectionPhase::Pending,
            DecisionProjectionState::Prepared { .. } => DecisionProjectionPhase::Prepared,
            DecisionProjectionState::Applied { .. } => DecisionProjectionPhase::Applied,
            DecisionProjectionState::Superseded { .. } => DecisionProjectionPhase::Superseded,
        }
    }

    pub fn prepared(&self) -> Option<&DecisionPreparedProjection> {
        match &self.state {
            DecisionProjectionState::Prepared { prepared }
            | DecisionProjectionState::Applied { prepared, .. } => Some(prepared),
            _ => None,
        }
    }
}

/// Adapter observation only. Stable bus facts come exclusively from the
/// retained typed target, never from an acknowledgment caller's replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionProjectionAcknowledgment {
    pub identity: DecisionProjectionIdentity,
    pub delta_digest: DecisionDigest,
    pub observed_version: DecisionVersionVector,
}

/// Optimistic local decision revision, not proof of current human authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionExpected {
    pub owner: BotOwner,
    pub run: RunId,
    pub decision: DecisionId,
    pub revision: DecisionRevision,
    pub binding_digest: DecisionDigest,
}

/// Exact local runtime-wait CAS; cleanup-only waits have no synthetic decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionWaitExpected {
    pub owner: BotOwner,
    pub run: RunId,
    pub wait: BotWaitId,
    pub revision: DecisionRevision,
    pub decision: Option<DecisionExpected>,
}

/// First-entry observation from the sole attached document owner. SQL repeats
/// physical placement and immutable request checks. Same-wait entries use the
/// retained predecessor. Re-entry after a completely settled prior wait uses
/// a newly durable canonical baseline covering all intervening ordinary writes,
/// while preserving the exact last marker and immutable physical subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionPublicationBase {
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    pub source_schema: DecisionSourceSchema,
    pub observed_created_at_ms: i64,
    /// Current attached document offset, checked again by the document mapper.
    /// This is not a separately retained SQL counter or history-size proof.
    pub timeline_len: u64,
    pub subject_digest: DecisionDigest,
    pub predecessor: DecisionProjectionPredecessor,
    pub bookkeeping: Option<DecisionSourceBookkeeping>,
}
