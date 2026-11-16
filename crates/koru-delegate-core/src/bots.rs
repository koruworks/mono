// SPDX-License-Identifier: MPL-2.0

//! Bot admission and result identities. Authority is supplied by the trusted AI
//! owner, never inferred from these serializable values or a caller label.
mod boot;
mod memory;
mod native;
mod refusal;
mod routine;
pub use boot::*;
pub use memory::*;
pub use native::*;
pub use refusal::BotInputRefusal;
pub use routine::*;

use crate::{
    AgentTurnId, AttemptGeneration, BotId, BotRequestId, DeviceId, DocumentId, RunAttemptId, RunId,
    SessionId, WorkspaceId,
};
pub use koru_store_core::UserScopeId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotOwner {
    pub user_scope: UserScopeId,
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub device: DeviceId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotProvenance {
    pub owner: BotOwner,
    pub bot: BotId,
    pub submission: BotRequestId,
    /// These UUIDs refer to existing AI identities. They are never minted or
    /// converted from a Bot/Run identity; keeping their owners in AI avoids a cycle.
    pub chat_id: Uuid,
    pub ai_attempt_id: Option<Uuid>,
    pub assistant_message_id: Option<Uuid>,
    pub config_revision: u64,
    pub policy_revision: u64,
    pub continued_from: Option<RunId>,
    /// Absent for the original Interactive payload. Retained identity alone
    /// never authorizes a Routine claim or changes its current access policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routine: Option<RoutineProvenance>,
    /// The Bot Run that asked for this one (M6). **Omitted from the
    /// serialized form whenever it is absent**, so a submission or a header
    /// without a parent keeps exactly its earlier bytes and every digest
    /// computed over them. It is provenance: it grants nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ChildParent>,
}
/// Who asked for a child Run (M6). Identities only; never authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ChildParent {
    pub run: RunId,
    pub bot: BotId,
    /// The asking Bot's conversation. An existing AI identity, as `chat_id`.
    pub chat_id: Uuid,
    pub child_request: crate::ChildRequestId,
}
impl BotProvenance {
    pub fn valid(&self) -> bool {
        self.config_revision > 0
            && self.config_revision <= i64::MAX as u64
            && self.policy_revision > 0
            && self.policy_revision <= i64::MAX as u64
            && v4(self.chat_id)
            && self.ai_attempt_id.is_none_or(v4)
            && self.assistant_message_id.is_none_or(v4)
            && self.ai_attempt_id.is_some() == self.assistant_message_id.is_some()
            && self.routine.is_none_or(|routine| {
                routine.valid()
                    && self.ai_attempt_id.is_some()
                    && self.continued_from.is_none()
                    && v4(self.bot.as_uuid())
                    && v4(self.submission.as_uuid())
            })
            && self.parent.is_none_or(|parent| {
                // A child is ordinary interactive work of its recipient: never
                // a Routine occurrence, never a continuation, never of itself.
                self.routine.is_none()
                    && self.continued_from.is_none()
                    && parent.bot != self.bot
                    && v4(parent.chat_id)
                    && v4(parent.run.as_uuid())
                    && v4(parent.bot.as_uuid())
                    && v4(parent.child_request.as_uuid())
            })
    }
}
fn v4(id: Uuid) -> bool {
    id.get_version() == Some(uuid::Version::Random) && id.get_variant() == uuid::Variant::RFC4122
}

/// Submitted only over the dedicated authenticated Bot port. The ordinary
/// delegation request deliberately has no Bot authority field.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotSubmission {
    pub provenance: BotProvenance,
    pub policy: CapturedBotPolicy,
    #[serde(with = "crate::bot_wire::StrictDelegationRequest")]
    #[cfg_attr(feature = "schema", schemars(with = "crate::DelegationRequest"))]
    pub request: crate::DelegationRequest,
    pub workspace: WorkspaceId,
    /// Current task input only. Never place assembled prior conversation
    /// history here: AI supplies that transiently through BeginTurn after
    /// observation is ready. The delegate remains controller owner.
    #[serde(with = "crate::bot_wire::StrictPendingInput")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "koru_session_protocol::PendingInput")
    )]
    pub input: koru_session_protocol::PendingInput,
}
impl std::fmt::Debug for BotSubmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotSubmission")
            .field("provenance", &self.provenance)
            .field("run", &self.request.run)
            .finish_non_exhaustive()
    }
}
impl BotSubmission {
    pub fn valid(&self) -> bool {
        self.valid_for_version(INTERACTIVE_BOT_SUBMISSION_VERSION)
    }

    /// Shape validation for an explicit envelope owner. Existing ordinary
    /// admission keeps calling `valid`, which never admits Routine work.
    pub fn valid_for_version(&self, version: u32) -> bool {
        let cohort = match (version, &self.provenance.routine) {
            (INTERACTIVE_BOT_SUBMISSION_VERSION, None) => {
                matches!(self.request.origin, crate::RunOrigin::Manual)
            }
            (ROUTINE_BOT_SUBMISSION_VERSION, Some(routine)) => {
                routine.matches_origin(&self.request.origin)
                    && v4(self.request.run.as_uuid())
                    && self
                        .request
                        .context
                        .iter()
                        .all(|pin| pin.workspace() == self.workspace)
            }
            _ => false,
        };
        self.provenance.valid()
            && self.policy.valid()
            && self.workspace == self.policy.workspace
            && self.request.executing_device == self.provenance.owner.device
            && cohort
            && matches!(self.request.scope, crate::WorkspaceScope::BotFolder { .. })
            && self.request.grants.len() == 1
            && self.request.grants.contains(&self.workspace)
            && self.provenance.continued_from != Some(self.request.run)
            && self
                .provenance
                .parent
                .is_none_or(|parent| parent.run != self.request.run)
    }
}

/// Durable AI-owned pause mirrored at the execution owner before acknowledgement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotAdmissionBarrier {
    pub owner: BotOwner,
    pub bot: BotId,
    pub request: BotRequestId,
    pub epoch: u64,
    pub paused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotRunKey {
    pub owner: BotOwner,
    pub bot: BotId,
    pub run: RunId,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotSubmissionKey {
    pub owner: BotOwner,
    pub bot: BotId,
    pub request: BotRequestId,
}
/// Local reservation facts; canonical status stays in the Run document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotAdmissionState {
    Queued,
    Reserved,
    Executing,
    Settled,
    Refused,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotAdmissionView {
    pub key: BotSubmissionKey,
    pub run: RunId,
    pub state: BotAdmissionState,
    pub queue_sequence: u64,
    pub fence: Option<BotExecutionFence>,
}

/// One admission of a Bot that has not settled: reserved or executing (at
/// most one per Bot) or queued. `held` is the session owner's answer to
/// whether a person holds the Run's session or asked for it; it is a
/// reason to wait, never permission to end anything.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotUnsettledAdmission {
    pub admission: BotAdmissionView,
    pub held: bool,
}

/// The work of one Bot its execution owner has not settled, read from the
/// durable ledger every time it is asked (private IPC 12). The reserved or
/// executing admission comes first, then queued ones in queue order. `more`
/// says the queued page was cut at the caller's limit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotUnsettledWork {
    pub admissions: Vec<BotUnsettledAdmission>,
    pub more: bool,
}
impl BotUnsettledWork {
    /// The owner reports nothing reserved, executing or queued for the Bot.
    pub fn is_settled(&self) -> bool {
        self.admissions.is_empty() && !self.more
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotAccess {
    Default,
    Workspace,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CapturedBotPolicy {
    pub workspace: WorkspaceId,
    pub access: BotAccess,
    pub selected_pages: BTreeSet<DocumentId>,
    /// Explicit accepted destination, never inferred from the Run's location.
    pub destination: WorkspaceId,
    /// Private connection grants (M5). **Omitted from the serialized form
    /// whenever it is empty**, so a policy without a grant keeps exactly its
    /// pre-M5 bytes and every digest computed over them (both policy
    /// barriers, a Routine grant, a decision binding, a stored submission).
    /// This is the only field of this type that may ever be skipped.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub connections: BTreeSet<crate::ConnectionGrantRef>,
}
impl CapturedBotPolicy {
    pub fn valid(&self) -> bool {
        self.workspace == self.destination
            && self.selected_pages.len() <= 1024
            && self.connections.len() <= crate::CONNECTION_GRANT_LIMIT
    }
    /// Whether this policy holds the grant. A reference only: the connection's
    /// current state is its owner's to check at use time.
    pub fn grants_connection(&self, grant: &crate::ConnectionGrantRef) -> bool {
        self.connections.contains(grant)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotExecutionFence {
    pub owner: BotOwner,
    pub bot: BotId,
    pub run: RunId,
    pub attempt: RunAttemptId,
    pub generation: AttemptGeneration,
    pub session: SessionId,
    pub turn: Option<AgentTurnId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NativeResultKind {
    CreatedPage,
    EditedPage,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NativeResultReference {
    pub operation_id: Uuid,
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    pub version: Vec<u8>,
    pub title: String,
    pub confirmed_at_ms: i64,
    pub kind: NativeResultKind,
    /// The immutable Store version this save produced (M6). **Omitted from
    /// the serialized form whenever it is absent**, so a reference written
    /// before it existed keeps exactly its bytes. Absent means the result can
    /// never be handed over as a pinned page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "Option<String>"))]
    pub version_id: Option<crate::DocumentVersionId>,
}
impl std::fmt::Debug for NativeResultReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeResultReference")
            .field("operation_id", &self.operation_id)
            .field("workspace", &self.workspace)
            .field("document", &self.document)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}
impl NativeResultReference {
    pub fn valid(&self) -> bool {
        v4(self.operation_id)
            && !self.version.is_empty()
            && self.version.len() <= 65536
            && !self.title.trim().is_empty()
            && self.title.len() <= 256
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotDeliveryReceipt {
    pub run: RunId,
    pub attempt: RunAttemptId,
    pub generation: AttemptGeneration,
    pub chat_id: Uuid,
    pub ai_attempt_id: Uuid,
    pub assistant_message_id: Uuid,
    pub confirmed_at_ms: i64,
}
impl BotDeliveryReceipt {
    pub fn valid(&self) -> bool {
        v4(self.chat_id) && v4(self.ai_attempt_id) && v4(self.assistant_message_id)
    }
}

/// An owned binding, not a filesystem sandbox. Its Debug never exposes a path.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "String", into = "String")]
pub struct BotWorkingFolder(String);
impl BotWorkingFolder {
    pub fn new(value: String) -> Result<Self, &'static str> {
        let windows = |path: &str| {
            let bytes = path.as_bytes();
            bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && (bytes[2] == b'/' || bytes[2] == b'\\')
        };
        let unc = |path: &str| {
            let mut parts = path.split(['\\', '/']);
            let valid = |part: &str| !part.is_empty() && !matches!(part, "." | ".." | "?");
            parts.next().is_some_and(valid) && parts.next().is_some_and(valid)
        };
        let absolute = value.starts_with('/')
            || windows(&value)
            || value
                .strip_prefix("\\\\?\\")
                .is_some_and(|path| windows(path) || path.strip_prefix("UNC\\").is_some_and(unc))
            || value.strip_prefix("\\\\").is_some_and(unc);
        if value.is_empty() || value.len() > 32768 || value.contains('\0') || !absolute {
            return Err("invalid absolute Bot folder");
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for BotWorkingFolder {
    type Error = &'static str;
    fn try_from(v: String) -> Result<Self, Self::Error> {
        Self::new(v)
    }
}
impl From<BotWorkingFolder> for String {
    fn from(v: BotWorkingFolder) -> Self {
        v.0
    }
}
impl std::fmt::Debug for BotWorkingFolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotWorkingFolder([REDACTED])")
    }
}

#[cfg(test)]
mod tests;
