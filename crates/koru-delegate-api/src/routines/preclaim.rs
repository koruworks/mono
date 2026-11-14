// SPDX-License-Identifier: MPL-2.0

//! Correlated policy synchronization evidence, never live claim authority.
use super::*;
use koru_delegate_core::{BotAdmissionBarrier, CapturedBotPolicy, FreshBotPolicyProof};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SynchronizeRoutinePreclaim {
    pub slot: RoutineSlot,
    pub semantic_hash: RoutineDigest32,
    pub preparation_hash: RoutineDigest32,
}

/// Only a Prepared draft is eligible. Linked, Closed and missing drafts are
/// Stale refusals. The owner must recheck current canonical state before Ready;
/// this codec cannot prove that an acknowledgement is durable or current.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SynchronizeRoutinePreclaimReply {
    Ready {
        slot: RoutineSlot,
        semantic_hash: RoutineDigest32,
        preparation_hash: RoutineDigest32,
        admission: BotAdmissionBarrier,
        proof: FreshBotPolicyProof,
    },
    Refused {
        code: RoutineBridgeRefusal,
    },
}

impl std::fmt::Debug for SynchronizeRoutinePreclaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SynchronizeRoutinePreclaim([REDACTED])")
    }
}
impl std::fmt::Debug for SynchronizeRoutinePreclaimReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SynchronizeRoutinePreclaimReply([REDACTED])")
    }
}

fn policy_valid(policy: &CapturedBotPolicy) -> bool {
    policy.valid()
        && WorkspaceId::parse(&policy.workspace.to_storage()) == Some(policy.workspace)
        && policy
            .selected_pages
            .iter()
            .all(|id| koru_delegate_core::DocumentId::parse(&id.to_storage()) == Some(*id))
}

impl codec::sealed::Sealed for SynchronizeRoutinePreclaim {}
impl RoutineBridgePayload for SynchronizeRoutinePreclaim {
    fn valid(&self) -> bool {
        self.slot.valid()
    }
}
impl codec::sealed::Sealed for SynchronizeRoutinePreclaimReply {}
impl RoutineBridgePayload for SynchronizeRoutinePreclaimReply {
    fn valid(&self) -> bool {
        let Self::Ready {
            slot,
            admission,
            proof,
            ..
        } = self
        else {
            return true;
        };
        let Some(routine) = &proof.routine else {
            return false;
        };
        let global = &proof.global;
        slot.valid()
            && admission.owner == slot.owner
            && admission.bot == slot.bot
            && v4(admission.request.as_uuid())
            && !admission.paused
            && admission.epoch == global.config_revision
            && global.key.owner == slot.owner
            && global.key.bot == slot.bot
            && global.key.request.to_storage().parse() == Ok(global.key.request)
            && global.config_revision > 0
            && global.config_revision <= i64::MAX as u64
            && global.policy_revision > 0
            && global.policy_revision <= i64::MAX as u64
            && global.effects_allowed
            && policy_valid(&global.policy)
            && routine.valid()
            && routine.owner == slot.owner
            && routine.bot == slot.bot
            && routine.schedule == slot.schedule
            && routine.request.to_storage().parse() == Ok(routine.request)
            && routine.effects_allowed
            && policy_valid(&routine.policy)
            && routine.policy.workspace == global.policy.workspace
    }
}
impl RoutineBridgeReply<SynchronizeRoutinePreclaim> for SynchronizeRoutinePreclaimReply {
    fn matches_request(&self, request: &SynchronizeRoutinePreclaim) -> bool {
        self.valid()
            && request.valid()
            && match self {
                Self::Ready {
                    slot,
                    semantic_hash,
                    preparation_hash,
                    ..
                } => {
                    *slot == request.slot
                        && *semantic_hash == request.semantic_hash
                        && *preparation_hash == request.preparation_hash
                }
                Self::Refused { .. } => true,
            }
    }
}

#[cfg(test)]
mod tests;
