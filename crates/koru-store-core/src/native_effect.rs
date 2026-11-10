// SPDX-License-Identifier: MPL-2.0
//! Local scoped operation fences. They prove effect liveness, never its result.
use crate::ids::{DocumentId, UserScopeId, WorkspaceId};
use crate::model::Timestamp;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NativeEffectKind {
    CreateDocument,
    EditDocument,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NativeEffectBinding {
    pub user_scope: UserScopeId,
    pub scope: String,
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub request_key: Uuid,
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    pub kind: NativeEffectKind,
    pub request_fingerprint: [u8; 32],
}
impl NativeEffectBinding {
    pub fn valid(&self) -> bool {
        !self.user_scope.as_uuid().is_nil()
            && !self.workspace.as_uuid().is_nil()
            && !self.document.as_uuid().is_nil()
            && !self.scope.is_empty()
            && self.scope.len() <= 256
            && self.request_key.get_version_num() == 4
            && self.request_key.get_variant() == uuid::Variant::RFC4122
    }
}
impl std::fmt::Debug for NativeEffectBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeEffectBinding")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeEffectFence {
    pub binding: NativeEffectBinding,
    pub fenced_at: Timestamp,
    pub drained_at: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NativeEffectDrainReceipt {
    pub binding: NativeEffectBinding,
    pub fenced_at: Timestamp,
    pub drained_at: Timestamp,
}
