// SPDX-License-Identifier: MPL-2.0
//! Narrow owner projections. Retained creation evidence is not live permission.
use super::*;

/// Immutable creation receipt projection; mutable state is read separately.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotRoutineCreateResult {
    pub request_id: BotRequestId,
    pub bot_id: BotId,
    pub schedule_id: ScheduleId,
    pub state: BotRoutineCreationState,
    pub refusal: Option<BotRoutineCreationRefusal>,
}
impl From<&BotRoutineCreationReceipt> for BotRoutineCreateResult {
    fn from(receipt: &BotRoutineCreationReceipt) -> Self {
        Self {
            request_id: receipt.route.creation_request,
            bot_id: receipt.route.bot,
            schedule_id: receipt.route.schedule,
            state: receipt.state,
            refusal: receipt.refusal,
        }
    }
}

/// Current creation coordination status, never a current Schedule lifecycle,
/// grant body or occurrence list. Frozen revisions describe original creation
/// evidence and do not claim current effect permission or service availability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotRoutineCreationStatus {
    pub request_id: BotRequestId,
    pub bot_id: BotId,
    pub schedule_id: ScheduleId,
    pub observed_token: BotRoutineCreationObserved,
    pub refusal: Option<BotRoutineCreationRefusal>,
    /// The owner command is permitted by current state/lifecycle evidence;
    /// this is not a connectivity probe or a promise that recovery succeeds.
    pub can_retry: bool,
    pub source_bot_policy_revision: u64,
    pub grant_revision: u64,
}
#[cfg(test)]
mod tests;
