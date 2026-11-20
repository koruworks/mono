// SPDX-License-Identifier: MPL-2.0
//! The owner's controls of one Bot Routine (P4.3): what a person may ask for
//! and what is answered. None of these values is execution authority: the
//! delegate's schedule coordinator owns the Schedule write and the claim, and
//! the AI worker owns the Routine's access.
use super::*;
use crate::bots::BotAccess;
use koru_delegate_schedule::ScheduleState;

/// What `set_routine_lifecycle` asks for. Pause is not Stop: a Run that is
/// already claimed finishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotRoutineLifecycleAction {
    Enable,
    Pause,
}

/// The control a `routine_control` reply answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotRoutineControlAction {
    Enable,
    Pause,
    Edit,
}
impl From<BotRoutineLifecycleAction> for BotRoutineControlAction {
    fn from(action: BotRoutineLifecycleAction) -> Self {
        match action {
            BotRoutineLifecycleAction::Enable => Self::Enable,
            BotRoutineLifecycleAction::Pause => Self::Pause,
        }
    }
}

/// The Routine after one enable, pause or edit. `applied` is answered only
/// after the coordinator's commit was acknowledged; `already_applied` is the
/// exact replay of that commit, after a lost reply.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotRoutineControl {
    pub schedule_id: ScheduleId,
    pub action: BotRoutineControlAction,
    pub outcome: RoutineControlOutcome,
    #[cfg_attr(feature = "schema", schemars(range(min = 1)))]
    pub definition_revision: u32,
    #[serde(with = "super::listing::state")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "super::listing::state::StrictState")
    )]
    pub state: ScheduleState,
    pub next: RoutineNextObservation,
}

/// The Routine's access after an edit. There is no pending state: the edit
/// is answered only once its policy barrier is installed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotRoutineAccess {
    pub schedule_id: ScheduleId,
    pub grant_revision: u64,
    pub access: BotAccess,
    pub selected_page_count: u16,
}
impl BotRoutineAccess {
    pub fn of(schedule: ScheduleId, summary: &BotRoutineGrantSummary) -> Self {
        Self {
            schedule_id: schedule,
            grant_revision: summary.grant_revision,
            access: summary.access,
            selected_page_count: summary.selected_page_count,
        }
    }
}

/// Why a Routine cannot be turned on or tested now, in the order they are
/// reported. Checked from current facts each time; never stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotRoutineNotReadyReason {
    BotPaused,
    BotArchived,
    CreationNotBound,
    AccessUnavailable,
    AccessPending,
}
impl BotRoutineNotReadyReason {
    pub const fn token(self) -> &'static str {
        match self {
            Self::BotPaused => "bot_paused",
            Self::BotArchived => "bot_archived",
            Self::CreationNotBound => "creation_not_bound",
            Self::AccessUnavailable => "access_unavailable",
            Self::AccessPending => "access_pending",
        }
    }
}

/// The state an owner control left the Routine in, as the coordinator
/// answered it. An owner control only ever writes a manual pause, and only
/// the Schedule's current owner answers one.
pub fn routine_control_state(lifecycle: RoutineControlLifecycle, owner: DeviceId) -> ScheduleState {
    match lifecycle {
        RoutineControlLifecycle::Active => ScheduleState::Active { owner },
        RoutineControlLifecycle::Paused => ScheduleState::Paused {
            owner,
            reason: SchedulePauseReason::Manual,
        },
    }
}

impl RoutineThisComputer {
    /// This computer with the zone name its system reports now. A name the
    /// compiled zone database does not know is no zone: the read then stays
    /// `owner_local`.
    pub fn detected(device: DeviceId, iana: &str) -> Option<Self> {
        Some(Self {
            device,
            zone: koru_delegate_schedule::CanonicalZone::parse(iana).ok()?,
        })
    }
}

/// The public bound of one control command or reply, enum wrapper included.
pub const BOT_ROUTINE_CONTROL_BYTES: usize = 256 * 1024;
pub fn validate_routine_control_bytes<T: Serialize>(value: &T) -> Result<(), BotError> {
    bounded(value, BOT_ROUTINE_CONTROL_BYTES).map_err(|_| BotError::ContextTooLarge)
}

impl std::fmt::Debug for BotRoutineControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotRoutineControl([REDACTED])")
    }
}
