// SPDX-License-Identifier: MPL-2.0

//! Native Bot creation choices, window progress, and exact Role-page proofs.
//! The engine owns durable creation state; these views carry no executor.
use super::*;

/// The Store scope a Bot's Role page is created under. Distinct from every
/// other creation namespace, so a key here never replays another's page.
pub const BOT_ROLE_SCOPE: &str = "koru-bot-role";
/// The most characters an authored Role page title may hold.
pub const BOT_ROLE_TITLE_LIMIT: usize = 256;
/// The most bytes of authored Role text one creation carries.
pub const BOT_ROLE_MARKDOWN_LIMIT: usize = 256 * 1024;
/// The most unfinished creations one [`BotCommand::ListBotCreations`] page returns.
pub const BOT_CREATION_PAGE_LIMIT: u16 = 100;
/// The native creation record bound, including the prepared Role seed.
pub const BOT_RECEIPT_OUTCOME_LIMIT: usize = 1024 * 1024;

/// The Role a new Bot starts with.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotRoleInput {
    /// An ordinary page that already exists in the home workspace. It stays
    /// shared: the Bot does not own it.
    Existing { page: BotPage },
    /// A new ordinary page the owner creates in the home workspace from this
    /// title and Markdown, owned by the Bot through its creation receipt.
    Authored { title: String, markdown: String },
}

impl std::fmt::Debug for BotRoleInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Existing { page } => f.debug_struct("Existing").field("page", page).finish(),
            Self::Authored { .. } => f.write_str("Authored([REDACTED])"),
        }
    }
}

/// Where the new Bot's work happens.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotFolderChoice {
    /// A folder the owner creates for this Bot alone, at
    /// `<AI root>/bots/<BotId>/work`. Nothing in the request names it, and
    /// any other field beside `kind` is refused.
    // A struct variant: serde ignores unknown fields beside a unit variant
    // of an internally tagged enum even under `deny_unknown_fields`.
    Managed {},
    /// A folder that already exists, given as an absolute path. It is
    /// validated, never created or emptied.
    Selected { path: String },
}

impl std::fmt::Debug for BotFolderChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Managed {} => f.write_str("Managed"),
            Self::Selected { .. } => f.write_str("Selected([REDACTED])"),
        }
    }
}

/// A new Bot's name and runtime defaults. Its Role and folder come from the
/// creation's own choices.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotCreationConfig {
    pub name: String,
    #[serde(deserialize_with = "strict_model")]
    #[cfg_attr(feature = "schema", schemars(with = "StrictModel"))]
    pub model: ModelSelection,
    pub mode: Option<String>,
    pub fast: Option<bool>,
    /// The face the person picked ([`BotConfig::avatar`]). Absent means
    /// none: the Bot wears the default face. Omitted when absent, so
    /// a request that names no face has the bytes, and so the fingerprint,
    /// it had before the field existed, and a `creation_v2` receipt written
    /// before it still decodes and still matches its replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(range(min = 0, max = 23)))]
    pub avatar: Option<u8>,
}

impl std::fmt::Debug for BotCreationConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotCreationConfig").finish_non_exhaustive()
    }
}

impl BotCreationConfig {
    /// The Bot configuration this creation produces, once its Role page and
    /// folder binding are known.
    pub fn bot_config(&self, role: BotPage, working_folder: String) -> BotConfig {
        BotConfig {
            name: self.name.clone(),
            role,
            working_folder,
            model: self.model.clone(),
            mode: self.mode.clone(),
            fast: self.fast,
            avatar: self.avatar,
        }
    }

    /// The checks [`validate_config`] makes of these fields.
    pub fn validate(&self) -> Result<(), BotError> {
        if self.name.trim().is_empty()
            || self.name.len() > 256
            || self.model.requested_model.trim().is_empty()
            || self.model.requested_model.len() > 512
            || !self.model.provider.as_str().starts_with("agent-")
            || self
                .mode
                .as_ref()
                .is_some_and(|mode| mode.is_empty() || mode.len() > 128)
        {
            return Err(BotError::Invalid);
        }
        validate_avatar(self.avatar)
    }
}

/// A bounded authored Role: a title a page can carry, and Markdown within
/// [`BOT_ROLE_MARKDOWN_LIMIT`].
pub fn validate_role_text(title: &str, markdown: &str) -> Result<(), BotError> {
    if title.trim().is_empty()
        || title.chars().count() > BOT_ROLE_TITLE_LIMIT
        || title.chars().any(char::is_control)
        || markdown.len() > BOT_ROLE_MARKDOWN_LIMIT
        || markdown.contains('\0')
    {
        return Err(BotError::Invalid);
    }
    Ok(())
}

/// The most bytes a folder path or binding may hold.
pub const BOT_FOLDER_PATH_LIMIT: usize = 32768;

/// An absolute folder path a person may name.
pub fn validate_folder_path(path: &str) -> Result<(), BotError> {
    if path.is_empty()
        || path.len() > BOT_FOLDER_PATH_LIMIT
        || path.contains('\0')
        || !std::path::Path::new(path).is_absolute()
    {
        return Err(BotError::Invalid);
    }
    Ok(())
}

/// Where a creation stands, as the person reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotCreationStage {
    /// The owner is working on it.
    Pending,
    /// It stopped with a `reason`; the same request (Retry) resumes it.
    /// Whatever it already made is kept and named.
    Incomplete,
    /// The Bot exists.
    Completed,
    /// Refused before it made anything; the BotId is free again.
    Rejected,
    /// The person ended it ([`BotCommand::AbandonBotCreation`]). No Bot is
    /// made, the BotId stays reserved forever, and what it made is named in
    /// `left_behind`, never removed.
    Abandoned,
}

/// The step a creation reached. Each is durable before the effect it guards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotCreationPhase {
    /// The intent and the BotId reservation are recorded; nothing else.
    Intent,
    /// The folder may have been created; a recovery adopts exactly it.
    FolderPending,
    /// The folder binding is verified and recorded.
    FolderReady,
    /// The Role page may have been asked for; a recovery replays the same
    /// candidate, key and seed hash.
    RolePending,
    /// The Role page's original creation is verified against the seed.
    RoleVerified,
}

/// Why a creation stopped or was refused. Stable codes, never prose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotCreationReason {
    /// The owner, workspace or its placement could not be confirmed now.
    OwnerUnavailable,
    /// The Store did not answer, or its answer may have been lost.
    StoreUnavailable,
    /// The folder could not be created or read.
    FolderUnavailable,
    /// Something already occupies the managed folder on first acceptance.
    FolderOccupied,
    /// The folder resolves somewhere other than its intended binding.
    FolderMismatch,
    /// The selected folder is not an existing folder.
    FolderInvalid,
    /// The Role candidate exists with a different or unprovable origin.
    RoleConflict,
    /// The Role page is not a live page of the home workspace now.
    RoleUnavailable,
    /// A selected page is not readable now.
    PageUnavailable,
    /// The final transaction failed and rolled back, or a step could not be
    /// recorded; Retry repeats it, and a stalled step is retried by itself.
    StorageFailed,
    /// The Role page was created and is no longer in the home workspace:
    /// deleted permanently, or its registration never completed. Koru never
    /// registers it again, so Retry cannot finish this; Abandon ends it.
    RoleMissing,
    /// The Store refused the Role page definitely (its input was invalid).
    /// Retry would be refused the same way; Abandon ends it.
    StoreRefused,
}

impl BotCreationReason {
    /// Every reason, for bounds that must hold whichever is recorded.
    pub const ALL: &[Self] = &[
        Self::OwnerUnavailable,
        Self::StoreUnavailable,
        Self::FolderUnavailable,
        Self::FolderOccupied,
        Self::FolderMismatch,
        Self::FolderInvalid,
        Self::RoleConflict,
        Self::RoleUnavailable,
        Self::PageUnavailable,
        Self::StorageFailed,
        Self::RoleMissing,
        Self::StoreRefused,
    ];

    /// Whether Retry can resolve an incomplete creation parked for this
    /// reason, perhaps once the person has fixed what it names (restored the
    /// Role page from Trash, emptied the folder). The others are terminal
    /// for this creation: only Abandon ends it.
    pub const fn retryable(self) -> bool {
        !matches!(
            self,
            Self::RoleConflict
                | Self::RoleMissing
                | Self::StoreRefused
                | Self::FolderOccupied
                | Self::FolderInvalid
        )
    }
}

/// What an abandoned creation leaves in place for the person: a Role page
/// it created, and the managed folder it made. Nothing is removed; ordinary
/// page and file actions deal with them. Something the creation never got
/// to make is not named.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotCreationLeftovers {
    /// The authored Role page, when the Store holds it or may: asked and
    /// its answer not seen (then `role_page_unconfirmed` is true on the
    /// creation). Not named when the Store refused it, so nothing was
    /// written, or when it is gone for good.
    pub role_page: Option<BotPage>,
    /// The managed folder, when it is there: the abandon and every later
    /// read look at the disk. Also named, as possibly there, when the look
    /// failed or a step of the creation was still under way when it was
    /// abandoned. A selected folder was the person's own and is never named
    /// here.
    pub folder: Option<String>,
}

/// What a look at a creation's managed folder found when an answer was
/// made. Not on the wire: it decides only whether `left_behind` names the
/// folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotFolderSeen {
    /// Something is at the folder's path.
    Present,
    /// Nothing is at the folder's path, and no step of the creation can
    /// make it any more.
    Absent,
    /// Not looked at, the look failed, or a step may still make it.
    Unknown,
}

/// One creation, as its authorized owner reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotCreation {
    pub request_id: BotRequestId,
    pub bot_id: BotId,
    pub home_workspace: WorkspaceId,
    pub name: String,
    pub stage: BotCreationStage,
    pub phase: BotCreationPhase,
    pub reason: Option<BotCreationReason>,
    /// Whether Retry can resolve this `incomplete` creation; false for every
    /// other stage and for a terminal reason (only Abandon ends it).
    #[serde(default)]
    pub retryable: bool,
    /// The Role page: an existing one from the start; an authored one from
    /// the moment the Store may have been asked for it (`role_pending`), so
    /// the person can always find it, also while the Bot is not created yet.
    pub role_page: Option<BotPage>,
    /// True while an authored `role_page` is not proven to exist: the Store
    /// was asked and its answer was not seen, or the page is gone.
    #[serde(default)]
    pub role_page_unconfirmed: bool,
    /// The canonical folder the Bot is or will be bound to, from
    /// `folder_ready`: where a selected path or link resolves, so a client
    /// can show it when it differs from what was typed.
    #[serde(default)]
    pub folder_binding: Option<String>,
    /// For an `abandoned` creation, what it made and left in place.
    #[serde(default)]
    pub left_behind: Option<BotCreationLeftovers>,
    /// The Bot, once `completed` and while it is readable.
    pub bot: Option<Bot>,
}

/// The position after the last creation of a page (oldest first).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotCreationCursor {
    pub created_at_ms: i64,
    pub request_id: BotRequestId,
}

/// One page of a workspace's unfinished creations, oldest first;
/// `next_cursor` is absent on the last page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotCreationPage {
    pub creations: Vec<BotCreation>,
    pub next_cursor: Option<BotCreationCursor>,
}

/// Where a Role page whose origin is known stands in its workspace now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotRolePlacement {
    /// A live ordinary page of the workspace.
    Live,
    /// In the workspace's Trash (itself or an ancestor).
    Trashed,
    /// Not in the workspace at all: deleted permanently, or never
    /// registered. It is never registered again.
    Missing,
}

/// What the strict Store creation of a Role page answers: the stored
/// original origin, and where the page stands in its workspace now.
#[derive(Clone, PartialEq, Eq)]
pub struct BotRoleProof {
    pub scope: String,
    pub request_key: String,
    pub request_hash: String,
    pub initial_update: Vec<u8>,
    pub placement: BotRolePlacement,
}
impl std::fmt::Debug for BotRoleProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotRoleProof")
            .field("placement", &self.placement)
            .finish_non_exhaustive()
    }
}

/// One strict Role creation, exactly as the receipt recorded it.
#[derive(Clone)]
pub struct BotRoleSeed {
    pub owner: BotOwner,
    pub workspace: WorkspaceId,
    pub profile: ProfileId,
    pub title: String,
    pub candidate: DocumentId,
    pub request_key: String,
    pub request_hash: String,
    pub seed: Vec<u8>,
}

impl BotRoleProof {
    /// The stored origin is exactly this creation's: same scope, key, hash
    /// and first-update bytes. A matching title or body proves nothing.
    pub fn matches(&self, seed: &BotRoleSeed) -> bool {
        self.scope == BOT_ROLE_SCOPE
            && self.request_key == seed.request_key
            && self.request_hash == seed.request_hash
            && self.initial_update == seed.seed
    }
}

#[cfg(test)]
mod tests;
