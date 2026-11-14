// SPDX-License-Identifier: MPL-2.0
//! Bounded owner observations. None of these values is execution authority.
use super::*;
use crate::AiProviderId;
use crate::bots::BotAccess;
use koru_delegate_schedule::ScheduleState;

pub const BOT_ROUTINE_LIST_BYTES: usize = 4 * 1024 * 1024;

pub fn validate_routine_list_request(
    bot: BotId,
    after: Option<ScheduleId>,
    limit: u8,
) -> Result<(), BotError> {
    if !(1..=16).contains(&limit)
        || bot.to_storage().parse::<BotId>().ok() != Some(bot)
        || after.is_some_and(|id| id.to_storage().parse::<ScheduleId>().ok() != Some(id))
    {
        return Err(BotError::Invalid);
    }
    Ok(())
}

/// Includes the command/response enum wrapper; counts before output allocation.
pub fn validate_routine_list_bytes<T: Serialize>(value: &T) -> Result<(), BotError> {
    bounded(value, BOT_ROUTINE_LIST_BYTES).map_err(|_| BotError::ContextTooLarge)
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotRoutineListPage {
    pub bot_id: BotId,
    pub bot_revision: u64,
    pub effective_model: BotRoutineModelObservation,
    #[cfg_attr(feature = "schema", schemars(length(max = 16)))]
    pub items: Vec<BotRoutineListItem>,
    pub next_schedule: Option<ScheduleId>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotRoutineModelObservation {
    Configured {
        provider: AiProviderId,
        requested_model: String,
    },
    Unavailable {},
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotRoutineListItem {
    pub schedule_id: ScheduleId,
    pub creation: BotRoutineCreationObservation,
    pub grant: BotRoutineGrantSummary,
    pub live: BotRoutineLiveObservation,
    pub local_conversations: BotRoutineLocalConversations,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotRoutineCreationObservation {
    pub request_id: BotRequestId,
    pub state: BotRoutineCreationState,
    pub refusal: Option<BotRoutineCreationRefusal>,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotRoutineGrantSummary {
    pub source_bot_policy_revision: u64,
    pub grant_revision: u64,
    pub access: BotAccess,
    pub selected_page_count: u16,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "observation", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotRoutineLiveObservation {
    Current {
        name: ScheduleName,
        definition_revision: u32,
        #[serde(with = "state")]
        #[cfg_attr(feature = "schema", schemars(with = "state::StrictState"))]
        state: ScheduleState,
    },
    NotLive {},
    Unavailable {},
}
impl From<RoutineOverviewObservation> for BotRoutineLiveObservation {
    fn from(value: RoutineOverviewObservation) -> Self {
        match value {
            RoutineOverviewObservation::Current {
                name,
                definition_revision,
                state,
            } => Self::Current {
                name,
                definition_revision,
                state,
            },
            RoutineOverviewObservation::NotLive => Self::NotLive {},
            RoutineOverviewObservation::Unavailable => Self::Unavailable {},
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotRoutineLocalConversations {
    Available {
        #[cfg_attr(feature = "schema", schemars(length(max = 4)))]
        items: Vec<BotRoutineLocalConversation>,
        more: bool,
    },
    Unavailable {},
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotRoutineLocalConversation {
    pub occurrence_ms: i64,
    pub run_id: RunId,
    pub chat_id: uuid::Uuid,
    pub attempt_id: uuid::Uuid,
    pub conversation: BotRoutineConversationVisibility,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotRoutineConversationVisibility {
    Available,
    Unavailable,
}

macro_rules! redacted {
    ($($ty:ty),*) => {$(impl std::fmt::Debug for $ty {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(concat!(stringify!($ty), "([REDACTED])"))
        }
    })*};
}
redacted!(
    BotRoutineListPage,
    BotRoutineListItem,
    BotRoutineModelObservation,
    BotRoutineLiveObservation
);
#[cfg(test)]
mod tests;

// The neutral state remains the field's owning type. Close only this public
// observation's wire representation, without changing ordinary Schedule APIs.
mod state {
    use super::*;
    #[derive(Serialize, Deserialize)]
    #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
    #[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
    pub(super) enum StrictState {
        Active {
            #[cfg_attr(feature = "schema", schemars(with = "String"))]
            owner: DeviceId,
        },
        Paused {
            #[cfg_attr(feature = "schema", schemars(with = "String"))]
            owner: DeviceId,
            reason: StrictReason,
        },
        HandoffPending {
            #[cfg_attr(feature = "schema", schemars(with = "String"))]
            owner: DeviceId,
            #[cfg_attr(feature = "schema", schemars(with = "Vec<String>"))]
            requests: Vec<HandoffId>,
        },
        Retired,
    }
    #[derive(Serialize, Deserialize)]
    #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
    #[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
    pub(super) enum StrictReason {
        Manual,
        ProviderNotDelegatable,
        ScopeUnavailable,
        ContextUnavailable {
            item: u32,
            stage: koru_delegate_core::ScheduleContextFailureStage,
        },
    }
    impl From<StrictReason> for SchedulePauseReason {
        fn from(value: StrictReason) -> Self {
            match value {
                StrictReason::Manual => Self::Manual,
                StrictReason::ProviderNotDelegatable => Self::ProviderNotDelegatable,
                StrictReason::ScopeUnavailable => Self::ScopeUnavailable,
                StrictReason::ContextUnavailable { item, stage } => {
                    Self::ContextUnavailable { item, stage }
                }
            }
        }
    }
    pub fn serialize<S: serde::Serializer>(value: &ScheduleState, s: S) -> Result<S::Ok, S::Error> {
        value.serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<ScheduleState, D::Error> {
        Ok(match StrictState::deserialize(d)? {
            StrictState::Active { owner } => ScheduleState::Active { owner },
            StrictState::Paused { owner, reason } => ScheduleState::Paused {
                owner,
                reason: reason.into(),
            },
            StrictState::HandoffPending { owner, requests } => {
                ScheduleState::HandoffPending { owner, requests }
            }
            StrictState::Retired => ScheduleState::Retired,
        })
    }
}
