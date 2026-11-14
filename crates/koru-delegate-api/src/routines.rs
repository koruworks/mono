// SPDX-License-Identifier: MPL-2.0

//! Private reverse-bridge vocabulary and codec only. No HTTP handler, IPC
//! command or scheduler call is installed here. Every decoded identity is
//! evidence to compare with the owning service, never independent authority.
use koru_delegate_core::{
    BotRoutineCloseReason, HandoffId, RoutineClaimIdentity, RoutineDigest32,
    RoutineLinkAcknowledgement, RoutineLinkOutcome, RoutinePrepared, RoutineSlot,
    RoutineTerminalReason, WorkspaceId, WorkspaceScope,
};
use koru_delegate_schedule::{ScheduleContextPointer, ScheduleDefinition, ScheduleLifecycle};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod auth;
mod codec;
pub use auth::RoutineBridgeAuthority;
pub use codec::{
    RoutineBridgeCodecError, RoutineBridgeEnvelope, RoutineBridgePayload, RoutineBridgeReply,
};

pub const ROUTINE_BRIDGE_BODY_LIMIT: usize = 4 * 1024 * 1024;
/// One total transport deadline; this vocabulary does not start a timer.
pub const ROUTINE_BRIDGE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);
pub const ROUTINE_BRIDGE_METHOD: &str = "POST";
pub const ROUTINE_PREPARE_ROUTE: &str = "/internal/bots/routines/v1/prepare";
pub const ROUTINE_PREPARATION_ROUTE: &str = "/internal/bots/routines/v1/preparation";
pub const ROUTINE_FINALIZE_ROUTE: &str = "/internal/bots/routines/v1/finalize";
pub const ROUTINE_LINK_ROUTE: &str = "/internal/bots/routines/v1/link";

fn v4(id: Uuid) -> bool {
    id.get_version() == Some(uuid::Version::Random) && id.get_variant() == uuid::Variant::RFC4122
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareOccurrence {
    pub slot: RoutineSlot,
    pub workspace: WorkspaceId,
    #[serde(with = "koru_delegate_schedule::routine_wire::Definition")]
    pub schedule_definition: ScheduleDefinition,
    pub owner_handoff: Option<HandoffId>,
}
impl std::fmt::Debug for PrepareOccurrence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrepareOccurrence([REDACTED])")
    }
}
impl PrepareOccurrence {
    fn matches_prepared(&self, prepared: &RoutinePrepared) -> bool {
        // The initial request supplies a complete source definition, not a
        // caller-chosen digest. Its owning service must independently verify
        // and hash that definition. This codec checks repeated identity only.
        prepared.valid()
            && prepared.slot == self.slot
            && prepared.submission.workspace == self.workspace
            && prepared.definition.revision == self.schedule_definition.revision
            && prepared.definition.owner_handoff == self.owner_handoff
            && prepared.submission.request.objective == self.schedule_definition.template.objective
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadPreparation {
    pub slot: RoutineSlot,
    pub semantic_hash: RoutineDigest32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinalizeOccurrence {
    pub claim: RoutineClaimIdentity,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadOccurrenceLink {
    pub claim: RoutineClaimIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutineBridgeRefusal {
    Invalid,
    Unauthorized,
    Unavailable,
    Capacity,
    Conflict,
    Stale,
    Forbidden,
    ContextTooLarge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PrepareOccurrenceReply {
    Prepared {
        preparation: RoutinePrepared,
    },
    Closed {
        slot: RoutineSlot,
        semantic_hash: RoutineDigest32,
        reason: BotRoutineCloseReason,
    },
    Refused {
        code: RoutineBridgeRefusal,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReadPreparationReply {
    Prepared {
        preparation: RoutinePrepared,
    },
    Linked {
        slot: RoutineSlot,
        semantic_hash: RoutineDigest32,
        claim: RoutineClaimIdentity,
    },
    Closed {
        slot: RoutineSlot,
        semantic_hash: RoutineDigest32,
        reason: BotRoutineCloseReason,
    },
    Missing {
        slot: RoutineSlot,
        semantic_hash: RoutineDigest32,
    },
    Refused {
        code: RoutineBridgeRefusal,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FinalizeOccurrenceReply {
    Linked {
        claim: RoutineClaimIdentity,
    },
    Terminal {
        claim: RoutineClaimIdentity,
        reason: RoutineTerminalReason,
    },
    Refused {
        code: RoutineBridgeRefusal,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReadOccurrenceLinkReply {
    Linked {
        claim: RoutineClaimIdentity,
    },
    Terminal {
        claim: RoutineClaimIdentity,
        reason: RoutineTerminalReason,
    },
    Missing {
        claim: RoutineClaimIdentity,
    },
    Refused {
        code: RoutineBridgeRefusal,
    },
}

macro_rules! payload {
    ($name:ty, $this:ident, $valid:expr) => {
        impl codec::sealed::Sealed for $name {}
        impl RoutineBridgePayload for $name {
            fn valid(&$this) -> bool { $valid }
        }
    };
}
payload!(PrepareOccurrence, self, {
    let Some(association) = &self.schedule_definition.routine else {
        return false;
    };
    let template = &self.schedule_definition.template;
    let mut documents = std::collections::BTreeSet::new();
    self.slot.valid()
        && association.valid()
        && association.owner == self.slot.owner
        && association.bot == self.slot.bot
        && association.home == self.workspace
        && self.schedule_definition.revision > 0
        && self.schedule_definition.effective_at_ms <= self.slot.occurrence_ms
        && self.schedule_definition.lifecycle == ScheduleLifecycle::Active
        && self.owner_handoff.is_none_or(|id| v4(id.as_uuid()))
        && matches!(template.scope, WorkspaceScope::BotFolder { .. })
        && template.grants.len() == 1
        && template.grants.contains(&self.workspace)
        && template.context.len() <= 16
        && template.context.iter().all(|item| {
            matches!(item,
            ScheduleContextPointer::Document{workspace,document}
                if *workspace==self.workspace && documents.insert(*document))
        })
});
payload!(ReadPreparation, self, self.slot.valid());
payload!(FinalizeOccurrence, self, self.claim.valid());
payload!(ReadOccurrenceLink, self, self.claim.valid());
payload!(
    PrepareOccurrenceReply,
    self,
    match self {
        Self::Prepared { preparation } => preparation.valid(),
        Self::Closed { slot, .. } => slot.valid(),
        Self::Refused { .. } => true,
    }
);
payload!(
    ReadPreparationReply,
    self,
    match self {
        Self::Prepared { preparation } => preparation.valid(),
        Self::Linked { slot, claim, .. } => slot.valid() && claim.valid() && *slot == claim.slot,
        Self::Closed { slot, .. } | Self::Missing { slot, .. } => slot.valid(),
        Self::Refused { .. } => true,
    }
);
payload!(
    FinalizeOccurrenceReply,
    self,
    match self {
        Self::Linked { claim } | Self::Terminal { claim, .. } => claim.valid(),
        Self::Refused { .. } => true,
    }
);
payload!(
    ReadOccurrenceLinkReply,
    self,
    match self {
        Self::Linked { claim } | Self::Terminal { claim, .. } | Self::Missing { claim } =>
            claim.valid(),
        Self::Refused { .. } => true,
    }
);

impl RoutineBridgeReply<PrepareOccurrence> for PrepareOccurrenceReply {
    fn matches_request(&self, request: &PrepareOccurrence) -> bool {
        self.valid()
            && request.valid()
            && match self {
                Self::Prepared { preparation } => request.matches_prepared(preparation),
                Self::Closed { slot, .. } => *slot == request.slot,
                Self::Refused { .. } => true,
            }
    }
}
impl RoutineBridgeReply<ReadPreparation> for ReadPreparationReply {
    fn matches_request(&self, request: &ReadPreparation) -> bool {
        self.valid()
            && request.valid()
            && match self {
                Self::Prepared { preparation } => {
                    preparation.slot == request.slot
                        && preparation.semantic_hash == request.semantic_hash
                }
                Self::Linked {
                    slot,
                    semantic_hash,
                    ..
                }
                | Self::Closed {
                    slot,
                    semantic_hash,
                    ..
                }
                | Self::Missing {
                    slot,
                    semantic_hash,
                } => *slot == request.slot && *semantic_hash == request.semantic_hash,
                Self::Refused { .. } => true,
            }
    }
}
impl RoutineBridgeReply<FinalizeOccurrence> for FinalizeOccurrenceReply {
    fn matches_request(&self, request: &FinalizeOccurrence) -> bool {
        self.valid()
            && request.valid()
            && match self {
                Self::Linked { claim } | Self::Terminal { claim, .. } => *claim == request.claim,
                Self::Refused { .. } => true,
            }
    }
}
impl RoutineBridgeReply<ReadOccurrenceLink> for ReadOccurrenceLinkReply {
    fn matches_request(&self, request: &ReadOccurrenceLink) -> bool {
        self.valid()
            && request.valid()
            && match self {
                Self::Linked { claim } | Self::Terminal { claim, .. } | Self::Missing { claim } => {
                    *claim == request.claim
                }
                Self::Refused { .. } => true,
            }
    }
}

impl FinalizeOccurrenceReply {
    /// Shared identity observation only. A later owning service must read the
    /// positive durable claim independently before finalizing or dispatching.
    pub fn acknowledgement(&self) -> Option<RoutineLinkAcknowledgement> {
        match self {
            Self::Linked { claim } => Some(RoutineLinkAcknowledgement {
                claim: claim.clone(),
                outcome: RoutineLinkOutcome::Linked {},
            }),
            Self::Terminal { claim, reason } => Some(RoutineLinkAcknowledgement {
                claim: claim.clone(),
                outcome: RoutineLinkOutcome::Terminal { reason: *reason },
            }),
            Self::Refused { .. } => None,
        }
    }
}
impl ReadOccurrenceLinkReply {
    pub fn acknowledgement(&self) -> Option<RoutineLinkAcknowledgement> {
        match self {
            Self::Linked { claim } => Some(RoutineLinkAcknowledgement {
                claim: claim.clone(),
                outcome: RoutineLinkOutcome::Linked {},
            }),
            Self::Terminal { claim, reason } => Some(RoutineLinkAcknowledgement {
                claim: claim.clone(),
                outcome: RoutineLinkOutcome::Terminal { reason: *reason },
            }),
            Self::Missing { .. } | Self::Refused { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests;
