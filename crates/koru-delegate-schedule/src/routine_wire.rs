// SPDX-License-Identifier: MPL-2.0

//! Closed private seed/capture objects leave legacy Schedule decoders unchanged.
use crate::{
    DelegationTemplate, RecurrenceRule, RoutineAssociation, ScheduleContextFailureStage,
    ScheduleContextPointer, ScheduleDefinition, ScheduleHeader, ScheduleLifecycle,
    SchedulePauseReason,
};
use koru_delegate_core::{
    ActorRef, BlobHash, BlobKind, BlockNodeId, DeviceId, DocumentId, ModelName, Objective,
    ScheduleId, SessionProfileKey, WorkspaceId, WorkspaceScope,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Serialize, Deserialize)]
#[serde(remote = "ScheduleHeader", deny_unknown_fields)]
pub struct Header {
    pub schedule: ScheduleId,
    pub created_at_ms: i64,
    pub initial_owner: DeviceId,
    #[serde(with = "Definition")]
    pub definition: ScheduleDefinition,
}
#[derive(Serialize, Deserialize)]
#[serde(remote = "ScheduleDefinition", deny_unknown_fields)]
pub struct Definition {
    pub revision: u32,
    pub effective_at_ms: i64,
    #[serde(with = "Template")]
    pub template: DelegationTemplate,
    pub recurrence: RecurrenceRule,
    #[serde(with = "Lifecycle")]
    pub lifecycle: ScheduleLifecycle,
    pub routine: Option<RoutineAssociation>,
}
#[derive(Serialize, Deserialize)]
#[serde(remote = "DelegationTemplate", deny_unknown_fields)]
struct Template {
    #[serde(with = "koru_delegate_core::bot_wire::StrictActorRef")]
    pub requested_by: ActorRef,
    pub objective: Objective,
    #[serde(deserialize_with = "koru_delegate_core::bot_wire::deserialize_profile")]
    pub agent: SessionProfileKey,
    pub requested_model: Option<ModelName>,
    #[serde(with = "koru_delegate_core::bot_wire::StrictWorkspaceScope")]
    pub scope: WorkspaceScope,
    pub grants: BTreeSet<WorkspaceId>,
    #[serde(deserialize_with = "contexts")]
    pub context: Vec<ScheduleContextPointer>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Context {
    Document {
        workspace: WorkspaceId,
        document: DocumentId,
    },
    Block {
        workspace: WorkspaceId,
        document: DocumentId,
        node: BlockNodeId,
    },
    Blob {
        workspace: WorkspaceId,
        blob: BlobHash,
        blob_kind: BlobKind,
    },
}
fn contexts<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Vec<ScheduleContextPointer>, D::Error> {
    Vec::<Context>::deserialize(d).map(|items| {
        items
            .into_iter()
            .map(|item| match item {
                Context::Document {
                    workspace,
                    document,
                } => ScheduleContextPointer::Document {
                    workspace,
                    document,
                },
                Context::Block {
                    workspace,
                    document,
                    node,
                } => ScheduleContextPointer::Block {
                    workspace,
                    document,
                    node,
                },
                Context::Blob {
                    workspace,
                    blob,
                    blob_kind,
                } => ScheduleContextPointer::Blob {
                    workspace,
                    blob,
                    blob_kind,
                },
            })
            .collect()
    })
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "ScheduleLifecycle",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Lifecycle {
    Active,
    Paused {
        #[serde(with = "PauseReason")]
        reason: SchedulePauseReason,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "SchedulePauseReason",
    tag = "reason",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum PauseReason {
    Manual {},
    ProviderNotDelegatable {},
    ScopeUnavailable {},
    ContextUnavailable {
        item: u32,
        stage: ScheduleContextFailureStage,
    },
}
