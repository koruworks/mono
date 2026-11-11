// SPDX-License-Identifier: MPL-2.0
//! Creating a Bot together with its Role page and managed folder (P1.8 B2).
//!
//! Every effect is a finite job on the bounded provider-administration pool;
//! the writer only records, schedules and applies. In order:
//!
//! 0. **Preparation**, on the pool: fresh authority, model discovery, the
//!    selected folder validated, the authored seed built and every identity
//!    minted. Nothing is written.
//! 1. **T1**, the writer: the intent and the BotId reservation, one row.
//!    No folder, Store call, Bot or Chat exists before it commits.
//! 2. **Folder**: the writer marks `folder_pending`, then a job creates
//!    exactly `<AI root>/bots/<BotId>/work` (or validates a selected folder),
//!    checking canonical containment level by level. **T2** records it.
//! 3. **Role** (authored only): the writer marks `role_pending`, then a job
//!    asks the Store's strict creation origin with the recorded candidate,
//!    key, hash and seed, and checks the stored origin against the seed.
//!    **T3** records it, stage `accepted`.
//! 4. A job rechecks authority; **T4** on the writer writes the Bot, its
//!    main Chat, the association and selected pages and completes the
//!    receipt in one transaction.
//!
//! A failure parks the creation with its reason; the same request (Retry)
//! or a restart resumes it from its recorded phase, reauthorizing first.
//! Recovery adopts only the exact canonical folder the intent names and
//! replays only the recorded candidate. Nothing here removes a folder, and
//! nothing moves a Role page to Trash.
use super::*;
use koru_ai_core::ProfileId;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Distinct BotIds one startup-scan step visits (two index seeks each).
const SCAN_STEP: u16 = 16;

/// The filesystem as creation uses it: no removal of any kind exists here,
/// so no creation path can clean anything up.
pub(in crate::worker) trait BotFolderSystem: Send + Sync {
    /// Resolve every link; an error when the path does not exist.
    fn canonical(&self, path: &Path) -> std::io::Result<PathBuf>;
    /// Whether anything (a folder, file or link) is at `path`, without
    /// following a final link.
    fn occupied(&self, path: &Path) -> std::io::Result<bool>;
    /// Create exactly one folder; `AlreadyExists` when anything is there.
    fn create_dir(&self, path: &Path) -> std::io::Result<()>;
    /// Whether `path` resolves to a folder.
    fn is_dir(&self, path: &Path) -> std::io::Result<bool>;
    /// Whether the folder at `path` holds nothing at all.
    fn is_empty_dir(&self, path: &Path) -> std::io::Result<bool>;
}

/// The real filesystem.
pub(in crate::worker) struct NativeFolders;
impl BotFolderSystem for NativeFolders {
    fn canonical(&self, path: &Path) -> std::io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }
    fn occupied(&self, path: &Path) -> std::io::Result<bool> {
        match std::fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
    fn create_dir(&self, path: &Path) -> std::io::Result<()> {
        std::fs::create_dir(path)
    }
    fn is_dir(&self, path: &Path) -> std::io::Result<bool> {
        Ok(std::fs::metadata(path)?.is_dir())
    }
    fn is_empty_dir(&self, path: &Path) -> std::io::Result<bool> {
        Ok(std::fs::read_dir(path)?.next().is_none())
    }
}

/// Koru's own data, which no selected folder may be: the AI root, the
/// Store's data root, and the AI root's `bots/` (every managed folder).
#[derive(Clone, Debug, Default)]
pub(in crate::worker) struct ReservedFolders {
    pub(in crate::worker) ai_root: PathBuf,
    pub(in crate::worker) store_root: Option<PathBuf>,
}

impl ReservedFolders {
    /// Whether the canonical `folder` is one of them. Each root is compared
    /// in its canonical form when it can be read, as written otherwise.
    fn holds(&self, fs: &dyn BotFolderSystem, folder: &Path) -> bool {
        let canonical = |path: &Path| fs.canonical(path).unwrap_or_else(|_| path.to_path_buf());
        let ai_root = canonical(&self.ai_root);
        folder == ai_root
            || folder.starts_with(ai_root.join("bots"))
            || self
                .store_root
                .as_deref()
                .is_some_and(|store| folder == canonical(store))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::worker) enum FolderFailure {
    /// Something is at the managed folder on first acceptance.
    Occupied,
    /// A level resolves somewhere other than where it was made.
    Mismatch,
    /// The selected folder does not exist or is not a folder, or its path
    /// cannot be stored with the same meaning.
    Invalid,
    /// The selected folder is Koru's own data ([`ReservedFolders`]).
    Reserved,
    /// The filesystem did not answer.
    Unavailable,
}

/// A canonical folder as it is stored and shown. Windows' verbatim prefix
/// is dropped, so programs launched in the folder do not see it, but only
/// when the path without it resolves back to exactly the same folder: a
/// trailing dot or space, a reserved device name or an over-long path can
/// mean something else without the prefix, and such a folder is refused
/// rather than bound to a different meaning. A binding must also fit the
/// stored bound.
fn display(fs: &dyn BotFolderSystem, path: &Path) -> Result<String, FolderFailure> {
    let text = path.to_str().ok_or(FolderFailure::Invalid)?;
    let stripped = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        Some(format!(r"\\{rest}"))
    } else {
        text.strip_prefix(r"\\?\").map(str::to_owned)
    };
    let shown = match stripped {
        None => text.to_owned(),
        Some(stripped) => match fs.canonical(Path::new(&stripped)) {
            Ok(back) if back == path => stripped,
            Ok(_) => return Err(FolderFailure::Invalid),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(FolderFailure::Invalid);
            }
            Err(error) => return Err(io(error)),
        },
    };
    validate_folder_path(&shown).map_err(|_| FolderFailure::Invalid)?;
    Ok(shown)
}

fn io(_: std::io::Error) -> FolderFailure {
    FolderFailure::Unavailable
}

/// One level under a canonical parent: made if absent, and refused unless it
/// is a folder that resolves to exactly where it was made.
fn ensure_level(fs: &dyn BotFolderSystem, level: &Path) -> Result<(), FolderFailure> {
    match fs.create_dir(level) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(io(error)),
    }
    if fs.canonical(level).map_err(io)? != level || !fs.is_dir(level).map_err(io)? {
        return Err(FolderFailure::Mismatch);
    }
    Ok(())
}

/// The managed folder: exactly `<root>/bots/<BotId>/work`, made level by
/// level under the canonical root. A link anywhere on the way that resolves
/// elsewhere refuses.
///
/// `first` is true only for the folder job the writer dispatches in the
/// same step that durably marked `folder_pending`: nothing of this creation
/// can be at the path yet, so anything there is a conflict and nothing is
/// made. Any later job (a Retry, a restart, a lost answer) cannot know from
/// durable state whether an existing folder is the one it made, so it
/// adopts one only when it is a folder there exactly and empty; a folder
/// with anything in it is a mismatch. Adopting grants no ownership of the
/// folder, and nothing here ever removes anything.
pub(in crate::worker) fn prepare_managed(
    fs: &dyn BotFolderSystem,
    root: &Path,
    bot: BotId,
    first: bool,
) -> Result<String, FolderFailure> {
    let root = fs.canonical(root).map_err(io)?;
    let bots = root.join("bots");
    let parent = bots.join(bot.to_storage());
    let target = parent.join("work");
    if first && fs.occupied(&target).map_err(io)? {
        return Err(FolderFailure::Occupied);
    }
    ensure_level(fs, &bots)?;
    ensure_level(fs, &parent)?;
    let made = match fs.create_dir(&target) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if first {
                return Err(FolderFailure::Occupied);
            }
            false
        }
        Err(error) => return Err(io(error)),
    };
    if fs.canonical(&target).map_err(io)? != target || !fs.is_dir(&target).map_err(io)? {
        return Err(FolderFailure::Mismatch);
    }
    if !made && !fs.is_empty_dir(&target).map_err(io)? {
        return Err(FolderFailure::Mismatch);
    }
    display(fs, &target)
}

/// A selected folder: it exists, is a folder and is not Koru's own data.
/// Validated, never made. Its binding is its canonical form.
pub(in crate::worker) fn check_selected(
    fs: &dyn BotFolderSystem,
    reserved: &ReservedFolders,
    path: &str,
) -> Result<String, FolderFailure> {
    validate_folder_path(path).map_err(|_| FolderFailure::Invalid)?;
    let canonical = match fs.canonical(Path::new(path)) {
        Ok(canonical) => canonical,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(FolderFailure::Invalid);
        }
        Err(error) => return Err(io(error)),
    };
    if !fs.is_dir(&canonical).map_err(io)? {
        return Err(FolderFailure::Invalid);
    }
    if reserved.holds(fs, &canonical) {
        return Err(FolderFailure::Reserved);
    }
    display(fs, &canonical)
}

/// The AI root a managed folder is made under must itself be storable
/// with its meaning (see [`display`]); checked in preparation, before T1.
fn check_managed_root(fs: &dyn BotFolderSystem, root: &Path) -> Result<(), FolderFailure> {
    let canonical = fs.canonical(root).map_err(io)?;
    display(fs, &canonical).map(|_| ())
}

fn folder_failure(error: FolderFailure) -> Failure {
    match error {
        FolderFailure::Invalid => failure(
            "invalid_request",
            "The selected folder is not an existing folder that can be used as it is named.",
            false,
        ),
        FolderFailure::Reserved => failure(
            "invalid_request",
            "Koru's own data folders cannot be a Bot's working folder.",
            false,
        ),
        _ => failure("folder_unavailable", "The folder could not be read.", true),
    }
}

/// "Change folder before the first task": the new folder must exist, be a
/// folder and not be Koru's own data. Off the writer, in the command's
/// preparation; the answer is the canonical binding Update stores.
pub(in crate::worker) fn require_existing_folder(
    fs: &dyn BotFolderSystem,
    reserved: &ReservedFolders,
    path: &str,
) -> ApiResult<String> {
    check_selected(fs, reserved, path).map_err(folder_failure)
}

fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn fingerprint(request: &BotCommand) -> ApiResult<String> {
    Ok(blake3_hex(
        &serde_json::to_vec(request).map_err(|_| bot_failure(BotError::Invalid))?,
    ))
}

/// The Store request of a recorded authored Role, exactly as T1 kept it.
fn role_seed(record: &BotCreationRecord) -> Option<BotRoleSeed> {
    let BotCreationRole::Authored {
        store_request,
        candidate,
        title,
        seed: Some(seed),
        seed_hash,
        ..
    } = &record.role
    else {
        return None;
    };
    Some(BotRoleSeed {
        owner: BotOwner {
            user_scope: record.user_scope,
            device: record.owner_device,
        },
        workspace: record.home_workspace,
        profile: record.profile,
        title: title.clone(),
        candidate: *candidate,
        request_key: store_request.to_string(),
        request_hash: seed_hash.clone(),
        seed: seed.0.clone(),
    })
}

/// What the writer drives, with the open creations' list. Nothing here is
/// durable: the receipts are. The writer is the only creator, so the list
/// stays exact once the startup scan has read every unfinished receipt.
pub(in crate::worker) struct CreationDriver {
    root: PathBuf,
    folders: Arc<dyn BotFolderSystem>,
    reserved: ReservedFolders,
    /// Off only in fixtures that have no runtime to ask.
    discover_models: bool,
    open: BTreeMap<(UserScopeId, BotRequestId), Tracked>,
    /// The startup scan's next BotId bound; `None` once it has finished.
    scan: Option<String>,
    /// The scan's own backoff after a failed step.
    scan_retry: Retry,
    /// Unfinished creations the scan found but could not read, retried with
    /// backoff. Each was reported once.
    unreadable: BTreeMap<(UserScopeId, BotRequestId), Retry>,
    /// A step is on the creation lane. At most one is, so a creation never
    /// waits behind another's queued work and the lane's queue never fills.
    stepping: bool,
}

/// A backoff: when a failed thing may be tried again, and how often it failed.
#[derive(Clone, Copy, Debug, Default)]
struct Retry {
    not_before: Option<Instant>,
    failures: u32,
}

impl Retry {
    fn ready(&self, now: Instant) -> bool {
        self.not_before.is_none_or(|at| at <= now)
    }
    /// One more failure: wait 1 s, doubling to at most 5 minutes. True for
    /// the first failure in a row, the one worth reporting.
    fn failed(&mut self, now: Instant) -> bool {
        self.failures = self.failures.saturating_add(1);
        let wait = Duration::from_secs(1u64 << self.failures.min(9).saturating_sub(1));
        self.not_before = Some(now + wait.min(Duration::from_secs(300)));
        self.failures == 1
    }
}

struct Tracked {
    /// The receipt without its seed: what lists and reads show.
    receipt: BotCreationReceipt,
    busy: bool,
    due: bool,
    retry: Retry,
    /// The writer could not record this creation's last step; it is retried
    /// from its durable phase with backoff and shown with this reason
    /// meanwhile, never as silently pending.
    stalled: Option<BotCreationReason>,
}

impl CreationDriver {
    pub(in crate::worker) fn new(root: PathBuf, folders: Arc<dyn BotFolderSystem>) -> Self {
        Self {
            reserved: ReservedFolders {
                ai_root: root.clone(),
                store_root: None,
            },
            root,
            folders,
            discover_models: true,
            open: BTreeMap::new(),
            scan: Some(String::new()),
            scan_retry: Retry::default(),
            unreadable: BTreeMap::new(),
            stepping: false,
        }
    }
    #[cfg(test)]
    pub(in crate::worker) fn without_model_discovery(mut self) -> Self {
        self.discover_models = false;
        self
    }
    /// The Store's data root, which no selected folder may be.
    pub(in crate::worker) fn with_store_root(mut self, store_root: Option<PathBuf>) -> Self {
        self.reserved.store_root = store_root;
        self
    }
    pub(in crate::worker) fn folders(&self) -> Arc<dyn BotFolderSystem> {
        self.folders.clone()
    }
    pub(in crate::worker) fn reserved(&self) -> ReservedFolders {
        self.reserved.clone()
    }
    fn track(&mut self, receipt: &BotCreationReceipt, due: bool) {
        let key = (receipt.record.user_scope, receipt.request);
        if !receipt.open() {
            self.open.remove(&key);
            return;
        }
        let shown = BotCreationReceipt {
            record: receipt.record.without_seed(),
            ..receipt.clone()
        };
        self.unreadable.remove(&key);
        let entry = self.open.entry(key).or_insert(Tracked {
            receipt: shown.clone(),
            busy: false,
            due: false,
            retry: Retry::default(),
            stalled: None,
        });
        entry.receipt = shown;
        entry.due |= due && receipt.record.reason.is_none();
    }

    /// A step's write or read failed: retry it from its durable phase after
    /// a backoff, and show why meanwhile. Reported once per run of failures.
    fn stall(&mut self, key: (UserScopeId, BotRequestId), reason: BotCreationReason) {
        if let Some(tracked) = self.open.get_mut(&key) {
            tracked.due = true;
            tracked.stalled = Some(reason);
            if tracked.retry.failed(Instant::now()) {
                tracing::warn!(target: "koru_ai::bots", request = %key.1, reason = ?reason, "a Bot creation step could not be recorded; retrying with backoff");
            }
        }
    }

    /// A step was recorded: no backoff, nothing to show.
    fn recorded(&mut self, key: (UserScopeId, BotRequestId)) {
        if let Some(tracked) = self.open.get_mut(&key)
            && tracked.stalled.take().is_some()
        {
            tracked.retry = Retry::default();
            tracing::info!(target: "koru_ai::bots", request = %key.1, "a stalled Bot creation is moving again");
        }
    }

    /// The view of a tracked creation, with a stall shown as incomplete.
    fn shown(&self, key: &(UserScopeId, BotRequestId), mut view: BotCreation) -> BotCreation {
        if view.stage == BotCreationStage::Pending
            && let Some(reason) = self.open.get(key).and_then(|tracked| tracked.stalled)
        {
            view.stage = BotCreationStage::Incomplete;
            view.reason = Some(reason);
        }
        view
    }
}

/// Preparation of one new creation, off the writer.
pub(in crate::worker) struct CreationPreparationJob {
    context: CommandContext,
    request: BotCommand,
    reply: SyncSender<ApiResult<BotResponse>>,
    authority: Arc<dyn BotAuthority>,
    folders: Arc<dyn BotFolderSystem>,
    reserved: ReservedFolders,
    root: PathBuf,
    discover_models: bool,
}
pub(in crate::worker) struct CreationPrepared {
    context: CommandContext,
    request: BotCommand,
    reply: SyncSender<ApiResult<BotResponse>>,
    authority: Result<BotAuthoritySnapshot, BotError>,
    /// The intent a first acceptance records. An exact retry ignores it, so
    /// a retry never needs the runtime online or rebuilds the seed.
    draft: ApiResult<BotCreationRecord>,
}

impl CreationPreparationJob {
    pub(in crate::worker) fn run(self, agents: Option<&AgentSessions>) -> CreationPrepared {
        let prepared =
            catch_unwind(AssertUnwindSafe(|| self.prepare(agents))).unwrap_or_else(|_| {
                (
                    Err(BotError::Unavailable),
                    Err(bot_failure(BotError::Unavailable)),
                )
            });
        CreationPrepared {
            context: self.context,
            request: self.request,
            reply: self.reply,
            authority: prepared.0,
            draft: prepared.1,
        }
    }

    fn prepare(
        &self,
        agents: Option<&AgentSessions>,
    ) -> (
        Result<BotAuthoritySnapshot, BotError>,
        ApiResult<BotCreationRecord>,
    ) {
        let BotCommand::CreateBotWithRole {
            bot_id,
            home_workspace,
            role,
            folder,
            config,
            access,
            selected_pages,
            ..
        } = &self.request
        else {
            return (Err(BotError::Invalid), Err(bot_failure(BotError::Invalid)));
        };
        let mut pages: Vec<BotPage> = selected_pages.clone();
        if let BotRoleInput::Existing { page } = role {
            pages.push(page.clone());
        }
        let authority = self.authority.inspect(&[*home_workspace], &pages);
        let draft = (|| -> ApiResult<BotCreationRecord> {
            let snapshot = authority.clone().map_err(bot_failure)?;
            config.validate().map_err(bot_failure)?;
            let probe = config.bot_config(
                BotPage {
                    workspace: *home_workspace,
                    document: DocumentId::new(),
                },
                String::new(),
            );
            if self.discover_models {
                validate_model_evidence(agents, &probe, false)?;
            }
            let mut selected = selected_pages.clone();
            normalize_pages(&mut selected, *home_workspace).map_err(bot_failure)?;
            let role = match role {
                BotRoleInput::Existing { page } => {
                    if page.workspace != *home_workspace {
                        return Err(bot_failure(BotError::Invalid));
                    }
                    BotCreationRole::Existing { page: page.clone() }
                }
                BotRoleInput::Authored { title, markdown } => {
                    validate_role_text(title, markdown).map_err(bot_failure)?;
                    let seed = koru_import_markdown::document_snapshot(
                        koru_core::WorkspaceId::from_uuid(home_workspace.as_uuid()),
                        title,
                        markdown,
                    )
                    .map_err(|_| {
                        failure(
                            "invalid_role",
                            "The Role text could not be prepared as a page.",
                            false,
                        )
                    })?;
                    let mut semantic = blake3::Hasher::new();
                    semantic.update(title.as_bytes());
                    semantic.update(&[0]);
                    semantic.update(markdown.as_bytes());
                    BotCreationRole::Authored {
                        store_request: Uuid::new_v4(),
                        candidate: DocumentId::new(),
                        title: title.clone(),
                        seed_hash: blake3_hex(&seed),
                        request_hash: semantic.finalize().to_hex().to_string(),
                        seed: Some(BotSeedBytes(seed)),
                    }
                }
            };
            let folder = match folder {
                BotFolderChoice::Managed {} => {
                    check_managed_root(self.folders.as_ref(), &self.root)
                        .map_err(folder_failure)?;
                    BotCreationFolder::Managed {
                        root: self
                            .root
                            .to_str()
                            .ok_or_else(|| bot_failure(BotError::Unavailable))?
                            .to_owned(),
                        path: managed_folder(&self.root, *bot_id)
                            .to_str()
                            .ok_or_else(|| bot_failure(BotError::Unavailable))?
                            .to_owned(),
                    }
                }
                BotFolderChoice::Selected { path } => {
                    check_selected(self.folders.as_ref(), &self.reserved, path)
                        .map_err(folder_failure)?;
                    BotCreationFolder::Selected { path: path.clone() }
                }
            };
            let record = BotCreationRecord {
                bot: *bot_id,
                main_chat: ChatId::new(),
                user_scope: snapshot.owner.user_scope,
                owner_device: snapshot.owner.device,
                home_workspace: *home_workspace,
                profile: snapshot.workspace(*home_workspace).map_err(bot_failure)?,
                config: config.clone(),
                access: *access,
                selected_pages: selected,
                role,
                folder,
                phase: BotCreationPhase::Intent,
                reason: None,
                folder_binding: None,
                role_page: None,
                created_at_ms: Timestamp::now().millis(),
            };
            // Oversize input refuses here, before any effect, with the most
            // evidence any later step can record already counted.
            record.with_maximal_evidence().encode().map_err(|_| {
                failure(
                    "invalid_request",
                    "The Role text is too large to create.",
                    false,
                )
            })?;
            Ok(record)
        })();
        (authority, draft)
    }
}

/// One finite owner step, off the writer.
pub(in crate::worker) struct CreationJob {
    key: (UserScopeId, BotRequestId),
    expected: BotCreationRecord,
    step: CreationStep,
    authority: Arc<dyn BotAuthority>,
    folders: Arc<dyn BotFolderSystem>,
    reserved: ReservedFolders,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::worker) enum CreationStep {
    Folder { first: bool },
    Role,
    Authorize,
}
pub(in crate::worker) struct CreationStepResult {
    key: (UserScopeId, BotRequestId),
    expected: BotCreationRecord,
    step: CreationStep,
    outcome: CreationOutcome,
}
#[derive(Debug)]
enum CreationOutcome {
    /// Authority refused or did not answer before the effect: nothing done.
    Owner,
    Folder(Result<String, FolderFailure>),
    Role(Result<BotRoleProof, BotError>),
    Authorized(Result<ProfileId, BotCreationReason>),
    /// The step stopped unexpectedly, possibly after its effect.
    Panicked,
}

impl CreationJob {
    pub(in crate::worker) fn run(self) -> CreationStepResult {
        let outcome =
            catch_unwind(AssertUnwindSafe(|| self.execute())).unwrap_or(CreationOutcome::Panicked);
        CreationStepResult {
            key: self.key,
            expected: self.expected,
            step: self.step,
            outcome,
        }
    }

    fn execute(&self) -> CreationOutcome {
        let record = &self.expected;
        let mut pages = Vec::new();
        if self.step == CreationStep::Authorize {
            pages.push(record.role_target());
            pages.extend(record.selected_pages.iter().cloned());
        }
        // The owner's authority is read again before every effect.
        let Ok(checked) = self.authority.inspect(&[record.home_workspace], &pages) else {
            return CreationOutcome::Owner;
        };
        if checked.owner.user_scope != record.user_scope
            || checked.owner.device != record.owner_device
            || checked.workspace(record.home_workspace) != Ok(record.profile)
        {
            return CreationOutcome::Owner;
        }
        match self.step {
            CreationStep::Folder { first } => CreationOutcome::Folder(match &record.folder {
                BotCreationFolder::Managed { root, .. } => {
                    prepare_managed(self.folders.as_ref(), Path::new(root), record.bot, first)
                }
                BotCreationFolder::Selected { path } => {
                    check_selected(self.folders.as_ref(), &self.reserved, path)
                }
            }),
            CreationStep::Role => CreationOutcome::Role(match role_seed(record) {
                Some(seed) if blake3_hex(&seed.seed) == seed.request_hash => {
                    self.authority.create_role_page(&seed)
                }
                _ => Err(BotError::Conflict),
            }),
            CreationStep::Authorize => {
                CreationOutcome::Authorized(if checked.page(&record.role_target()).is_err() {
                    Err(BotCreationReason::RoleUnavailable)
                } else if record
                    .selected_pages
                    .iter()
                    .any(|page| checked.page(page).is_err())
                {
                    Err(BotCreationReason::PageUnavailable)
                } else {
                    Ok(record.profile)
                })
            }
        }
    }
}

/// A creation read, in two owner checks off the writer: first who asks
/// (before any receipt is read), then the creation's home workspace.
pub(in crate::worker) struct CreationReadJob {
    context: CommandContext,
    request_id: BotRequestId,
    reply: SyncSender<ApiResult<BotResponse>>,
    authority: Arc<dyn BotAuthority>,
    found: Option<(BotOwner, BotCreation)>,
}
pub(in crate::worker) struct CreationReadResult {
    job: CreationReadJob,
    checked: Result<BotAuthoritySnapshot, BotError>,
}
impl CreationReadJob {
    pub(in crate::worker) fn run(self) -> CreationReadResult {
        let workspaces: Vec<WorkspaceId> = self
            .found
            .iter()
            .map(|(_, creation)| creation.home_workspace)
            .collect();
        let checked = catch_unwind(AssertUnwindSafe(|| {
            self.authority.inspect(&workspaces, &[])
        }))
        .unwrap_or(Err(BotError::Unavailable));
        CreationReadResult { job: self, checked }
    }
}

impl WorkerState {
    /// Creation and its read run their own owner checks; everything else
    /// keeps the ordinary preparation. `None` when this took the command.
    pub(in crate::worker) fn route_bot_creation(
        &mut self,
        command: WorkerCommand,
    ) -> Option<WorkerCommand> {
        let Some(authority) = self.bot_authority.clone() else {
            return Some(command);
        };
        let WorkerCommand::Bots {
            context,
            request,
            reply,
        } = command
        else {
            return Some(command);
        };
        let job = match &request {
            BotCommand::CreateBotWithRole { .. } => {
                ProviderAdminJob::BotCreationPrepare(CreationPreparationJob {
                    context,
                    request,
                    reply,
                    authority,
                    folders: self.bot_creations.folders.clone(),
                    reserved: self.bot_creations.reserved.clone(),
                    root: self.bot_creations.root.clone(),
                    discover_models: self.bot_creations.discover_models,
                })
            }
            BotCommand::ReadBotCreation { request_id } => {
                ProviderAdminJob::BotCreationRead(CreationReadJob {
                    context,
                    request_id: *request_id,
                    reply,
                    authority,
                    found: None,
                })
            }
            _ => {
                return Some(WorkerCommand::Bots {
                    context,
                    request,
                    reply,
                });
            }
        };
        self.submit_creation_job(job);
        None
    }

    /// Submit, or answer the caller at once when the pool is full or gone.
    fn submit_creation_job(&mut self, job: ProviderAdminJob) {
        let Some(pool) = self.provider_admin.as_ref() else {
            refuse_creation_job(job, bot_failure(BotError::Unavailable));
            return;
        };
        if let Err(error) = pool.try_submit(job) {
            refuse_creation_job(error.job, error.failure);
        }
    }

    /// T1, or an exact retry's progress. Current authority is checked before
    /// any receipt is read or replayed.
    pub(in crate::worker) fn complete_bot_creation_preparation(
        &mut self,
        prepared: CreationPrepared,
    ) {
        let CreationPrepared {
            context,
            request,
            reply,
            authority,
            draft,
        } = prepared;
        let result = self.begin_creation(&context, &request, authority, draft);
        let _ = reply.send(result);
    }

    fn begin_creation(
        &mut self,
        context: &CommandContext,
        request: &BotCommand,
        authority: Result<BotAuthoritySnapshot, BotError>,
        draft: ApiResult<BotCreationRecord>,
    ) -> ApiResult<BotResponse> {
        authorize(context, AiScope::ManageBots)?;
        let snapshot = authority.map_err(bot_failure)?;
        let owner = snapshot.owner;
        let BotCommand::CreateBotWithRole {
            request_id,
            bot_id,
            home_workspace,
            ..
        } = request
        else {
            return Err(bot_failure(BotError::Invalid));
        };
        let fingerprint = fingerprint(request)?;
        if let Some(existing) = self
            .repository
            .bot_creation(owner.user_scope, *request_id)
            .map_err(bot_failure)?
        {
            // Authority first: another device's request, or one whose home
            // workspace this owner cannot reach now, learns nothing more,
            // not even that its input differs.
            if existing.record.owner_device != owner.device {
                return Err(bot_failure(BotError::Forbidden));
            }
            snapshot
                .workspace(existing.record.home_workspace)
                .map_err(bot_failure)?;
            if existing.fingerprint != fingerprint || existing.record.bot != *bot_id {
                return Err(bot_failure(BotError::Conflict));
            }
            let existing = self.retry_creation(existing)?;
            return Ok(BotResponse::Creation {
                creation: self.creation_view(&existing),
            });
        }
        let record = draft?;
        if record.user_scope != owner.user_scope
            || record.owner_device != owner.device
            || record.home_workspace != *home_workspace
            || snapshot.workspace(*home_workspace) != Ok(record.profile)
        {
            return Err(bot_failure(BotError::Forbidden));
        }
        if let BotCreationRole::Existing { page } = &record.role {
            snapshot.page(page).map_err(bot_failure)?;
        }
        for page in &record.selected_pages {
            snapshot.page(page).map_err(bot_failure)?;
        }
        let receipt = self
            .repository
            .begin_bot_creation(
                *request_id,
                &record,
                &fingerprint,
                Timestamp::now().millis(),
            )
            .map_err(bot_failure)?;
        tracing::info!(target: "koru_ai::bots", request = %request_id, "Bot creation accepted");
        self.bot_creations.track(&receipt, true);
        Ok(BotResponse::Creation {
            creation: receipt.view(None),
        })
    }

    /// Retry: a parked creation resumes from its recorded phase; one that is
    /// moving, finished or refused only reports itself.
    fn retry_creation(&mut self, receipt: BotCreationReceipt) -> ApiResult<BotCreationReceipt> {
        let key = (receipt.record.user_scope, receipt.request);
        let busy = self.bot_creations.open.get(&key).is_some_and(|t| t.busy);
        if !receipt.open() || receipt.record.reason.is_none() || busy {
            if receipt.open() {
                self.bot_creations.track(&receipt, true);
                // A stalled one is tried again now, not after its backoff.
                if let Some(tracked) = self.bot_creations.open.get_mut(&key) {
                    tracked.retry.not_before = None;
                }
            }
            return Ok(receipt);
        }
        let mut resumed = receipt.record.clone();
        resumed.reason = None;
        let receipt = self
            .repository
            .advance_bot_creation(
                key.0,
                key.1,
                &receipt.record,
                &resumed,
                receipt.stage,
                Timestamp::now().millis(),
            )
            .map_err(bot_failure)?;
        self.bot_creations.track(&receipt, true);
        Ok(receipt)
    }

    fn creation_view(&self, receipt: &BotCreationReceipt) -> BotCreation {
        let bot = (receipt.stage == BotReceiptStage::Completed)
            .then(|| super::lifecycle::readable_bot(&self.repository, receipt.record.bot))
            .and_then(Result::ok)
            .flatten();
        self.bot_creations.shown(
            &(receipt.record.user_scope, receipt.request),
            receipt.view(bot),
        )
    }

    /// The startup scan, one bounded step per call, then at most one due
    /// creation's next step, and only while the creation lane is free.
    /// Called from the writer loop on every pass: when nothing is due, or a
    /// step is already on the lane, it reads nothing. A failure waits for its
    /// backoff; nothing is retried per pass.
    pub(in crate::worker) fn schedule_bot_creations(&mut self) {
        if self.bot_authority.is_none() || self.provider_admin.is_none() {
            return;
        }
        let now = Instant::now();
        if let Some(after) = self.bot_creations.scan.clone()
            && self.bot_creations.scan_retry.ready(now)
        {
            match self.repository.bot_creation_scan(&after, SCAN_STEP) {
                Ok(step) => {
                    for bot in &step.skipped {
                        tracing::warn!(target: "koru_ai::bots", bot = %bot, "an unfinished Bot creation's receipt could not be parsed; skipped");
                    }
                    for key in step.found {
                        self.read_scanned_creation(key, now);
                    }
                    if self.bot_creations.scan_retry.failures > 0 {
                        tracing::info!(target: "koru_ai::bots", "the Bot creation scan is moving again");
                    }
                    self.bot_creations.scan_retry = Retry::default();
                    self.bot_creations.scan = step.next;
                }
                Err(error) => {
                    if self.bot_creations.scan_retry.failed(now) {
                        tracing::warn!(target: "koru_ai::bots", ?error, "the Bot creation scan stopped; retrying with backoff");
                    }
                }
            }
        }
        let unreadable: Vec<_> = self
            .bot_creations
            .unreadable
            .iter()
            .filter(|(_, retry)| retry.ready(now))
            .map(|(key, _)| *key)
            .collect();
        for key in unreadable {
            self.read_scanned_creation(key, now);
        }
        if self.bot_creations.stepping {
            return;
        }
        let next = self
            .bot_creations
            .open
            .iter()
            .filter(|(_, tracked)| tracked.due && !tracked.busy && tracked.retry.ready(now))
            .min_by_key(|(_, tracked)| {
                (tracked.retry.failures, tracked.receipt.record.created_at_ms)
            })
            .map(|(key, _)| *key);
        if let Some(key) = next {
            self.step_bot_creation(key);
        }
    }

    /// Read one receipt the scan named. One that cannot be read is kept for
    /// a later try with backoff, reported once, never dropped.
    fn read_scanned_creation(&mut self, key: (UserScopeId, BotRequestId), now: Instant) {
        match self.repository.bot_creation(key.0, key.1) {
            Ok(Some(receipt)) => self.bot_creations.track(&receipt, true),
            Ok(None) => {
                self.bot_creations.unreadable.remove(&key);
            }
            Err(error) => {
                if self
                    .bot_creations
                    .unreadable
                    .entry(key)
                    .or_default()
                    .failed(now)
                {
                    tracing::warn!(target: "koru_ai::bots", request = %key.1, ?error, "an unfinished Bot creation could not be read; retrying with backoff");
                }
            }
        }
    }

    fn step_bot_creation(&mut self, key: (UserScopeId, BotRequestId)) {
        let receipt = match self.repository.bot_creation(key.0, key.1) {
            Ok(Some(receipt)) if receipt.open() => receipt,
            Ok(_) => {
                self.bot_creations.open.remove(&key);
                return;
            }
            // Kept, and tried again after a backoff: a read that failed says
            // nothing about whether the creation exists.
            Err(_) => {
                self.bot_creations
                    .stall(key, BotCreationReason::StorageFailed);
                return;
            }
        };
        if receipt.record.reason.is_some() {
            self.bot_creations.track(&receipt, false);
            return;
        }
        let record = &receipt.record;
        // A mark that a step may be about to happen is durable before it.
        let mark = |phase| {
            let mut next = record.clone();
            next.phase = phase;
            next
        };
        let (expected, step) = match record.phase {
            BotCreationPhase::Intent => {
                match self.advance_creation(
                    &receipt,
                    &mark(BotCreationPhase::FolderPending),
                    receipt.stage,
                ) {
                    // The mark was written just now: this job is the first,
                    // so nothing of ours can be at the path.
                    Some(marked) => (marked.record, CreationStep::Folder { first: true }),
                    None => return,
                }
            }
            // Any later folder job may follow our own mkdir: it adopts only
            // an empty folder exactly there.
            BotCreationPhase::FolderPending => {
                (record.clone(), CreationStep::Folder { first: false })
            }
            BotCreationPhase::FolderReady if record.authored() => {
                match self.advance_creation(
                    &receipt,
                    &mark(BotCreationPhase::RolePending),
                    receipt.stage,
                ) {
                    Some(marked) => (marked.record, CreationStep::Role),
                    None => return,
                }
            }
            BotCreationPhase::RolePending => (record.clone(), CreationStep::Role),
            BotCreationPhase::FolderReady | BotCreationPhase::RoleVerified => {
                (record.clone(), CreationStep::Authorize)
            }
        };
        let (Some(authority), Some(pool)) =
            (self.bot_authority.clone(), self.provider_admin.as_ref())
        else {
            return;
        };
        let job = ProviderAdminJob::BotCreationStep(CreationJob {
            key,
            expected,
            step,
            authority,
            folders: self.bot_creations.folders.clone(),
            reserved: self.bot_creations.reserved.clone(),
        });
        let submitted = pool.try_submit(job).is_ok();
        let Some(tracked) = self.bot_creations.open.get_mut(&key) else {
            return;
        };
        if submitted {
            tracked.busy = true;
            tracked.due = false;
            self.bot_creations.stepping = true;
        } else {
            // The lane is gone or full: wait out a backoff, never a pass.
            tracked.retry.failed(Instant::now());
        }
    }

    /// Move a creation on the writer, keeping the list exact. `None` when
    /// the receipt moved meanwhile (it is reread) or the write failed (the
    /// creation stalls and is retried from its durable phase with backoff).
    fn advance_creation(
        &mut self,
        receipt: &BotCreationReceipt,
        next: &BotCreationRecord,
        stage: BotReceiptStage,
    ) -> Option<BotCreationReceipt> {
        let key = (receipt.record.user_scope, receipt.request);
        match self.repository.advance_bot_creation(
            key.0,
            key.1,
            &receipt.record,
            next,
            stage,
            Timestamp::now().millis(),
        ) {
            Ok(moved) => {
                self.bot_creations.track(&moved, true);
                self.bot_creations.recorded(key);
                Some(moved)
            }
            Err(BotError::Conflict) => {
                match self.repository.bot_creation(key.0, key.1) {
                    Ok(Some(current)) => self.bot_creations.track(&current, true),
                    Ok(None) => {
                        self.bot_creations.open.remove(&key);
                    }
                    Err(_) => self
                        .bot_creations
                        .stall(key, BotCreationReason::StorageFailed),
                }
                None
            }
            Err(_) => {
                self.bot_creations
                    .stall(key, BotCreationReason::StorageFailed);
                None
            }
        }
    }

    fn park_creation(&mut self, receipt: &BotCreationReceipt, reason: BotCreationReason) {
        let mut parked = receipt.record.clone();
        parked.reason = Some(reason);
        if self
            .advance_creation(receipt, &parked, receipt.stage)
            .is_some()
        {
            tracing::info!(target: "koru_ai::bots", request = %receipt.request, reason = ?reason, "Bot creation incomplete");
        }
    }

    /// Apply one step's answer on the writer: T2, T3, T4, a park or a
    /// rejection. An answer for a receipt that has moved since is dropped.
    pub(in crate::worker) fn complete_bot_creation_step(&mut self, result: CreationStepResult) {
        let CreationStepResult {
            key,
            expected,
            step,
            outcome,
        } = result;
        self.bot_creations.stepping = false;
        if let Some(tracked) = self.bot_creations.open.get_mut(&key) {
            tracked.busy = false;
        }
        let receipt = match self.repository.bot_creation(key.0, key.1) {
            Ok(Some(receipt)) if receipt.open() && receipt.record == expected => receipt,
            Ok(Some(receipt)) => {
                self.bot_creations.track(&receipt, true);
                return;
            }
            Ok(None) => {
                self.bot_creations.open.remove(&key);
                return;
            }
            // The answer cannot be applied now: the step runs again from its
            // durable phase after a backoff, which is safe because every
            // effect it guards was marked durable before it ran.
            Err(_) => {
                self.bot_creations
                    .stall(key, BotCreationReason::StorageFailed);
                return;
            }
        };
        let record = receipt.record.clone();
        let first = matches!(step, CreationStep::Folder { first: true });
        match outcome {
            CreationOutcome::Owner => {
                self.park_creation(&receipt, BotCreationReason::OwnerUnavailable)
            }
            CreationOutcome::Panicked => self.park_creation(
                &receipt,
                match step {
                    CreationStep::Folder { .. } => BotCreationReason::FolderUnavailable,
                    CreationStep::Role => BotCreationReason::StoreUnavailable,
                    CreationStep::Authorize => BotCreationReason::OwnerUnavailable,
                },
            ),
            CreationOutcome::Folder(Ok(binding)) => {
                let mut next = record;
                next.phase = BotCreationPhase::FolderReady;
                next.folder_binding = Some(binding);
                self.advance_creation(&receipt, &next, BotReceiptStage::Prepared);
            }
            CreationOutcome::Folder(Err(error)) => {
                // Refused before anything of ours exists: the BotId is free.
                let reject = match error {
                    FolderFailure::Occupied if first => Some(BotCreationReason::FolderOccupied),
                    FolderFailure::Invalid | FolderFailure::Reserved
                        if matches!(record.folder, BotCreationFolder::Selected { .. }) =>
                    {
                        Some(BotCreationReason::FolderInvalid)
                    }
                    _ => None,
                };
                match reject {
                    Some(reason) => {
                        let mut next = record;
                        next.reason = Some(reason);
                        if self
                            .advance_creation(&receipt, &next, BotReceiptStage::Rejected)
                            .is_some()
                        {
                            tracing::info!(target: "koru_ai::bots", request = %key.1, reason = ?reason, "Bot creation rejected");
                        }
                    }
                    None => self.park_creation(
                        &receipt,
                        match error {
                            FolderFailure::Unavailable => BotCreationReason::FolderUnavailable,
                            _ => BotCreationReason::FolderMismatch,
                        },
                    ),
                }
            }
            CreationOutcome::Role(Ok(proof)) => {
                let seed = role_seed(&record);
                if !seed.as_ref().is_some_and(|seed| proof.matches(seed)) {
                    self.park_creation(&receipt, BotCreationReason::RoleConflict);
                } else if !proof.page_live {
                    self.park_creation(&receipt, BotCreationReason::RoleUnavailable);
                } else {
                    let mut next = record.clone();
                    next.phase = BotCreationPhase::RoleVerified;
                    next.role_page = Some(record.role_target());
                    self.advance_creation(&receipt, &next, BotReceiptStage::Accepted);
                }
            }
            CreationOutcome::Role(Err(error)) => self.park_creation(
                &receipt,
                match error {
                    BotError::Conflict => BotCreationReason::RoleConflict,
                    BotError::Forbidden | BotError::NotFound => BotCreationReason::OwnerUnavailable,
                    _ => BotCreationReason::StoreUnavailable,
                },
            ),
            CreationOutcome::Authorized(Err(reason)) => self.park_creation(&receipt, reason),
            CreationOutcome::Authorized(Ok(profile)) => {
                match self.repository.complete_bot_creation(
                    key.0,
                    key.1,
                    &record,
                    profile,
                    Timestamp::now().millis(),
                ) {
                    Ok((done, _)) => {
                        tracing::info!(target: "koru_ai::bots", request = %key.1, "Bot created");
                        self.bot_creations.track(&done, false);
                    }
                    Err(error) => {
                        tracing::warn!(target: "koru_ai::bots", request = %key.1, ?error, "the Bot creation transaction rolled back");
                        self.park_creation(&receipt, BotCreationReason::StorageFailed);
                    }
                }
            }
        }
    }

    /// A read's owner check came back: read the receipt under that owner,
    /// then check its home workspace before anything is disclosed.
    pub(in crate::worker) fn complete_bot_creation_read(&mut self, result: CreationReadResult) {
        let CreationReadResult { mut job, checked } = result;
        let answer = (|| -> ApiResult<Option<BotResponse>> {
            authorize(&job.context, AiScope::ManageBots)?;
            let checked = checked.map_err(bot_failure)?;
            if let Some((owner, creation)) = job.found.take() {
                if checked.owner != owner {
                    return Err(bot_failure(BotError::Forbidden));
                }
                checked
                    .workspace(creation.home_workspace)
                    .map_err(bot_failure)?;
                return Ok(Some(BotResponse::Creation { creation }));
            }
            let owner = checked.owner;
            let receipt = match self
                .repository
                .bot_creation(owner.user_scope, job.request_id)
            {
                Ok(Some(receipt)) if receipt.record.owner_device == owner.device => receipt,
                Ok(_) | Err(BotError::Conflict) => return Err(not_found()),
                Err(error) => return Err(bot_failure(error)),
            };
            job.found = Some((owner, self.creation_view(&receipt)));
            Ok(None)
        })();
        match answer {
            Ok(Some(response)) => {
                let _ = job.reply.send(Ok(response));
            }
            Ok(None) => self.submit_creation_job(ProviderAdminJob::BotCreationRead(job)),
            Err(error) => {
                let _ = job.reply.send(Err(error));
            }
        }
    }

    /// Commands answered on the writer from the ordinary preparation: the
    /// list. Creation and its read reach here only without an owner to ask.
    pub(in crate::worker) fn bot_creation_command(
        &self,
        context: &CommandContext,
        command: &BotCommand,
    ) -> Option<ApiResult<BotResponse>> {
        match command {
            BotCommand::ListBotCreations {
                workspace,
                cursor,
                limit,
            } => Some(self.list_bot_creations(context, *workspace, cursor.as_ref(), *limit)),
            BotCommand::CreateBotWithRole { .. } | BotCommand::ReadBotCreation { .. } => Some(
                authorize(context, AiScope::ManageBots)
                    .and(Err(bot_failure(BotError::Unavailable))),
            ),
            _ => None,
        }
    }

    /// The owner's unfinished creations in a workspace it may reach now,
    /// oldest first, from the writer's list. Until the startup scan has read
    /// every unfinished receipt the list is incomplete, so it answers
    /// retryable unavailable rather than a short list.
    fn list_bot_creations(
        &self,
        context: &CommandContext,
        workspace: WorkspaceId,
        cursor: Option<&BotCreationCursor>,
        limit: u16,
    ) -> ApiResult<BotResponse> {
        authorize(context, AiScope::ManageBots)?;
        let proof = context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        let owner = proof.authority.owner().map_err(bot_failure)?;
        proof.authority.workspace(workspace).map_err(bot_failure)?;
        if limit == 0 || limit > BOT_CREATION_PAGE_LIMIT {
            return Err(bot_failure(BotError::Invalid));
        }
        if self.bot_creations.scan.is_some() {
            return Err(unavailable_failure());
        }
        let mut open: Vec<&BotCreationReceipt> = self
            .bot_creations
            .open
            .values()
            .map(|tracked| &tracked.receipt)
            .filter(|receipt| {
                receipt.record.user_scope == owner.user_scope
                    && receipt.record.owner_device == owner.device
                    && receipt.record.home_workspace == workspace
            })
            .filter(|receipt| {
                cursor.is_none_or(|c| {
                    (receipt.record.created_at_ms, receipt.request)
                        > (c.created_at_ms, c.request_id)
                })
            })
            .collect();
        open.sort_by_key(|receipt| (receipt.record.created_at_ms, receipt.request));
        let more = open.len() > limit as usize;
        open.truncate(limit as usize);
        let next_cursor = more
            .then(|| open.last())
            .flatten()
            .map(|last| BotCreationCursor {
                created_at_ms: last.record.created_at_ms,
                request_id: last.request,
            });
        Ok(BotResponse::Creations {
            page: BotCreationPage {
                creations: open
                    .into_iter()
                    .map(|receipt| {
                        self.bot_creations.shown(
                            &(receipt.record.user_scope, receipt.request),
                            receipt.view(None),
                        )
                    })
                    .collect(),
                next_cursor,
            },
        })
    }
}

fn refuse_creation_job(job: ProviderAdminJob, error: Failure) {
    match job {
        ProviderAdminJob::BotCreationPrepare(job) => {
            let _ = job.reply.send(Err(error));
        }
        ProviderAdminJob::BotCreationRead(job) => {
            let _ = job.reply.send(Err(error));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
