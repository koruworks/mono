// SPDX-License-Identifier: MPL-2.0

//! Creating a Bot together with its Role page and working folder (P1.8 B2).
//!
//! The request names what the person chose; the owner derives everything
//! else. A new Role page is planted by the Store's strict creation origin
//! with a candidate, request key and seed hash that are durable in the
//! `creation_v2` receipt before the Store is asked, so a page that exists
//! while its Bot does not is always named by a receipt. The managed folder
//! is derived only from the BotId under the AI root. Nothing here deletes a
//! folder or moves a page to Trash.
use super::*;
mod identity;
pub use identity::{BotCreationIdentity, bot_creation_identity};

/// The Store scope a Bot's Role page is created under. Distinct from every
/// other creation namespace, so a key here never replays another's page.
pub const BOT_ROLE_SCOPE: &str = "koru-bot-role";
/// The most characters an authored Role page title may hold.
pub const BOT_ROLE_TITLE_LIMIT: usize = 256;
/// The most bytes of authored Role text one creation carries.
pub const BOT_ROLE_MARKDOWN_LIMIT: usize = 256 * 1024;
/// The most unfinished creations one [`BotCommand::ListBotCreations`] page returns.
pub const BOT_CREATION_PAGE_LIMIT: u16 = 100;
/// The bound of the whole stored receipt outcome, seed included (ai16's CHECK).
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
    /// none: the Bot's face is derived from its id. Omitted when absent, so
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

/// The managed folder of a Bot: derived only from its BotId under the AI
/// root, never from a name or anything a request says.
pub fn managed_folder(root: &std::path::Path, bot: BotId) -> std::path::PathBuf {
    root.join("bots").join(bot.to_storage()).join("work")
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

/// Opaque bytes stored as lowercase hex inside the JSON receipt.
#[derive(Clone, PartialEq, Eq)]
pub struct BotSeedBytes(pub Vec<u8>);
impl std::fmt::Debug for BotSeedBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BotSeedBytes({} bytes)", self.0.len())
    }
}
impl Serialize for BotSeedBytes {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(self.0.len() * 2);
        for byte in &self.0 {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 15) as usize] as char);
        }
        serializer.serialize_str(&out)
    }
}
impl<'de> Deserialize<'de> for BotSeedBytes {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let digit = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        if text.len() % 2 != 0 {
            return Err(serde::de::Error::custom("odd hex length"));
        }
        text.as_bytes()
            .chunks(2)
            .map(|pair| Some(digit(pair[0])? << 4 | digit(pair[1])?))
            .collect::<Option<Vec<u8>>>()
            .map(Self)
            .ok_or_else(|| serde::de::Error::custom("not lowercase hex"))
    }
}

/// The Role a creation records: an existing reference, or everything a
/// recovery needs to replay one Store creation byte for byte.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotCreationRole {
    Existing {
        page: BotPage,
    },
    Authored {
        /// The Store request key, minted once for the operation's lifetime.
        store_request: uuid::Uuid,
        /// The page's identity, minted once for the operation's lifetime.
        candidate: DocumentId,
        title: String,
        /// The exact prepared page bytes. Recovery input only; dropped when
        /// the creation completes.
        seed: Option<BotSeedBytes>,
        /// blake3 of `seed`: the Store request hash. Kept after completion.
        seed_hash: String,
        /// blake3 of the authored title and Markdown. Kept after completion.
        request_hash: String,
    },
}

/// The folder a creation records. A managed folder carries the trusted root
/// and the path derived from it; nothing else is ever a cleanup target,
/// because nothing is ever cleaned up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotCreationFolder {
    Managed { root: String, path: String },
    Selected { path: String },
}

/// The `creation_v2` receipt body (P1.8 §1). Strict: an unknown field, a
/// version other than 2 or an inconsistent group refuses to decode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotCreationRecord {
    pub bot: BotId,
    pub main_chat: ChatId,
    pub user_scope: UserScopeId,
    pub owner_device: DeviceId,
    pub home_workspace: WorkspaceId,
    /// The destination Profile under the authority that accepted the intent.
    pub profile: ProfileId,
    pub config: BotCreationConfig,
    pub access: BotAccess,
    pub selected_pages: Vec<BotPage>,
    pub role: BotCreationRole,
    pub folder: BotCreationFolder,
    pub phase: BotCreationPhase,
    pub reason: Option<BotCreationReason>,
    /// The verified canonical folder, from `folder_ready`.
    pub folder_binding: Option<String>,
    /// The verified Role page, from `role_verified` (authored only).
    pub role_page: Option<BotPage>,
    pub created_at_ms: i64,
    /// The person ended the creation (outer stage `completed`, no Bot).
    /// `reason` keeps why it had stopped, if it had.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub abandoned: bool,
}

fn hex64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

impl BotCreationRecord {
    /// Field and group consistency. Stage consistency is the repository's,
    /// which knows the receipt's outer stage.
    pub fn validate(&self) -> Result<(), BotError> {
        self.config.validate()?;
        let home = self.home_workspace;
        let mut pages = self.selected_pages.clone();
        normalize_pages(&mut pages, home)?;
        if pages != self.selected_pages || self.created_at_ms < 0 {
            return Err(BotError::Invalid);
        }
        match &self.folder {
            BotCreationFolder::Managed { root, path } => {
                if root.is_empty()
                    || *path
                        != managed_folder(std::path::Path::new(root), self.bot)
                            .to_str()
                            .ok_or(BotError::Invalid)?
                {
                    return Err(BotError::Invalid);
                }
            }
            BotCreationFolder::Selected { path } => validate_folder_path(path)?,
        }
        if (self.phase >= BotCreationPhase::FolderReady) != self.folder_binding.is_some()
            || self
                .folder_binding
                .as_ref()
                .is_some_and(|binding| validate_folder_path(binding).is_err())
        {
            return Err(BotError::Invalid);
        }
        match &self.role {
            BotCreationRole::Existing { page } => {
                if page.workspace != home
                    || self.phase > BotCreationPhase::FolderReady
                    || self.role_page.is_some()
                {
                    return Err(BotError::Invalid);
                }
            }
            BotCreationRole::Authored {
                candidate,
                title,
                seed,
                seed_hash,
                request_hash,
                ..
            } => {
                validate_role_text(title, "")?;
                if !hex64(seed_hash)
                    || !hex64(request_hash)
                    || seed
                        .as_ref()
                        .is_some_and(|seed| seed.0.is_empty() || seed.0.len() > 16 * 1024 * 1024)
                    || (self.phase == BotCreationPhase::RoleVerified)
                        != (self.role_page
                            == Some(BotPage {
                                workspace: home,
                                document: *candidate,
                            }))
                    || (self.phase != BotCreationPhase::RoleVerified && self.role_page.is_some())
                {
                    return Err(BotError::Invalid);
                }
            }
        }
        Ok(())
    }

    /// The Role page the Bot points at once created.
    pub fn role_target(&self) -> BotPage {
        match &self.role {
            BotCreationRole::Existing { page } => page.clone(),
            BotCreationRole::Authored { candidate, .. } => BotPage {
                workspace: self.home_workspace,
                document: *candidate,
            },
        }
    }

    pub fn authored(&self) -> bool {
        matches!(self.role, BotCreationRole::Authored { .. })
    }

    /// Whether a Role page of this creation may exist: the Store may have
    /// been asked, and it did not answer that the identity holds a page of
    /// another origin (which the Store refuses before any effect).
    pub fn may_hold_role_page(&self) -> bool {
        self.authored()
            && self.phase >= BotCreationPhase::RolePending
            && self.reason != Some(BotCreationReason::RoleConflict)
    }

    /// The person ended this creation.
    pub fn abandoned(&self) -> bool {
        self.abandoned
    }

    /// What this creation made, or may have made, and leaves in place.
    ///
    /// The durable phase says what may exist (each mark is written before
    /// the effect it guards); it never says what does. So:
    /// - the managed folder is named from `folder_pending` on, unless a look
    ///   at the disk found nothing there ([`BotFolderSeen::Absent`]): a mkdir
    ///   that was refused leaves no folder to name;
    /// - the Role page is named while the Store may hold it, never when the
    ///   Store refused it definitely (`store_refused`: nothing was written)
    ///   or when it is gone for good (`role_missing`).
    pub fn left_behind(&self, folder: BotFolderSeen) -> BotCreationLeftovers {
        BotCreationLeftovers {
            role_page: (self.may_hold_role_page()
                && !matches!(
                    self.reason,
                    Some(BotCreationReason::StoreRefused | BotCreationReason::RoleMissing)
                ))
            .then(|| self.role_target()),
            folder: match &self.folder {
                BotCreationFolder::Managed { path, .. }
                    if self.phase >= BotCreationPhase::FolderPending
                        && folder != BotFolderSeen::Absent =>
                {
                    Some(self.folder_binding.clone().unwrap_or_else(|| path.clone()))
                }
                _ => None,
            },
        }
    }

    /// The managed folder this creation makes, if it makes one.
    pub fn managed_folder(&self) -> Option<&str> {
        match &self.folder {
            BotCreationFolder::Managed { path, .. } => Some(path),
            BotCreationFolder::Selected { .. } => None,
        }
    }

    /// Every effect is verified: the final transaction may run.
    pub fn ready(&self) -> bool {
        self.reason.is_none()
            && match self.role {
                BotCreationRole::Existing { .. } => self.phase == BotCreationPhase::FolderReady,
                BotCreationRole::Authored { .. } => self.phase == BotCreationPhase::RoleVerified,
            }
    }

    /// The Bot the final transaction writes, built only from this record.
    pub fn bot(&self, now: i64) -> Result<Bot, BotError> {
        let folder = self.folder_binding.clone().ok_or(BotError::Invalid)?;
        let config = self.config.bot_config(self.role_target(), folder);
        validate_config(&config, self.home_workspace)?;
        Ok(Bot {
            id: self.bot,
            home_workspace: self.home_workspace,
            user_scope: self.user_scope,
            owner_device: self.owner_device,
            config,
            access: self.access,
            selected_pages: self.selected_pages.clone(),
            revision: 1,
            policy_revision: 1,
            main_chat: self.main_chat,
            paused: false,
            folder_lock_request: None,
            folder_frozen_at_ms: None,
            created_at_ms: now,
            updated_at_ms: now,
            lifecycle: Default::default(),
            open_operation: None,
            held_by: None,
            pending_change: None,
            connections: Default::default(),
        })
    }

    /// The same intent with its prepared seed dropped, as a completed
    /// receipt keeps it: fingerprints, identities and proof stay.
    pub fn without_seed(&self) -> Self {
        let mut kept = self.clone();
        if let BotCreationRole::Authored { seed, .. } = &mut kept.role {
            *seed = None;
        }
        kept
    }

    /// This intent with the most evidence any later step can add: its last
    /// phase, the verified Role page, the longest reason and the longest
    /// folder binding a binding may be, every byte of it escaped in JSON.
    /// T1 encodes this, so no later step can outgrow the bound, least of all
    /// after the Role page exists.
    pub fn with_maximal_evidence(&self) -> Self {
        let mut most = self.clone();
        most.phase = match self.role {
            BotCreationRole::Existing { .. } => BotCreationPhase::FolderReady,
            BotCreationRole::Authored { .. } => BotCreationPhase::RoleVerified,
        };
        if self.authored() {
            most.role_page = Some(self.role_target());
        }
        most.reason = BotCreationReason::ALL
            .iter()
            .copied()
            .max_by_key(|reason| serde_json::to_string(reason).map_or(0, |s| s.len()));
        let mut binding = match &self.folder {
            BotCreationFolder::Managed { path, .. } | BotCreationFolder::Selected { path } => {
                path.clone()
            }
        };
        // A control character is the most a byte grows in JSON (`\u0001`).
        while binding.len() < BOT_FOLDER_PATH_LIMIT {
            binding.push('\u{1}');
        }
        binding.truncate(BOT_FOLDER_PATH_LIMIT);
        most.folder_binding = Some(binding);
        most.abandoned = true;
        most
    }

    /// The receipt outcome this record is stored as, within
    /// [`BOT_RECEIPT_OUTCOME_LIMIT`]. Oversize input refuses before any effect.
    pub fn encode(&self) -> Result<String, BotError> {
        let outcome = BotReceiptOutcome::CreationV2 {
            version: 2,
            creation: self.clone(),
        };
        outcome.validate()?;
        let json = serde_json::to_string(&outcome).map_err(|_| BotError::Invalid)?;
        if json.len() > BOT_RECEIPT_OUTCOME_LIMIT {
            return Err(BotError::Invalid);
        }
        Ok(json)
    }

    /// A stored outcome as a creation: `None` for a valid outcome of another
    /// kind (a legacy Create's), `Storage` for anything that does not decode,
    /// including an unknown version. Never reinterpreted as something else.
    pub fn decode(json: &str) -> Result<Option<Self>, BotError> {
        if json.len() > BOT_RECEIPT_OUTCOME_LIMIT {
            return Err(BotError::Storage);
        }
        let outcome: BotReceiptOutcome =
            serde_json::from_str(json).map_err(|_| BotError::Storage)?;
        outcome.validate().map_err(|_| BotError::Storage)?;
        Ok(match outcome {
            BotReceiptOutcome::CreationV2 { creation, .. } => Some(creation),
            _ => None,
        })
    }

    /// Whether `next` keeps every immutable field of `self`: only the
    /// phase, reason and verified evidence may move, the phase only on,
    /// `abandoned` may only be set, and the prepared seed may only be
    /// dropped, never changed.
    pub fn same_intent(&self, next: &Self) -> bool {
        let seed = |record: &Self| match &record.role {
            BotCreationRole::Authored { seed, .. } => seed.clone(),
            BotCreationRole::Existing { .. } => None,
        };
        let mut a = self.without_seed();
        let mut b = next.without_seed();
        for record in [&mut a, &mut b] {
            record.phase = BotCreationPhase::Intent;
            record.reason = None;
            record.folder_binding = None;
            record.role_page = None;
            record.abandoned = false;
        }
        a == b
            && next.phase >= self.phase
            && (next.abandoned || !self.abandoned)
            && seed(next).is_none_or(|kept| seed(self) == Some(kept))
    }
}

/// One `creation_v2` receipt with its outer stage, as the repository reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotCreationReceipt {
    pub request: BotRequestId,
    pub record: BotCreationRecord,
    pub fingerprint: String,
    pub stage: BotReceiptStage,
}

impl BotCreationReceipt {
    /// The owner's view of this creation; `bot` is the caller's current read.
    /// An abandoned creation's managed folder is named as possibly left;
    /// [`Self::view_seen`] says what a look at the disk found.
    pub fn view(&self, bot: Option<Bot>) -> BotCreation {
        self.view_seen(bot, BotFolderSeen::Unknown)
    }

    /// [`Self::view`], with what a look at the managed folder found.
    pub fn view_seen(&self, bot: Option<Bot>, folder: BotFolderSeen) -> BotCreation {
        let record = &self.record;
        let stage = match self.stage {
            BotReceiptStage::Completed if record.abandoned() => BotCreationStage::Abandoned,
            BotReceiptStage::Completed => BotCreationStage::Completed,
            BotReceiptStage::Rejected => BotCreationStage::Rejected,
            _ if record.reason.is_some() => BotCreationStage::Incomplete,
            _ => BotCreationStage::Pending,
        };
        // An authored page is shown from the moment the Store may have been
        // asked for it; it is confirmed only once its origin is verified.
        let (role_page, role_page_unconfirmed) = match &record.role {
            BotCreationRole::Existing { page } => (Some(page.clone()), false),
            BotCreationRole::Authored { .. } if record.may_hold_role_page() => (
                Some(record.role_target()),
                record.phase != BotCreationPhase::RoleVerified
                    || record.reason == Some(BotCreationReason::RoleMissing),
            ),
            BotCreationRole::Authored { .. } => (None, false),
        };
        BotCreation {
            request_id: self.request,
            bot_id: record.bot,
            home_workspace: record.home_workspace,
            name: record.config.name.clone(),
            stage,
            phase: record.phase,
            reason: record.reason,
            retryable: stage == BotCreationStage::Incomplete
                && record.reason.is_some_and(BotCreationReason::retryable),
            role_page,
            role_page_unconfirmed,
            folder_binding: record.folder_binding.clone(),
            left_behind: (stage == BotCreationStage::Abandoned).then(|| record.left_behind(folder)),
            bot: (stage == BotCreationStage::Completed)
                .then_some(bot)
                .flatten(),
        }
    }

    /// Unfinished: the owner still has work to do or to resume.
    pub fn open(&self) -> bool {
        matches!(
            self.stage,
            BotReceiptStage::Prepared | BotReceiptStage::Accepted
        )
    }
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
