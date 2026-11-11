// SPDX-License-Identifier: MPL-2.0

//! Engine-owned Bot configuration. No execution, HTTP or database owner lives here.

use crate::{Chat, ChatId, DocumentId, ModelSelection, ProfileId, WorkspaceId};
pub use koru_delegate_core::{BotId, BotRequestId, DeviceId, RunId};
pub use koru_store_core::UserScopeId;
use serde::{Deserialize, Serialize};
pub mod admission;
pub use admission::*;

pub mod lifecycle;
pub mod policy;
pub mod replay;
pub use lifecycle::*;
pub use policy::*;
pub mod creation;
pub use creation::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotAccess {
    Default,
    Workspace,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotPage {
    pub workspace: WorkspaceId,
    pub document: DocumentId,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotConfig {
    pub name: String,
    pub role: BotPage,
    pub working_folder: String,
    #[serde(deserialize_with = "strict_model")]
    #[cfg_attr(feature = "schema", schemars(with = "StrictModel"))]
    pub model: ModelSelection,
    pub mode: Option<String>,
    pub fast: Option<bool>,
}

impl std::fmt::Debug for BotConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotConfig").finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Bot {
    pub id: BotId,
    pub home_workspace: WorkspaceId,
    pub user_scope: UserScopeId,
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub owner_device: DeviceId,
    pub config: BotConfig,
    pub access: BotAccess,
    pub selected_pages: Vec<BotPage>,
    pub revision: u64,
    pub policy_revision: u64,
    pub main_chat: ChatId,
    pub paused: bool,
    pub folder_lock_request: Option<BotRequestId>,
    pub folder_frozen_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// Where the Bot is in its lifecycle. Reads fill it in; it is omitted
    /// while the Bot is active, so an active Bot's serialized form, and every
    /// stored receipt that holds one, is unchanged (P1.7).
    #[serde(default, skip_serializing_if = "BotLifecycle::is_active")]
    pub lifecycle: BotLifecycle,
    /// The Bot's unfinished lifecycle operation, moving or parked, so a
    /// client that lost its id can find it, resume it or reverse it. Reads
    /// fill it in; omitted when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_operation: Option<BotOpenLifecycle>,
    /// For an archived or trashed Bot, the completed operation that holds it
    /// there, to name in the Restore. Reads fill it in (`list_lifecycle`,
    /// and `read` of an archived Bot); omitted for an active Bot, so an
    /// active Bot's serialized form and every stored receipt are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_by: Option<BotLifecycleHolder>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BotOwner {
    pub user_scope: UserScopeId,
    pub device: DeviceId,
}

/// Immutable allowlist supplied only after fresh owner checks. Not a public DTO.
#[derive(Clone, Debug, Default)]
pub struct ChatReadAccess {
    allowed_bot_chats: Vec<ChatId>,
}
impl ChatReadAccess {
    pub fn ordinary() -> Self {
        Self::default()
    }
    pub fn authorized(chats: Vec<ChatId>) -> Self {
        Self {
            allowed_bot_chats: chats,
        }
    }
    pub fn is_ordinary(&self) -> bool {
        self.allowed_bot_chats.is_empty()
    }
    pub fn json(&self) -> String {
        serde_json::to_string(&self.allowed_bot_chats).expect("UUID serialization")
    }
}

/// Fresh authority from the same root's Store and validated supervisor bootstrap.
pub trait BotAuthority: Send + Sync {
    fn owner(&self) -> Result<BotOwner, BotError>;
    fn workspace(&self, workspace: WorkspaceId) -> Result<ProfileId, BotError>;
    fn page(&self, page: &BotPage) -> Result<(), BotError>;
    /// Trusted preparation only: capture immutable history under fresh current
    /// authority. Public callers cannot supply the returned version identities.
    fn capture(
        &self,
        _owner: BotOwner,
        _workspace: WorkspaceId,
        _profile: ProfileId,
        _pages: &[BotPage],
    ) -> Result<Vec<koru_delegate_core::RunContextItem>, BotError> {
        Err(BotError::Unavailable)
    }
    /// One bounded command's authority inspection; adapters batch this under one Store lock.
    fn inspect(
        &self,
        workspaces: &[WorkspaceId],
        pages: &[BotPage],
    ) -> Result<BotAuthoritySnapshot, BotError> {
        Ok(BotAuthoritySnapshot {
            owner: self.owner()?,
            workspaces: workspaces
                .iter()
                .map(|id| (*id, self.workspace(*id)))
                .collect(),
            pages: pages
                .iter()
                .map(|page| (page.clone(), self.page(page)))
                .collect(),
        })
    }
    /// Trusted creation only (P1.8 B2): plant a Bot's Role page through the
    /// Store's strict creation origin with exactly the recorded candidate,
    /// key, hash and seed, then read back its stored origin and whether it
    /// is live. `Conflict` when the candidate exists with another origin;
    /// `Unavailable` whenever the page may or may not have been written.
    fn create_role_page(&self, _seed: &BotRoleSeed) -> Result<BotRoleProof, BotError> {
        Err(BotError::Unavailable)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotCommand {
    CreateChat {
        request_id: BotRequestId,
        bot_id: BotId,
    },
    Start {
        request_id: BotRequestId,
        bot_id: BotId,
        chat_id: ChatId,
        expected_revision: u64,
        destination: WorkspaceId,
        task: BotTask,
    },
    Continue {
        request_id: BotRequestId,
        bot_id: BotId,
        chat_id: ChatId,
        expected_revision: u64,
        destination: WorkspaceId,
        task: BotTask,
        previous_run: koru_delegate_core::RunId,
    },
    Pause {
        request_id: BotRequestId,
        bot_id: BotId,
        expected_revision: u64,
    },
    Resume {
        request_id: BotRequestId,
        bot_id: BotId,
        expected_revision: u64,
    },
    ReadOperation {
        request_id: BotRequestId,
        bot_id: BotId,
    },
    ReconcileOperation {
        request_id: BotRequestId,
        bot_id: BotId,
    },
    Create {
        request_id: BotRequestId,
        bot_id: BotId,
        home_workspace: WorkspaceId,
        config: BotConfig,
        access: BotAccess,
        selected_pages: Vec<BotPage>,
    },
    Read {
        bot_id: BotId,
    },
    List {
        workspace: WorkspaceId,
    },
    Update {
        request_id: BotRequestId,
        bot_id: BotId,
        expected_revision: u64,
        config: BotConfig,
    },
    UpdateAccess {
        request_id: BotRequestId,
        bot_id: BotId,
        expected_revision: u64,
        expected_policy_revision: u64,
        access: BotAccess,
        selected_pages: Vec<BotPage>,
    },
    /// Settle the Bot's work and keep it, with its history, out of the roster.
    Archive {
        request_id: BotRequestId,
        bot_id: BotId,
        expected_revision: u64,
    },
    /// Settle the Bot's work and move it, as one recoverable unit, to Trash.
    Trash {
        request_id: BotRequestId,
        bot_id: BotId,
        expected_revision: u64,
    },
    /// Reverse a named Archive or Trash; the Bot returns paused.
    Restore {
        request_id: BotRequestId,
        bot_id: BotId,
        expected_revision: u64,
        source_request_id: BotRequestId,
    },
    /// Settle one conversation's work and move only it to Trash.
    TrashChat {
        request_id: BotRequestId,
        bot_id: BotId,
        chat_id: ChatId,
        expected_revision: u64,
    },
    RestoreChat {
        request_id: BotRequestId,
        bot_id: BotId,
        chat_id: ChatId,
        expected_revision: u64,
        source_request_id: BotRequestId,
    },
    /// Permanently delete a trashed Bot, from its named Trash operation.
    PurgeBot {
        request_id: BotRequestId,
        bot_id: BotId,
        expected_revision: u64,
        source_request_id: BotRequestId,
    },
    PurgeChat {
        request_id: BotRequestId,
        bot_id: BotId,
        chat_id: ChatId,
        expected_revision: u64,
        source_request_id: BotRequestId,
    },
    /// Progress of one lifecycle operation, with per-member outcomes.
    ReadLifecycleOperation {
        request_id: BotRequestId,
        bot_id: BotId,
    },
    /// The Bots of a workspace in one lifecycle (the Archived and Trash filters).
    ListLifecycle {
        workspace: WorkspaceId,
        lifecycle: BotLifecycle,
    },
    /// One bounded page of an active Bot's conversations outside Trash: the
    /// main conversation first, then the rest by recency. `cursor` is the
    /// previous page's `next_cursor`; `limit` is 1 to 100.
    ListBotChats {
        bot_id: BotId,
        cursor: Option<BotChatCursor>,
        limit: u16,
    },
    /// One bounded page of the owner's Bot conversations in Trash whose Bot
    /// lives in `workspace`, whatever the Bot's lifecycle: each names the
    /// operation that put it there and which Restore brings it back.
    /// `cursor` is the previous page's `next_cursor`; `limit` is 1 to 100.
    ListTrashedBotChats {
        workspace: WorkspaceId,
        cursor: Option<BotTrashedChatCursor>,
        limit: u16,
    },
    /// The conversations of a workspace's active Bots that are running or
    /// need the person, at most 200, read from durable AI state.
    ListActivity {
        workspace: WorkspaceId,
    },
    /// Create a Bot together with its Role page and working folder (P1.8).
    /// Answered with the creation's progress; the same request and input
    /// returns its current progress or the completed Bot, and changed input
    /// under the same request is `request_conflict`.
    CreateBotWithRole {
        request_id: BotRequestId,
        bot_id: BotId,
        home_workspace: WorkspaceId,
        role: BotRoleInput,
        folder: BotFolderChoice,
        config: BotCreationConfig,
        access: BotAccess,
        selected_pages: Vec<BotPage>,
    },
    /// One creation's progress, under current authority.
    ReadBotCreation {
        request_id: BotRequestId,
    },
    /// One page of a workspace's unfinished creations, oldest first.
    /// `limit` is 1 to 100.
    ListBotCreations {
        workspace: WorkspaceId,
        cursor: Option<BotCreationCursor>,
        limit: u16,
    },
    /// Resume an incomplete creation from its recorded intent, under fresh
    /// authority, without the original request. One that is moving,
    /// finished, or parked for a terminal reason only reports itself.
    RetryBotCreation {
        request_id: BotRequestId,
    },
    /// End an unfinished creation: no Bot is made, the retained Role text is
    /// dropped, the BotId stays reserved, and nothing is deleted; the answer
    /// names what it leaves behind. A completed creation is
    /// `request_conflict`.
    AbandonBotCreation {
        request_id: BotRequestId,
    },
    /// One Run of a Bot conversation, read through the execution owner: the
    /// Run that wrote reply `message_id` of `chat_id` (or the reply a branch
    /// copied there), with a window of its history from entry `from`, `len`
    /// entries (1 to 500). Answered `run` with the Run view
    /// `GET /v1/runs/{run}` answers, without `execution.session`: no
    /// stream, control or takeover handle.
    ReadRun {
        bot_id: BotId,
        chat_id: ChatId,
        message_id: crate::MessageId,
        run_id: koru_delegate_core::RunId,
        from: u64,
        #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 500)))]
        len: u32,
    },
}

/// The most history entries one [`BotCommand::ReadRun`] window carries: the
/// Run view's own bound.
pub const BOT_RUN_WINDOW_LIMIT: u32 = 500;

impl BotCommand {
    /// The lifecycle operation a command asks for, if it is one.
    pub fn lifecycle_kind(&self) -> Option<BotLifecycleKind> {
        Some(match self {
            Self::Archive { .. } => BotLifecycleKind::Archive,
            Self::Trash { .. } => BotLifecycleKind::Trash,
            Self::Restore { .. } => BotLifecycleKind::Restore,
            Self::TrashChat { .. } => BotLifecycleKind::TrashChat,
            Self::RestoreChat { .. } => BotLifecycleKind::RestoreChat,
            Self::PurgeBot { .. } => BotLifecycleKind::PurgeBot,
            Self::PurgeChat { .. } => BotLifecycleKind::PurgeChat,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum BotResponse {
    Lifecycle { operation: BotLifecycleProgress },
    Chat { reference: BotChatReference },
    Operation { operation: BotOperation },
    Bot { bot: Bot },
    List { bots: Vec<Bot> },
    Chats { page: BotChatPage },
    TrashedChats { page: BotTrashedChatPage },
    Activity { activity: BotActivityPage },
    Creation { creation: BotCreation },
    Creations { page: BotCreationPage },
    Run { view: koru_delegate_core::RunView },
}

/// The most conversations one [`BotCommand::ListActivity`] reply names.
pub const BOT_ACTIVITY_LIMIT: usize = 200;

/// What a Bot conversation is waiting on, from the AI owner's durable state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotActivityState {
    /// A reply is in progress: queued for the Bot or executing.
    Running,
    /// An approval or a question card is open for the person. Takes
    /// precedence over `running` when both hold.
    NeedsYou,
}

/// One conversation with work in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotActivity {
    pub bot_id: BotId,
    pub chat_id: ChatId,
    pub state: BotActivityState,
}

/// A workspace's Bot activity, ordered by Bot and then conversation.
/// `truncated` says more conversations had activity than were returned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotActivityPage {
    pub entries: Vec<BotActivity>,
    pub truncated: bool,
}

/// The most conversations one [`BotCommand::ListBotChats`] page returns.
pub const BOT_CHAT_PAGE_LIMIT: u16 = 100;

/// One page of a Bot's conversations; `next_cursor` is absent on the last page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotChatPage {
    pub chats: Vec<BotConversation>,
    pub next_cursor: Option<BotChatCursor>,
}

/// Which Bot a conversation belongs to, as its authorized owner reads it.
/// Display data, never authority: commands still name the Bot and Chat and
/// are checked against the owner's current proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotChatLabel {
    pub bot_id: BotId,
    /// The Bot's current name.
    pub bot_name: String,
    pub purpose: BotChatPurpose,
}

/// One conversation of a Bot, with the label its owner sees.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotConversation {
    pub chat: Chat,
    pub label: BotChatLabel,
}

/// The position after the last conversation of a page, in the page's own
/// order (main first, then newest first, then by ID). It names only what
/// that page already returned and grants nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotChatCursor {
    pub main: bool,
    pub updated_at_ms: i64,
    pub chat_id: ChatId,
}

/// One page of a workspace's Bot conversations in Trash, in conversation ID
/// order; `next_cursor` is absent on the last page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotTrashedChatPage {
    pub chats: Vec<BotTrashedChat>,
    pub next_cursor: Option<BotTrashedChatCursor>,
}

/// A Bot conversation in Trash, with what put it there and how it comes back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotTrashedChat {
    pub conversation: BotConversation,
    /// The operation whose Trash row hides it: its own `trash_chat`, or the
    /// whole Bot's `trash`. Name it as `source_request_id` in the Restore.
    pub trashed_by: BotTrashedBy,
    pub trashed_at_ms: i64,
    pub restore: BotChatRestore,
    /// When `restore` is `bot`: the completed operation that holds the Bot
    /// archived or in Trash (as the Bot's own `held_by`), to name as
    /// `source_request_id` in its `restore` first. Omitted for an active
    /// Bot's conversation, and for one whose Bot's holder cannot be found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_held_by: Option<BotLifecycleHolder>,
}

/// The lifecycle operation that put a conversation into Trash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotTrashedBy {
    pub request_id: BotRequestId,
    pub kind: BotLifecycleKind,
}

/// Which Restore brings a trashed conversation back now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotChatRestore {
    /// `restore_chat` naming `trashed_by`: it was trashed on its own and its
    /// Bot is active.
    Conversation,
    /// The Bot first: its Bot is trashed or archived now, and `restore_chat`
    /// refuses until it is not. In order:
    /// 1. `restore` naming the operation that holds the Bot: `bot_held_by`
    ///    (the Bot's `held_by` in `list_lifecycle`). When `trashed_by` is
    ///    the Bot's own `trash` it is that same operation, and this alone
    ///    brings the conversation back.
    /// 2. When `trashed_by` is a `trash_chat`, the conversation was trashed
    ///    on its own before; the Bot's Restore leaves it in Trash, and
    ///    `restore_chat` naming `trashed_by` then brings it back. Naming a
    ///    `trash_chat` in `restore` is `invalid_request`.
    Bot,
}

/// The position after the last conversation of a Trash page. It names only
/// what that page already returned and grants nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotTrashedChatCursor {
    pub chat_id: ChatId,
}

/// A durable conversation identity; history remains behind normal Chat reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotChatReference {
    pub bot_id: BotId,
    pub chat_id: ChatId,
}

/// Closed, bounded receipt vocabulary. Role creation is coordinated in the UI plan;
/// its Store intent must already be durable before the first create call.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotReceiptOutcome {
    ChatCreated {
        version: u32,
        reference: BotChatReference,
    },
    AccessPolicy {
        version: u32,
        bot: Bot,
        barrier: koru_delegate_core::BotPolicyBarrier,
    },
    Configuration {
        version: u32,
        bot: Bot,
    },
    RoleCreation {
        version: u32,
        bot: BotId,
        store_request: uuid::Uuid,
        destination: WorkspaceId,
        request_fingerprint: String,
        verified_page: Option<BotPage>,
        verified_version: Option<Vec<u8>>,
    },
    /// A Bot created together with its Role page and folder (P1.8 §1's
    /// stored receipt format addition). Version 2 only.
    CreationV2 {
        version: u32,
        creation: BotCreationRecord,
    },
}

impl BotReceiptOutcome {
    pub fn validate(&self) -> Result<(), BotError> {
        match self {
            Self::ChatCreated { version: 1, .. } => Ok(()),
            Self::AccessPolicy {
                version: 1,
                bot,
                barrier,
            } => validate_access_policy(bot, barrier),
            Self::Configuration { version: 1, .. } => Ok(()),
            Self::RoleCreation {
                version: 1,
                destination,
                verified_page,
                verified_version,
                request_fingerprint,
                ..
            } if request_fingerprint.len() == 64
                && verified_page
                    .as_ref()
                    .is_none_or(|page| page.workspace == *destination)
                && verified_page.is_some() == verified_version.is_some()
                && verified_version
                    .as_ref()
                    .is_none_or(|version| !version.is_empty() && version.len() <= 65536) =>
            {
                Ok(())
            }
            Self::CreationV2 {
                version: 2,
                creation,
            } => creation.validate(),
            _ => Err(BotError::Invalid),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotError {
    Invalid,
    NotFound,
    Forbidden,
    Unavailable,
    Conflict,
    RevisionConflict,
    FolderLocked,
    Storage,
    /// Another change to this Bot (a pause, a resume or an access change) is
    /// still `pending`, waiting for its owner to acknowledge it; retry once
    /// that operation reads accepted. Not a reused request id.
    Settling,
}
impl BotError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Invalid => "invalid_request",
            Self::NotFound => "not_found",
            Self::Forbidden => "wrong_owner",
            Self::Unavailable => "workspace_unavailable",
            Self::Conflict => "request_conflict",
            Self::RevisionConflict => "revision_conflict",
            Self::FolderLocked => "folder_locked",
            Self::Storage => "storage_error",
            Self::Settling => "bot_operation_pending",
        }
    }
}

pub trait BotRepository {
    fn bot(&self, id: BotId) -> Result<Option<Bot>, BotError>;
    fn bots(&self, scope: UserScopeId, workspace: WorkspaceId) -> Result<Vec<Bot>, BotError>;
    fn save_bot(
        &mut self,
        bot: &Bot,
        profile: ProfileId,
        request: BotRequestId,
        fingerprint: &str,
        expected: Option<u64>,
    ) -> Result<Bot, BotError>;
    fn bot_receipt(
        &self,
        scope: UserScopeId,
        request: BotRequestId,
        fingerprint: &str,
    ) -> Result<Option<Bot>, BotError>;
}

pub fn validate_config(config: &BotConfig, workspace: WorkspaceId) -> Result<(), BotError> {
    if config.name.trim().is_empty()
        || config.name.len() > 256
        || config.role.workspace != workspace
        || !std::path::Path::new(&config.working_folder).is_absolute()
        || config.working_folder.len() > 32768
        || config.working_folder.contains('\0')
        || config.model.requested_model.trim().is_empty()
        || config.model.requested_model.len() > 512
        || !config.model.provider.as_str().starts_with("agent-")
        || config
            .mode
            .as_ref()
            .is_some_and(|mode| mode.is_empty() || mode.len() > 128)
    {
        return Err(BotError::Invalid);
    }
    Ok(())
}

pub fn normalize_pages(pages: &mut Vec<BotPage>, workspace: WorkspaceId) -> Result<(), BotError> {
    if pages.len() > 1024 || pages.iter().any(|page| page.workspace != workspace) {
        return Err(BotError::Invalid);
    }
    pages.sort();
    pages.dedup();
    Ok(())
}

#[derive(Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct StrictModel {
    provider: crate::AiProviderId,
    requested_model: String,
    default_reasoning: crate::ReasoningChoice,
}
fn strict_model<'de, D: serde::Deserializer<'de>>(d: D) -> Result<ModelSelection, D::Error> {
    let m = StrictModel::deserialize(d)?;
    Ok(ModelSelection {
        provider: m.provider,
        requested_model: m.requested_model,
        default_reasoning: m.default_reasoning,
    })
}
#[cfg(test)]
mod tests;

/// Internal per-command evidence. Never a wire payload or authority cache.
#[derive(Clone)]
pub struct BotAuthoritySnapshot {
    pub owner: BotOwner,
    pub workspaces: std::collections::BTreeMap<WorkspaceId, Result<ProfileId, BotError>>,
    pub pages: std::collections::BTreeMap<BotPage, Result<(), BotError>>,
}
impl BotAuthority for BotAuthoritySnapshot {
    fn owner(&self) -> Result<BotOwner, BotError> {
        Ok(self.owner)
    }
    fn workspace(&self, id: WorkspaceId) -> Result<ProfileId, BotError> {
        self.workspaces
            .get(&id)
            .cloned()
            .unwrap_or(Err(BotError::Forbidden))
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.pages
            .get(page)
            .cloned()
            .unwrap_or(Err(BotError::Forbidden))
    }
}
