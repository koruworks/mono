// SPDX-License-Identifier: MPL-2.0

//! Engine-owned Bot configuration. No execution, HTTP or database owner lives here.

use crate::{Chat, ChatId, DocumentId, MessageId, ModelSelection, ProfileId, WorkspaceId};
use koru_delegate_core::Objective;
pub use koru_delegate_core::{BotId, BotRequestId, DeviceId, RunId};
use koru_delegate_schedule::{RecurrenceRule, ScheduleName};
pub use koru_store_core::UserScopeId;
use serde::{Deserialize, Serialize};
pub mod admission;
pub use admission::*;

pub mod lifecycle;
pub mod policy;
pub use lifecycle::*;
pub use policy::*;
pub mod avatar;
pub use avatar::*;
pub mod creation;
pub use creation::*;
pub mod library;
pub use library::*;
pub mod routines;
pub use routines::*;
pub mod children;
pub use children::*;

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
    /// The face the person picked for the Bot: an index into the face table
    /// ([`BOT_AVATAR_COUNT`]). Absent means none was picked, and the face is
    /// the default one ([`Bot::avatar`]). Omitted when absent, so
    /// a Bot with no choice serializes, and every stored receipt, captured
    /// Routine source and request fingerprint that holds one is, exactly as
    /// before the field existed (ai.db 24).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(range(min = 0, max = 23)))]
    pub avatar: Option<u8>,
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
    /// Ephemeral owner-list projection; absent from stored configuration and receipts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<BotSummary>,
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
    /// A pause, resume or access change of the Bot that its execution owner
    /// has not acknowledged yet, with since when and the owner's last answer:
    /// while it is there, changes of the Bot answer `bot_operation_pending`,
    /// except while an Archive, Trash or Restore holds the whole Bot, when
    /// they answer `bot_lifecycle_pending` (the hold is checked first).
    /// Reads fill it in; omitted when nothing is pending, so a Bot with
    /// nothing pending serializes, and every stored receipt holding one is
    /// stored, exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_change: Option<BotPendingChange>,
    /// The private connections this Bot may use without asking (M5): part of
    /// its policy, changed only by `update_access` with `connections`
    /// present. Omitted when the Bot holds none, so a Bot without a grant
    /// serializes, and every stored receipt that holds one is, exactly as
    /// before the field existed (ai.db 25).
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    #[cfg_attr(feature = "schema", schemars(length(max = 16)))]
    pub connections: std::collections::BTreeSet<koru_delegate_core::ConnectionGrantRef>,
    /// "May ask other bots" (M6): whether this Bot's Runs may ask another
    /// Bot. A setting of the Bot, changed only by `update_access` with
    /// `may_ask` present. It is not part of the captured policy: no policy
    /// bytes, digest or barrier holds it. On by default, and omitted while it
    /// is on, so a Bot that may ask serializes, and every stored receipt that
    /// holds one is, exactly as before the field existed (ai.db 26).
    #[serde(
        default = "may_ask_default",
        skip_serializing_if = "is_may_ask_default"
    )]
    pub may_ask: bool,
}

/// The default of [`Bot::may_ask`]: on.
pub const fn may_ask_default() -> bool {
    true
}
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_may_ask_default(value: &bool) -> bool {
    *value
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BotOwner {
    pub user_scope: UserScopeId,
    pub device: DeviceId,
}

/// Fresh authority from the same root's Store and validated supervisor bootstrap.
pub trait BotAuthority: Send + Sync {
    fn owner(&self) -> Result<BotOwner, BotError>;
    fn workspace(&self, workspace: WorkspaceId) -> Result<ProfileId, BotError>;
    fn page(&self, page: &BotPage) -> Result<(), BotError>;
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
    ReadRoutineDetail {
        bot_id: BotId,
        schedule_id: koru_delegate_core::ScheduleId,
        before_occurrence_ms: Option<i64>,
        #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 16)))]
        history_limit: u8,
    },
    ReadRoutines {
        bot_id: BotId,
        after_schedule: Option<koru_delegate_core::ScheduleId>,
        #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 16)))]
        limit: u8,
    },
    /// Vouched owner creation only: always Paused, with canonical inherited
    /// runtime/scope. The Routine starts from the default access, not a copy
    /// of the Bot's; `update_routine_access` changes it while it is paused.
    CreateRoutine {
        request_id: BotRequestId,
        bot_id: BotId,
        expected_bot_revision: u64,
        name: ScheduleName,
        objective: Objective,
        documents: Vec<DocumentId>,
        recurrence: RecurrenceRule,
    },
    ReadRoutineCreation {
        bot_id: BotId,
        request_id: BotRequestId,
    },
    RetryRoutineCreation {
        bot_id: BotId,
        original_request_id: BotRequestId,
        observed_token: BotRoutineCreationObserved,
    },
    /// Turn one Routine on, or pause it. One revision-checked commit by the
    /// Schedule's owner; it runs nothing now and catches up nothing. Enable
    /// is refused while the Bot is paused, archived or being moved; Pause
    /// does not stop a Run that is already claimed. Answered
    /// `routine_control`. A retry with the same revision is safe.
    SetRoutineLifecycle {
        bot_id: BotId,
        schedule_id: koru_delegate_core::ScheduleId,
        #[cfg_attr(feature = "schema", schemars(range(min = 1)))]
        expected_definition_revision: u32,
        action: BotRoutineLifecycleAction,
    },
    /// Replace one Routine's name and timing in one revision-checked commit,
    /// while it is on or paused. Its task, context and access are not
    /// changed. Answered `routine_control`.
    EditRoutine {
        bot_id: BotId,
        schedule_id: koru_delegate_core::ScheduleId,
        #[cfg_attr(feature = "schema", schemars(range(min = 1)))]
        expected_definition_revision: u32,
        expected_name: ScheduleName,
        name: ScheduleName,
        recurrence: RecurrenceRule,
    },
    /// Replace one paused Routine's own access. Refused while the Routine is
    /// on and while a Run of it has not finished. The access it was set up
    /// with stays recorded. Answered `routine_access`; the same request
    /// returns its retained answer.
    UpdateRoutineAccess {
        request_id: BotRequestId,
        bot_id: BotId,
        schedule_id: koru_delegate_core::ScheduleId,
        #[cfg_attr(feature = "schema", schemars(range(min = 1)))]
        expected_grant_revision: u64,
        access: BotAccess,
        #[cfg_attr(feature = "schema", schemars(length(max = 1024)))]
        selected_pages: Vec<BotPage>,
    },
    /// Read Role and ordered Knowledge references under current owner authority.
    ReadLibrary {
        bot_id: BotId,
    },
    /// Replace Knowledge context for new tasks without changing edit grants.
    /// Exact retries return the immutable receipt even after a page disappears.
    SetKnowledge {
        request_id: BotRequestId,
        bot_id: BotId,
        expected_revision: u64,
        pages: Vec<BotPage>,
    },
    /// Bind an ordinary Memory page without changing Role, Knowledge or grants.
    SetMemory {
        request_id: BotRequestId,
        bot_id: BotId,
        expected_revision: u64,
        memory: Option<BotPage>,
    },
    CreateChat {
        request_id: BotRequestId,
        bot_id: BotId,
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
    /// Pause every Bot of this person on this computer (M8): new work is
    /// refused from the commit of the change, running work continues, and
    /// no Bot's revision moves. `expected_team_revision` is the revision
    /// `read_team_pause` last answered (0 for a team never paused); another
    /// value is `team_revision_conflict`. Pausing a team that is already
    /// paused answers the current state and changes nothing. Answered
    /// `team_pause`.
    PauseTeam {
        request_id: BotRequestId,
        expected_team_revision: u64,
    },
    /// End the team pause (M8). Each Bot returns to its own pause state.
    /// The same rules as `pause_team`. Answered `team_pause`.
    ResumeTeam {
        request_id: BotRequestId,
        expected_team_revision: u64,
    },
    /// The team pause of this person on this computer (M8). Answered
    /// `team_pause`.
    ReadTeamPause {},
    ReadOperation {
        request_id: BotRequestId,
        bot_id: BotId,
    },
    ReconcileOperation {
        request_id: BotRequestId,
        bot_id: BotId,
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
        /// The Bot's whole set of connection grants (M5). Absent changes no
        /// grant, so a caller that does not know the field keeps them all;
        /// present replaces the set, and an empty list removes every grant.
        /// A grant is accepted only for a connection of the same person on
        /// this computer that is connected: otherwise
        /// `connection_unavailable`. The request fingerprint covers it only
        /// when it is present.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "schema", schemars(length(max = 16)))]
        connections: Option<std::collections::BTreeSet<koru_delegate_core::ConnectionGrantRef>>,
        /// "May ask other bots" (M6). Absent changes nothing, so a caller
        /// that does not know the field keeps the Bot's setting; present sets
        /// it. The request fingerprint covers it only when it is present.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        may_ask: Option<bool>,
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
        #[serde(default)]
        cursor: Option<BotActivityCursor>,
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
}

impl BotCommand {
    /// An owner control of one Routine (P4.3): its Bot and its Schedule.
    pub fn routine_control(&self) -> Option<(BotId, koru_delegate_core::ScheduleId)> {
        match self {
            Self::SetRoutineLifecycle {
                bot_id,
                schedule_id,
                ..
            }
            | Self::EditRoutine {
                bot_id,
                schedule_id,
                ..
            }
            | Self::UpdateRoutineAccess {
                bot_id,
                schedule_id,
                ..
            } => Some((*bot_id, *schedule_id)),
            _ => None,
        }
    }

    /// The lifecycle operation a command asks for, if it is one.
    pub fn lifecycle_kind(&self) -> Option<BotLifecycleKind> {
        Some(match self {
            Self::Archive { .. } => BotLifecycleKind::Archive,
            Self::Trash { .. } => BotLifecycleKind::Trash,
            Self::Restore { .. } => BotLifecycleKind::Restore,
            Self::TrashChat { .. } => BotLifecycleKind::TrashChat,
            Self::RestoreChat { .. } => BotLifecycleKind::RestoreChat,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum BotResponse {
    RoutineDetail {
        #[serde(flatten)]
        reply: BotRoutineDetailEnvelope,
    },
    Routines {
        page: BotRoutineListPage,
    },
    RoutineCreated {
        receipt: BotRoutineCreateResult,
    },
    RoutineCreation {
        creation: BotRoutineCreationStatus,
    },
    RoutineControl {
        control: BotRoutineControl,
    },
    RoutineAccess {
        access: BotRoutineAccess,
    },
    Library {
        library: BotLibraryView,
    },
    KnowledgeSaved {
        receipt: BotKnowledgeReceipt,
    },
    Lifecycle {
        operation: BotLifecycleProgress,
    },
    Chat {
        reference: BotChatReference,
    },
    Operation {
        operation: BotOperation,
    },
    Bot {
        bot: Bot,
    },
    List {
        bots: Vec<Bot>,
    },
    Chats {
        page: BotChatPage,
    },
    TrashedChats {
        page: BotTrashedChatPage,
    },
    Activity {
        activity: BotActivityPage,
    },
    Creation {
        creation: BotCreation,
    },
    Creations {
        page: BotCreationPage,
    },
    /// M8: the team pause after `pause_team`, `resume_team` or
    /// `read_team_pause`.
    TeamPause {
        team: TeamPauseView,
    },
}

/// The team pause of one person on one computer (M8). A team that was never
/// paused reads as revision 0, not paused, `acknowledged`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TeamPauseView {
    /// The computer whose Bots this pause holds.
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub owner_device: DeviceId,
    pub paused: bool,
    /// Moves by one with every change. The `expected_team_revision` of the
    /// next `pause_team` or `resume_team`.
    pub revision: u64,
    pub stage: TeamPauseStage,
    /// When the change that is still `pending` was made; absent once what
    /// runs the Bots has confirmed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_since_ms: Option<i64>,
}

impl TeamPauseView {
    /// A team that was never paused.
    pub fn never_paused(owner_device: DeviceId) -> Self {
        Self {
            owner_device,
            paused: false,
            revision: 0,
            stage: TeamPauseStage::Acknowledged,
            pending_since_ms: None,
        }
    }
}

/// Whether what runs the Bots has confirmed the team's current state. While
/// `pending`, new work is already refused here; a task that was already
/// queued there may still start until the change is confirmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum TeamPauseStage {
    Pending,
    Acknowledged,
}

impl TeamPauseStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Acknowledged => "acknowledged",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "acknowledged" => Some(Self::Acknowledged),
            _ => None,
        }
    }
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
    /// precedence over `running` when it holds.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<BotActivityCursor>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotActivityCursor {
    pub bot: uuid::Uuid,
    pub thread: uuid::Uuid,
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotSummary {
    pub latest: Option<BotMainPreview>,
    pub pending_results: u64,
    pub open_requests: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotMainPreview {
    pub message: MessageId,
    pub at_ms: i64,
    pub text: String,
}

impl std::fmt::Debug for BotSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotSummary")
            .field("pending_results", &self.pending_results)
            .field("open_requests", &self.open_requests)
            .finish_non_exhaustive()
    }
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
/// order; `next_cursor` is absent on the last page. Every page but the last
/// holds exactly `limit` conversations, and `next_cursor` names the last of
/// them.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotError {
    Invalid,
    ContextTooLarge,
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
    /// A new attempt cannot convert a Routine Run into a Manual continuation.
    RoutineRetryUnsupported,
    /// A connection grant names a connection that is not this person's on
    /// this computer, or is not connected (M5).
    ConnectionUnavailable,
    /// The team is paused (M8): new work is refused. Reading, Stop, a
    /// message to a Bot that is working, and every edit stay allowed.
    TeamPaused,
    /// `expected_team_revision` is not the team's current revision (M8):
    /// read the team pause again.
    TeamRevisionConflict,
}
impl BotError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Invalid => "invalid_request",
            Self::ContextTooLarge => "context_too_large",
            Self::NotFound => "not_found",
            Self::Forbidden => "wrong_owner",
            Self::Unavailable => "workspace_unavailable",
            Self::Conflict => "request_conflict",
            Self::RevisionConflict => "revision_conflict",
            Self::FolderLocked => "folder_locked",
            Self::Storage => "storage_error",
            Self::Settling => "bot_operation_pending",
            Self::RoutineRetryUnsupported => "routine_retry_unsupported",
            Self::ConnectionUnavailable => "connection_unavailable",
            Self::TeamPaused => "team_paused",
            Self::TeamRevisionConflict => "team_revision_conflict",
        }
    }
}

pub fn validate_config(config: &BotConfig, workspace: WorkspaceId) -> Result<(), BotError> {
    validate_bot_model(&config.model)?;
    if config.name.trim().is_empty()
        || config.name.len() > 256
        || config.role.workspace != workspace
        || !std::path::Path::new(&config.working_folder).is_absolute()
        || config.working_folder.len() > 32768
        || config.working_folder.contains('\0')
        || config
            .mode
            .as_ref()
            .is_some_and(|mode| mode.is_empty() || mode.len() > 128)
    {
        return Err(BotError::Invalid);
    }
    validate_avatar(config.avatar)
}

/// The model portion of canonical Bot validation, also used by bounded
/// observational readers which deliberately do not decode execution settings.
pub fn validate_bot_model(model: &ModelSelection) -> Result<(), BotError> {
    if model.requested_model.trim().is_empty()
        || model.requested_model.len() > 512
        || !model.provider.as_str().starts_with("agent-")
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
