// SPDX-License-Identifier: MPL-2.0

//! Narrow external Bot commands. Request UUIDs belong to the local owner,
//! not to the client label. Retry an uncertain start with identical input.
use super::*;
use koru_ai_core::DocumentId;
use koru_ai_core::bots::BotId;
pub use koru_ai_core::bots::BotSummary;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListBotsRequest {
    pub workspace: WorkspaceId,
    #[serde(default)]
    pub cursor: Option<String>,
    pub limit: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListBotsResponse {
    pub bots: Vec<BotSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartBotTaskRequest {
    pub workspace: WorkspaceId,
    pub bot_id: BotId,
    #[schemars(with = "String")]
    pub request_id: Uuid,
    pub text: String,
    #[serde(default)]
    pub document_ids: Vec<DocumentId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadBotTaskRequest {
    pub workspace: WorkspaceId,
    pub bot_id: BotId,
    #[schemars(with = "String")]
    pub request_id: Uuid,
    pub attempt_id: InferenceAttemptId,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StopBotTaskRequest {
    pub workspace: WorkspaceId,
    pub bot_id: BotId,
    #[schemars(with = "String")]
    pub request_id: Uuid,
    pub attempt_id: InferenceAttemptId,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct BotTaskResult {
    pub document_id: DocumentId,
    pub workspace: WorkspaceId,
    pub title: String,
    /// The confirmed version, encoded as standard base64.
    pub version: String,
    pub confirmed_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BotResultsState {
    Preparing,
    Available,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ReadBotTaskResponse {
    pub attempt_id: InferenceAttemptId,
    pub status: AttemptStatus,
    /// None until terminal; terminal failures may contain a partial answer.
    pub answer: Option<String>,
    pub results_state: BotResultsState,
    pub results: Vec<BotTaskResult>,
    pub next_cursor: Option<String>,
}
