// SPDX-License-Identifier: MPL-2.0

//! Bot control views shared with the native window adapter.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotOperationKind {
    UpdateAccess,
    Pause,
    Resume,
}
impl BotOperationKind {
    pub fn token(self) -> &'static str {
        match self {
            Self::UpdateAccess => "update_access",
            Self::Pause => "pause",
            Self::Resume => "resume",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "update_access" => Some(Self::UpdateAccess),
            "pause" => Some(Self::Pause),
            "resume" => Some(Self::Resume),
            _ => None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotOperationStatus {
    Pending,
    Accepted,
    Rejected,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotOperation {
    pub request_id: BotRequestId,
    pub bot_id: BotId,
    pub kind: BotOperationKind,
    pub run_id: Option<RunId>,
    pub status: BotOperationStatus,
}
/// A change to a Bot that is still being delivered to its execution owner:
/// what a change of the Bot answered `bot_operation_pending` waits on. Reads
/// fill it in (`read`, `list`, `list_lifecycle`); it is never stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotPendingChange {
    /// The change's own operation, readable with `read_operation`. It may be
    /// one the AI owner made itself to deliver the same change: a pause or
    /// resume its execution owner refused for good as conflicting
    /// (`bot_conflict`, `bot_stale`) is settled `rejected` and offered again
    /// under a new request, so the client's own request then reads
    /// `rejected` while this one names the renewal.
    pub request_id: BotRequestId,
    /// `pause`, `resume` or `update_access`.
    pub kind: BotOperationKind,
    /// When the change was made, in epoch milliseconds (UTC).
    pub since_ms: i64,
    /// The stable code of the last failed or refused attempt to deliver it
    /// since the AI owner last started (such as `bot_unavailable` when the
    /// execution owner could not be reached). Omitted when there was none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_answer: Option<String>,
}
