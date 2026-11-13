// SPDX-License-Identifier: MPL-2.0

//! Shared repository observations. Decoding these values never authenticates them.
use super::*;
use crate::WorkspaceId;

/// Retained AI preparation closure, shared with the private reverse bridge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotRoutineCloseReason {
    ExpiredUnclaimed,
    SlotSkipped,
    OtherWinner,
    ConfigurationChanged,
    AuthorityChanged,
    Stopped,
}

/// Retained deny-only routing identity, registered before Store creation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutineRouteIdentity {
    pub owner: BotOwner,
    pub bot: BotId,
    pub schedule: ScheduleId,
    pub workspace: WorkspaceId,
    pub creation_request: BotRequestId,
    pub seed_hash: RoutineDigest32,
    pub created_at_ms: i64,
}
impl RoutineRouteIdentity {
    pub fn valid(&self) -> bool {
        RoutineSlot {
            owner: self.owner,
            bot: self.bot,
            schedule: self.schedule,
            occurrence_ms: 0,
        }
        .valid()
            && v4(self.creation_request.as_uuid())
    }
    pub fn matches_slot(&self, slot: &RoutineSlot) -> bool {
        self.valid()
            && slot.valid()
            && self.owner == slot.owner
            && self.bot == slot.bot
            && self.schedule == slot.schedule
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutineTerminalReason {
    Stopped,
    ConfigurationChanged,
    AuthorityChanged,
    AlreadyTerminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoutineLinkState {
    Held {},
    Linked {},
    Terminal { reason: RoutineTerminalReason },
}
impl RoutineLinkState {
    /// Identity is checked separately; this only describes monotonic state.
    pub fn permits(self, next: Self) -> bool {
        self == next
            || matches!(
                (self, next),
                (Self::Held {}, Self::Linked {} | Self::Terminal { .. })
                    | (Self::Linked {}, Self::Terminal { .. })
            )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoutineLinkOutcome {
    Linked {},
    Terminal { reason: RoutineTerminalReason },
}
impl RoutineLinkOutcome {
    pub fn state(self) -> RoutineLinkState {
        match self {
            Self::Linked {} => RoutineLinkState::Linked {},
            Self::Terminal { reason } => RoutineLinkState::Terminal { reason },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutineLinkAcknowledgement {
    pub claim: RoutineClaimIdentity,
    pub outcome: RoutineLinkOutcome,
}
impl RoutineLinkAcknowledgement {
    pub fn matches_claim(&self, claim: &RoutineClaimIdentity) -> bool {
        self.claim.valid() && self.claim == *claim
    }
}

/// Complete durable winner, never mixed with a losing caller's candidate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutineClaimRecord {
    pub claim: RoutineClaimIdentity,
    pub submission_version: u8,
    pub submission: BotSubmission,
    pub link_state: RoutineLinkState,
    pub updated_at_ms: i64,
}
impl RoutineClaimRecord {
    pub fn valid(&self) -> bool {
        let p = &self.submission.provenance;
        self.claim.valid()
            && u32::from(self.submission_version) == ROUTINE_BOT_SUBMISSION_VERSION
            && self
                .submission
                .valid_for_version(u32::from(self.submission_version))
            && p.owner == self.claim.slot.owner
            && p.bot == self.claim.slot.bot
            && p.submission == self.claim.ids.submission
            && self.submission.request.run == self.claim.ids.run
            && p.chat_id == self.claim.ids.chat
            && p.ai_attempt_id == Some(self.claim.ids.attempt)
            && p.assistant_message_id == Some(self.claim.ids.assistant_message)
            && p.routine.is_some_and(|r| {
                r.schedule == self.claim.slot.schedule
                    && r.occurrence_ms == self.claim.slot.occurrence_ms
                    && r.definition == self.claim.definition
            })
    }
    pub fn accepts(&self, acknowledgement: &RoutineLinkAcknowledgement) -> bool {
        self.valid()
            && acknowledgement.matches_claim(&self.claim)
            && self.link_state.permits(acknowledgement.outcome.state())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoutineSkipOutcome {
    SkippedMissed {},
    SkippedBusy { holder: RunId },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutineSkippedClaim {
    pub slot: RoutineSlot,
    pub definition_revision: u32,
    /// Existing ledger row identity; no run_intents row is implied.
    pub planned_run: RunId,
    pub outcome: RoutineSkipOutcome,
    pub recorded_at_ms: i64,
}
impl RoutineSkippedClaim {
    pub fn valid(&self) -> bool {
        self.slot.valid()
            && self.definition_revision > 0
            && v4(self.planned_run.as_uuid())
            && match self.outcome {
                RoutineSkipOutcome::SkippedMissed {} => true,
                RoutineSkipOutcome::SkippedBusy { holder } => {
                    v4(holder.as_uuid()) && holder != self.planned_run
                }
            }
    }
}

/// Returned only by an authenticated read against the current execution owner.
/// Transport refusal/timeout is an error outside this enum, never Absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "observation", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoutineClaimObservation {
    Claimed {
        record: RoutineClaimRecord,
    },
    Skipped {
        record: RoutineSkippedClaim,
    },
    Absent {
        slot: RoutineSlot,
        observed_at_ms: i64,
    },
}
impl RoutineClaimObservation {
    pub fn slot(&self) -> &RoutineSlot {
        match self {
            Self::Claimed { record } => &record.claim.slot,
            Self::Skipped { record } => &record.slot,
            Self::Absent { slot, .. } => slot,
        }
    }
    pub fn valid(&self) -> bool {
        match self {
            Self::Claimed { record } => record.valid(),
            Self::Skipped { record } => record.valid(),
            Self::Absent { slot, .. } => slot.valid(),
        }
    }
}

#[cfg(test)]
mod tests;
