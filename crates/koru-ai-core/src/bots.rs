// SPDX-License-Identifier: MPL-2.0

//! Engine-owned Bot configuration. No execution, HTTP or database owner lives here.

use crate::{Chat, ChatId, DocumentId, ModelSelection, ProfileId, WorkspaceId};
use koru_delegate_core::Objective;
pub use koru_delegate_core::{BotId, BotRequestId, DeviceId, RunId};
use koru_delegate_schedule::{RecurrenceRule, ScheduleName};
pub use koru_store_core::UserScopeId;
use serde::{Deserialize, Serialize};
pub mod admission;
pub use admission::*;

pub mod lifecycle;
pub mod policy;
pub mod replay;
pub use lifecycle::*;
pub use policy::*;
pub mod avatar;
pub use avatar::*;
pub mod creation;
pub use creation::*;
pub mod library;
pub use library::*;
pub mod memory;
pub use memory::*;
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

/// Whether a Bot whose home is a workspace may be copied to the person's
/// linked desktops (M8, D22).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HomeCopy {
    /// The workspace, and its Profile, may leave this computer.
    Copied,
    /// The workspace, or its Profile, is kept on this computer only. A Bot
    /// that lives there is never copied: no record of it, of its
    /// conversations or of its messages is built.
    LocalOnly,
}

/// Fresh authority from the same root's Store and validated supervisor bootstrap.
pub trait BotAuthority: Send + Sync {
    fn owner(&self) -> Result<BotOwner, BotError>;
    fn workspace(&self, workspace: WorkspaceId) -> Result<ProfileId, BotError>;
    fn page(&self, page: &BotPage) -> Result<(), BotError>;
    /// Whether Bots whose home is `workspace` may be copied to linked
    /// desktops: the **stored** choice of where the workspace is kept,
    /// capped by its Profile's. A choice somebody made and a default nobody
    /// changed count alike.
    ///
    /// It never asks what the workspace is admitted to right now: a
    /// workspace that is only fenced for a moment is still copied.
    ///
    /// An authority that cannot answer says so, and its caller withholds the
    /// Bot: the default answers `Unavailable`, so an authority that does not
    /// implement this copies nothing.
    fn home_copy(&self, _workspace: WorkspaceId) -> Result<HomeCopy, BotError> {
        Err(BotError::Unavailable)
    }
    /// Exact captured history metadata, under current owner and reader-profile
    /// authority. Called off the AI writer; never resolves current body content.
    fn pinned_document_revision(
        &self,
        _owner: BotOwner,
        _workspace: WorkspaceId,
        _reader: ProfileId,
        _document: DocumentId,
        _version: koru_delegate_core::DocumentVersionId,
    ) -> Result<crate::StoreRevision, BotError> {
        Err(BotError::Unavailable)
    }
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

    /// Off-writer ordinary Memory creation/reconciliation under current Store
    /// authority. A cancellation does not prove an earlier write was undone.
    fn prepare_memory_page(
        &self,
        _owner: BotOwner,
        _reader: ProfileId,
        _identity: &BotMemoryIdentity,
        _cancellation: &crate::Cancellation,
    ) -> Result<PreparedMemoryPage, MemoryAuthorityError> {
        Err(BotError::Unavailable.into())
    }

    /// Private bounded current-state observation for an independently validated
    /// Bound creation. It grants no execution authority and creates no content.
    fn read_routine_overview(
        &self,
        _owner: BotOwner,
        _reader: ProfileId,
        _identity: &BotRoutineCreationIdentity,
        _cancellation: &crate::Cancellation,
    ) -> Result<RoutineOverviewObservation, BotError> {
        Err(BotError::Unavailable)
    }

    fn read_routine_detail(
        &self,
        _owner: BotOwner,
        _reader: ProfileId,
        _identity: &BotRoutineCreationIdentity,
        _observed_at_ms: i64,
        _cancellation: &crate::Cancellation,
    ) -> Result<RoutineDetailObservation, BotError> {
        Err(BotError::Unavailable)
    }

    /// Current complete Schedule definition and human/Profile proof, performed
    /// off-writer against the canonical physical candidate and retained origin.
    fn verify_routine_definition(
        &self,
        _owner: BotOwner,
        _reader: ProfileId,
        _identity: &BotRoutineCreationIdentity,
        _definition: &koru_delegate_schedule::ScheduleDefinition,
        _owner_handoff: Option<koru_delegate_core::HandoffId>,
        _cancellation: &crate::Cancellation,
    ) -> Result<(), BotError> {
        Err(BotError::Unavailable)
    }

    /// The same proof for an owner Test only: the live definition must be
    /// the offered one and exactly Paused(Manual). The caller has matched its
    /// own expectation of that Test first; a paused definition is never
    /// accepted because it merely arrived.
    fn verify_routine_test_definition(
        &self,
        _owner: BotOwner,
        _reader: ProfileId,
        _identity: &BotRoutineCreationIdentity,
        _definition: &koru_delegate_schedule::ScheduleDefinition,
        _owner_handoff: Option<koru_delegate_core::HandoffId>,
        _cancellation: &crate::Cancellation,
    ) -> Result<(), BotError> {
        Err(BotError::Unavailable)
    }

    /// The selected Routine's observation with one page of its Schedule's
    /// own firings: those below `before`, the newest `limit`. The default
    /// reads no firings.
    #[allow(clippy::too_many_arguments)]
    fn read_routine_detail_page(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        identity: &BotRoutineCreationIdentity,
        observed_at_ms: i64,
        _before: Option<i64>,
        _limit: u8,
        cancellation: &crate::Cancellation,
    ) -> Result<(RoutineDetailObservation, RoutineFiringsObservation), BotError> {
        self.read_routine_detail(owner, reader, identity, observed_at_ms, cancellation)
            .map(|observation| (observation, RoutineFiringsObservation::Unavailable))
    }

    /// Private off-writer creation/reconciliation after the delegate has
    /// acknowledged the exact deny-only route. Never launch authority.
    fn prepare_routine_document(
        &self,
        _owner: BotOwner,
        _reader: ProfileId,
        _identity: &BotRoutineCreationIdentity,
        _cancellation: &crate::Cancellation,
    ) -> Result<PreparedRoutineDocument, BotError> {
        Err(BotError::Unavailable)
    }

    fn capture_memory(
        &self,
        _owner: BotOwner,
        _reader: ProfileId,
        _binding: &BotMemoryBinding,
        _cancellation: &crate::Cancellation,
    ) -> Result<PreparedMemoryCapture, MemoryAuthorityError> {
        Err(BotError::Unavailable.into())
    }

    fn replay_memory_index(
        &self,
        _owner: BotOwner,
        _reader: ProfileId,
        _home: WorkspaceId,
        _capture: &BotLibraryCapture,
        _binding: Option<&BotMemoryBinding>,
        _cancellation: &crate::Cancellation,
    ) -> Result<VerifiedMemoryIndex, MemoryAuthorityError> {
        Err(BotError::Unavailable.into())
    }

    fn read_memory_page(
        &self,
        _owner: BotOwner,
        _reader: ProfileId,
        _binding: &BotMemoryBinding,
        _snapshot: MemorySnapshot,
        _position: MemoryReadPosition,
        _cancellation: &crate::Cancellation,
    ) -> Result<MemoryPageResult, MemoryAuthorityError> {
        Err(BotError::Unavailable.into())
    }

    /// Encoded evidence for a trusted off-writer Memory edit job only. The job
    /// must validate P3c history integrity before deriving a bounded recipe;
    /// these bytes must never enter the AI writer or gateway.
    fn memory_edit_history(
        &self,
        _owner: BotOwner,
        _reader: ProfileId,
        _binding: &BotMemoryBinding,
        _version: koru_delegate_core::DocumentVersionId,
        _cancellation: &crate::Cancellation,
    ) -> Result<koru_store_core::ports::BoundedDocumentVersion, MemoryAuthorityError> {
        Err(BotError::Unavailable.into())
    }

    /// Read-only exact creation-origin proof, never creation or latest-state
    /// fallback. Only an unchanged original seed can supply this edit base.
    fn memory_original_seed_history(
        &self,
        _owner: BotOwner,
        _reader: ProfileId,
        _identity: &BotMemoryIdentity,
        _binding: &BotMemoryBinding,
        _cancellation: &crate::Cancellation,
    ) -> Result<koru_store_core::ports::BoundedDocumentVersion, MemoryAuthorityError> {
        Err(BotError::Unavailable.into())
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
    /// Start one Run of a paused Routine now, under the Routine's own
    /// access, without turning it on. Answered `routine_test` once the
    /// Schedule's owner accepted the current minute; the Routine's history
    /// says what the minute became.
    TestRoutine {
        bot_id: BotId,
        schedule_id: koru_delegate_core::ScheduleId,
        #[cfg_attr(feature = "schema", schemars(range(min = 1)))]
        expected_definition_revision: u32,
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
    /// Staged Memory status contract; unavailable until complete Memory integration.
    ReadMemory {
        bot_id: BotId,
    },
    EnsureMemory {
        request_id: BotRequestId,
        bot_id: BotId,
        expected_revision: u64,
        title: String,
    },
    RetryMemory {
        bot_id: BotId,
        creation_request: BotRequestId,
        observed_state: BotMemoryState,
        observed_updated_at_ms: i64,
    },
    AbandonMemory {
        bot_id: BotId,
        creation_request: BotRequestId,
        observed_state: BotMemoryState,
        observed_updated_at_ms: i64,
    },
    ListMemory {
        bot_id: BotId,
        cursor: Option<String>,
        #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 20)))]
        limit: u16,
    },
    ReadMemoryFact {
        bot_id: BotId,
        fact_id: BotMemoryNodeId,
        cursor: Option<String>,
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
    /// Bounded owner roster for the external task facade, without configuration.
    ListTaskBots {
        workspace: WorkspaceId,
        after: Option<BotId>,
        limit: u16,
    },
    /// Read only the terminal assistant text of one exactly bound task.
    ReadTaskAnswer {
        bot_id: BotId,
        chat_id: ChatId,
        attempt_id: crate::InferenceAttemptId,
        #[cfg_attr(feature = "schema", schemars(with = "String"))]
        operation_id: uuid::Uuid,
        offset: u64,
    },
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
    /// The request cards of one conversation of a Bot that asked other Bots
    /// (M6): one per child request its Runs made there, oldest first, at
    /// most three per Run. Identities, states and counts only. Answered
    /// `child_requests`.
    ListChildRequests {
        bot_id: BotId,
        chat_id: ChatId,
    },
    /// Who asked for a conversation, when another Bot's request started it
    /// (M6). Answered `child_origin`, with no origin for any other
    /// conversation.
    ReadChildOrigin {
        bot_id: BotId,
        chat_id: ChatId,
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
    /// entries (1 to 500). Without `message_id`, the Run of a task started
    /// through the API (`start`/`continue`, which writes no reply) in
    /// `chat_id`, bound through the Bot's own admitted receipt; a reply's Run
    /// is read only through its reply. Answered `run` with the Run view
    /// `GET /v1/runs/{run}` answers, without `execution.session`: no
    /// stream, control or takeover handle.
    ReadRun {
        bot_id: BotId,
        chat_id: ChatId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<crate::MessageId>,
        run_id: koru_delegate_core::RunId,
        from: u64,
        #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 500)))]
        len: u32,
    },
    /// The person's own Stop for a task started through the API (`start` or
    /// `continue`), named by that operation's `request_id`, as
    /// `read_operation` names it. Its Run is stopped as a conversation's
    /// Stop and Archive stop theirs: a queued Run never starts, a running one
    /// ends even while a person holds its session; never the automatic
    /// interruption. Answered `task_stop`. A Stop only reduces work, so no
    /// hold refuses it. A task a conversation message started is not this
    /// command's: `not_found`.
    StopTask {
        request_id: BotRequestId,
        bot_id: BotId,
    },
}

/// The most history entries one [`BotCommand::ReadRun`] window carries: the
/// Run view's own bound.
pub const BOT_RUN_WINDOW_LIMIT: u32 = 500;

impl BotCommand {
    /// The shared staged Memory capability, independent of command mutation/read.
    pub fn is_memory(&self) -> bool {
        matches!(
            self,
            Self::ReadMemory { .. }
                | Self::EnsureMemory { .. }
                | Self::RetryMemory { .. }
                | Self::AbandonMemory { .. }
                | Self::ListMemory { .. }
                | Self::ReadMemoryFact { .. }
        )
    }

    /// An owner control of one Routine (P4.3): its Bot and its Schedule.
    pub fn routine_control(&self) -> Option<(BotId, koru_delegate_core::ScheduleId)> {
        match self {
            Self::SetRoutineLifecycle {
                bot_id,
                schedule_id,
                ..
            }
            | Self::TestRoutine {
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
    RoutineTest {
        test: BotRoutineTest,
    },
    RoutineAccess {
        access: BotRoutineAccess,
    },
    MemoryStatus {
        status: BotMemoryStatus,
    },
    MemoryList {
        page: BotMemoryListPage,
    },
    MemoryDetail {
        page: BotMemoryDetailPage,
    },
    Library {
        library: BotLibraryView,
    },
    KnowledgeSaved {
        receipt: BotKnowledgeReceipt,
    },
    TaskBots {
        page: BotSummaryPage,
    },
    TaskAnswer {
        answer: BotTaskAnswer,
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
    ChildRequests {
        requests: Vec<BotChildRequestCard>,
    },
    ChildOrigin {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<BotChildOrigin>,
    },
    Creation {
        creation: BotCreation,
    },
    Creations {
        page: BotCreationPage,
    },
    Run {
        view: koru_delegate_core::RunView,
    },
    TaskStop {
        stop: BotTaskStop,
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

/// Internal owner projection. Run identity is for the facade's authorized
/// result read; it is never an external control handle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotTaskAnswer {
    pub status: crate::AttemptStatus,
    pub message_id: crate::MessageId,
    pub run_id: Option<RunId>,
    pub markdown: Option<String>,
    pub next_offset: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotSummary {
    pub bot_id: BotId,
    pub name: String,
    pub main_chat: ChatId,
    pub revision: u64,
    pub paused: bool,
    pub lifecycle: BotLifecycle,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotSummaryPage {
    pub bots: Vec<BotSummary>,
    pub next: Option<BotId>,
}

/// What a [`BotCommand::StopTask`] found and did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotTaskStopOutcome {
    /// The task was queued or running, and this Stop ended it.
    Stopped,
    /// The Stop was delivered and its execution owner recorded the person's
    /// cancellation, but the Run's end is not proven yet (its process may
    /// still be exiting, or effects are still being drained). Ask again: a
    /// later `stop_task` answers `already_settled` once it has settled, or
    /// sends the Stop again.
    Stopping,
    /// It had already ended (finished, failed, or stopped before): nothing
    /// was sent.
    AlreadySettled,
    /// Its execution owner refused it before admitting it: it never ran.
    NeverAdmitted,
}

/// The answer to a [`BotCommand::StopTask`]: the task's operation, its Run
/// and what the Stop found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotTaskStop {
    pub request_id: BotRequestId,
    pub bot_id: BotId,
    pub run_id: RunId,
    pub outcome: BotTaskStopOutcome,
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
    /// precedence over `running` and `waiting` when it holds.
    NeedsYou,
    /// The reply asked other Bots and waits for their requests (M6). Takes
    /// precedence over `running`. [`BotActivity::waiting`] says on whom.
    Waiting,
}

/// One conversation with work in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotActivity {
    pub bot_id: BotId,
    pub chat_id: ChatId,
    pub state: BotActivityState,
    /// Present exactly when `state` is `waiting`: the Bots asked and whether
    /// this Bot is paused. Omitted otherwise, so an entry that does not wait
    /// serializes exactly as before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<BotActivityWaiting>,
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
