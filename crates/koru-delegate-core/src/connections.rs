// SPDX-License-Identifier: MPL-2.0

//! Neutral private-connection vocabulary (M5). Identifiers, digests, codes and
//! sizes only: no account label, no item title, no vendor reference, no link,
//! no secret and no content that was read can be spelled with these types.
//! A value here grants nothing; the ledger and the AI owner check authority.

use crate::decisions::{DecisionDigest, DecisionId, DecisionRevision};
use crate::{
    AgentTurnId, AttemptGeneration, ConnectionId, ConnectionItemId, PrivateDispatcherId,
    RunAttemptId, RunId, ScheduleId,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Most grants one Bot or one Routine may hold. The owning transaction also
/// enforces it; this bounds a decoded policy.
pub const CONNECTION_GRANT_LIMIT: usize = 16;

/// What one grant permits. A closed list: M5 qualifies one read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PrivateOperationKind {
    ReadItem,
}

/// A reference and a permission. Whether the connection can be used now is
/// checked at use time by its owner, never inferred from this value.
/// Ordered by connection, then operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ConnectionGrantRef {
    pub connection: ConnectionId,
    pub operation: PrivateOperationKind,
}

// The fixed ordered record the request fingerprint covers. It excludes the
// operation id, the connection revision and anything a model typed, so two
// calls that name the same item in different ways share a fingerprint.
#[derive(Serialize)]
struct RequestFingerprintV1 {
    version: u8,
    operation: PrivateOperationKind,
    connection: ConnectionId,
    item: ConnectionItemId,
    item_ref: DecisionDigest,
}

/// Recomputed by every owner from the proposal's own fields; never a caller's
/// assertion. Domain `koru.bot.private.request.v1`.
pub fn private_request_fingerprint(
    operation: PrivateOperationKind,
    connection: ConnectionId,
    item: ConnectionItemId,
    item_ref: DecisionDigest,
) -> [u8; 32] {
    // The record is five fixed scalars: it cannot exceed the bound or fail to
    // serialize, so a digest is always produced.
    crate::routine_digest_json(
        b"koru.bot.private.request.v1",
        &RequestFingerprintV1 {
            version: 1,
            operation,
            connection,
            item,
            item_ref,
        },
        1024,
    )
    .map(|digest| *digest.as_bytes())
    .unwrap_or([0; 32])
}

/// One read of one picked item, as the AI owner planned it. Ids and digests
/// only. Every field is required.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateOperationProposal {
    pub operation_id: Uuid,
    pub connection: ConnectionId,
    pub connection_revision: DecisionRevision,
    pub account: DecisionDigest,
    pub item: ConnectionItemId,
    pub item_ref: DecisionDigest,
    pub operation: PrivateOperationKind,
    pub request_fingerprint: [u8; 32],
}
impl PrivateOperationProposal {
    pub fn fingerprint(&self) -> [u8; 32] {
        private_request_fingerprint(self.operation, self.connection, self.item, self.item_ref)
    }
    /// Shape only: a v4 operation id and a fingerprint that recomputes.
    pub fn valid(&self) -> bool {
        self.operation_id.get_version_num() == 4
            && self.operation_id.get_variant() == uuid::Variant::RFC4122
            && self.request_fingerprint == self.fingerprint()
    }
    pub fn grant(&self) -> ConnectionGrantRef {
        ConnectionGrantRef {
            connection: self.connection,
            operation: self.operation,
        }
    }
}
impl std::fmt::Debug for PrivateOperationProposal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrivateOperationProposal")
            .field("operation_id", &self.operation_id)
            .field("operation", &self.operation)
            .finish_non_exhaustive()
    }
}

/// The immutable first consuming key of one private effect, as
/// `NativeResultKey` is for a page result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateEffectKey {
    pub run: RunId,
    pub operation_id: Uuid,
    pub attempt: RunAttemptId,
    pub generation: AttemptGeneration,
    pub turn: AgentTurnId,
}

/// Why a private operation was not performed. A closed list; no vendor text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateEffectRefusal {
    NotDispatched,
    AccessDenied,
    Revoked,
    WrongAccount,
    CredentialExpired,
    CredentialLocked,
    CredentialMissing,
    ItemUnavailable,
    DestinationChanged,
    VendorRejected,
    Unavailable,
}

/// `confirmed`, `refused` and `unknown` are final. Nothing leads back to
/// `prepared`, and nothing resends an `unknown` effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateEffectStage {
    Prepared,
    Dispatched,
    Confirmed,
    Refused,
    Unknown,
}
impl PrivateEffectStage {
    pub fn is_final(self) -> bool {
        matches!(self, Self::Confirmed | Self::Refused | Self::Unknown)
    }
}

/// What the row's own dispatcher records. Sizes and codes, never content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transition", rename_all = "snake_case", deny_unknown_fields)]
pub enum PrivateEffectTransition {
    Dispatched {},
    Confirmed { outcome_bytes: u32, withheld: bool },
    Refused { code: PrivateEffectRefusal },
    Unknown {},
}

/// `Execute` is answered once in the life of an effect: on the first fresh
/// commit that created it. Every replay is `ReconcileOnly`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateEffectDisposition {
    Execute,
    ReconcileOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateRoutineBinding {
    pub schedule: ScheduleId,
    pub grant_revision: u64,
}

/// The retained effect as its owner reports it. Every field is always
/// present; an absent value is `null`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateEffectView {
    pub key: PrivateEffectKey,
    /// The row's dispatcher, not the caller's.
    pub dispatcher: PrivateDispatcherId,
    pub decision: Option<DecisionId>,
    pub operation: PrivateOperationProposal,
    pub config_revision: u64,
    pub policy_revision: u64,
    pub routine: Option<PrivateRoutineBinding>,
    pub stage: PrivateEffectStage,
    pub refusal: Option<PrivateEffectRefusal>,
    pub outcome_bytes: Option<u32>,
    pub withheld: Option<bool>,
    pub disposition: PrivateEffectDisposition,
    /// True on exactly one reply: the advance that commits a confirmed,
    /// not-withheld read while both bound barriers still allow effects.
    pub deliver: bool,
}

#[cfg(test)]
mod tests;
