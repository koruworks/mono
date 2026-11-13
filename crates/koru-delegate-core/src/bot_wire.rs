// SPDX-License-Identifier: MPL-2.0

//! Strict serde adapters for the private Bot boundary. Ordinary session and
//! delegation decoders keep their existing compatibility behavior. Remote
//! derives construct the original types, so added source fields force review.
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Serialize, Deserialize)]
#[serde(remote = "crate::DelegationRequest", deny_unknown_fields)]
pub struct StrictDelegationRequest {
    pub run: RunId,
    #[serde(with = "StrictActorRef")]
    pub requested_by: ActorRef,
    pub objective: Objective,
    #[serde(deserialize_with = "deserialize_profile")]
    pub agent: SessionProfileKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<ModelName>,
    #[serde(with = "StrictWorkspaceScope")]
    pub scope: WorkspaceScope,
    #[serde(deserialize_with = "deserialize_contexts")]
    pub context: Vec<RunContextItem>,
    pub grants: BTreeSet<WorkspaceId>,
    pub executing_device: DeviceId,
    #[serde(default, with = "StrictRunOrigin")]
    pub origin: RunOrigin,
}

/// Closed profile spelling for private Bot/Routine DTOs. The ordinary
/// SessionProfileKey decoder retains its established forward compatibility.
#[doc(hidden)]
pub fn deserialize_profile<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<SessionProfileKey, D::Error> {
    #[derive(Deserialize)]
    #[serde(
        tag = "kind",
        content = "value",
        rename_all = "snake_case",
        deny_unknown_fields
    )]
    enum Profile {
        BuiltIn(String),
        Custom(uuid::Uuid),
    }
    match Profile::deserialize(d)? {
        Profile::BuiltIn(key) => key
            .parse()
            .map(SessionProfileKey::BuiltIn)
            .map_err(serde::de::Error::custom),
        Profile::Custom(key) => key
            .to_string()
            .parse()
            .map(SessionProfileKey::Custom)
            .map_err(serde::de::Error::custom),
    }
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "koru_session_protocol::PendingInput", deny_unknown_fields)]
pub struct StrictPendingInput {
    pub idempotency_key: IdempotencyKey,
    pub text: String,
    pub attachment_ids: Vec<AttachmentId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<koru_session_protocol::PathReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<koru_session_protocol::AgentModeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<koru_session_protocol::LaunchReasoning>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<koru_session_protocol::ModelName>,
}

#[derive(Serialize, Deserialize)]
#[serde(
    remote = "crate::ActorRef",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum StrictActorRef {
    Person { profile: ProfileId },
    Orchestrator { run: RunId },
    Agent { session: SessionId },
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "crate::WorkspaceScope",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum StrictWorkspaceScope {
    BotFolder {
        folder: BotWorkingFolder,
    },
    None {},
    ProjectRoot {
        project: ProjectId,
    },
    ProjectFolder {
        project: ProjectId,
        folder: RelativeFolder,
    },
    Temporary {},
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "crate::RunOrigin",
    tag = "origin",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum StrictRunOrigin {
    // An internally tagged unit variant ignores extra fields in serde even
    // with deny_unknown_fields. Empty-struct remote variants close that hole
    // while retaining the original unit variant and its serialized shape.
    Manual {},
    Scheduled {
        schedule: ScheduleId,
        occurrence_ms: i64,
        revision: u32,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "crate::RunContextItem",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum StrictRunContextItem {
    Document {
        workspace: WorkspaceId,
        document: DocumentId,
        version: DocumentVersionId,
    },
    Block {
        workspace: WorkspaceId,
        document: DocumentId,
        version: DocumentVersionId,
        node: BlockNodeId,
    },
    Blob {
        workspace: WorkspaceId,
        blob: BlobHash,
        blob_kind: BlobKind,
    },
}
fn deserialize_contexts<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<RunContextItem>, D::Error> {
    #[derive(Deserialize)]
    #[serde(transparent)]
    struct Item(#[serde(with = "StrictRunContextItem")] RunContextItem);
    Ok(Vec::<Item>::deserialize(deserializer)?
        .into_iter()
        .map(|item| item.0)
        .collect())
}

#[derive(Serialize, Deserialize)]
#[serde(
    remote = "koru_session_protocol::QuestionAnswerValue",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum StrictQuestionAnswerValue {
    Option { label: String },
    Text { text: String },
    Withheld {},
}
#[derive(Serialize, Deserialize)]
#[serde(remote = "koru_session_protocol::QuestionAnswer", deny_unknown_fields)]
pub struct StrictQuestionAnswer {
    pub question_id: String,
    #[serde(with = "StrictQuestionAnswerValue")]
    pub value: koru_session_protocol::QuestionAnswerValue,
}
pub fn deserialize_answers<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<QuestionAnswer>, D::Error> {
    #[derive(Deserialize)]
    #[serde(transparent)]
    struct Item(#[serde(with = "StrictQuestionAnswer")] QuestionAnswer);
    Ok(Vec::<Item>::deserialize(deserializer)?
        .into_iter()
        .map(|item| item.0)
        .collect())
}
