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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::worker) enum FolderFailure {
    /// Something is at the managed folder on first acceptance.
    Occupied,
    /// A level resolves somewhere other than where it was made.
    Mismatch,
    /// The selected folder does not exist or is not a folder.
    Invalid,
    /// The filesystem did not answer.
    Unavailable,
}

/// A canonical path as it is stored and shown: without Windows' verbatim
/// prefix, which programs launched in the folder should not see.
fn display(path: &Path) -> Result<String, FolderFailure> {
    let text = path.to_str().ok_or(FolderFailure::Unavailable)?;
    Ok(if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        text.strip_prefix(r"\\?\").unwrap_or(text).to_owned()
    })
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
/// level under the canonical root. On `first` acceptance anything already
/// there is a conflict and nothing is made; a recovery adopts the exact
/// canonical folder the intent names, which grants no ownership of its
/// contents. A link anywhere on the way that resolves elsewhere refuses.
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
    match fs.create_dir(&target) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if first {
                return Err(FolderFailure::Occupied);
            }
        }
        Err(error) => return Err(io(error)),
    }
    if fs.canonical(&target).map_err(io)? != target || !fs.is_dir(&target).map_err(io)? {
        return Err(FolderFailure::Mismatch);
    }
    display(&target)
}

/// A selected folder: it exists and is a folder. Validated, never made.
/// Its binding is its canonical form.
pub(in crate::worker) fn check_selected(
    fs: &dyn BotFolderSystem,
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
    display(&canonical)
}

fn folder_failure(error: FolderFailure) -> Failure {
    match error {
        FolderFailure::Invalid => failure(
            "invalid_request",
            "The selected folder is not an existing folder.",
            false,
        ),
        _ => failure("folder_unavailable", "The folder could not be read.", true),
    }
}

/// "Change folder before the first task": the new folder must exist and be
/// a folder. Off the writer, in the command's preparation.
pub(in crate::worker) fn require_existing_folder(
    fs: &dyn BotFolderSystem,
    path: &str,
) -> ApiResult<()> {
    check_selected(fs, path).map(|_| ()).map_err(folder_failure)
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
    /// Off only in fixtures that have no runtime to ask.
    discover_models: bool,
    open: BTreeMap<(UserScopeId, BotRequestId), Tracked>,
    /// The startup scan's next BotId bound; `None` once it has finished.
    scan: Option<String>,
}

struct Tracked {
    /// The receipt without its seed: what lists and reads show.
    receipt: BotCreationReceipt,
    busy: bool,
    due: bool,
    /// `folder_pending` was marked in this process and its first folder job
    /// has not run: anything at the managed path is then not ours.
    first: bool,
}

impl CreationDriver {
    pub(in crate::worker) fn new(root: PathBuf, folders: Arc<dyn BotFolderSystem>) -> Self {
        Self {
            root,
            folders,
            discover_models: true,
            open: BTreeMap::new(),
            scan: Some(String::new()),
        }
    }
    #[cfg(test)]
    pub(in crate::worker) fn without_model_discovery(mut self) -> Self {
        self.discover_models = false;
        self
    }
    pub(in crate::worker) fn folders(&self) -> Arc<dyn BotFolderSystem> {
        self.folders.clone()
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
        let entry = self.open.entry(key).or_insert(Tracked {
            receipt: shown.clone(),
            busy: false,
            due: false,
            first: false,
        });
        entry.receipt = shown;
        entry.due |= due && receipt.record.reason.is_none();
    }
}

/// Preparation of one new creation, off the writer.
pub(in crate::worker) struct CreationPreparationJob {
    context: CommandContext,
    request: BotCommand,
    reply: SyncSender<ApiResult<BotResponse>>,
    authority: Arc<dyn BotAuthority>,
    folders: Arc<dyn BotFolderSystem>,
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
                BotFolderChoice::Managed {} => BotCreationFolder::Managed {
                    root: self
                        .root
                        .to_str()
                        .ok_or_else(|| bot_failure(BotError::Unavailable))?
                        .to_owned(),
                    path: managed_folder(&self.root, *bot_id)
                        .to_str()
                        .ok_or_else(|| bot_failure(BotError::Unavailable))?
                        .to_owned(),
                },
                BotFolderChoice::Selected { path } => {
                    check_selected(self.folders.as_ref(), path).map_err(folder_failure)?;
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
            // Oversize input refuses here, before any effect.
            record.encode().map_err(|_| {
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
                BotCreationFolder::Selected { path } => check_selected(self.folders.as_ref(), path),
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
            if existing.fingerprint != fingerprint || existing.record.bot != *bot_id {
                return Err(bot_failure(BotError::Conflict));
            }
            if existing.record.owner_device != owner.device {
                return Err(bot_failure(BotError::Forbidden));
            }
            snapshot
                .workspace(existing.record.home_workspace)
                .map_err(bot_failure)?;
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
        receipt.view(bot)
    }

    /// The startup scan, one bounded step per call, then each due
    /// creation's next step. Called from the writer loop; cheap when idle.
    pub(in crate::worker) fn schedule_bot_creations(&mut self) {
        if self.bot_authority.is_none() || self.provider_admin.is_none() {
            return;
        }
        if let Some(after) = self.bot_creations.scan.clone() {
            match self.repository.bot_creation_scan(&after, SCAN_STEP) {
                Ok((found, next)) => {
                    for (scope, request) in found {
                        match self.repository.bot_creation(scope, request) {
                            Ok(Some(receipt)) => self.bot_creations.track(&receipt, true),
                            Ok(None) => {}
                            Err(error) => {
                                tracing::warn!(target: "koru_ai::bots", %request, ?error, "an unfinished Bot creation could not be read");
                            }
                        }
                    }
                    self.bot_creations.scan = next;
                }
                Err(error) => {
                    tracing::warn!(target: "koru_ai::bots", ?error, "the Bot creation scan stopped; retrying");
                }
            }
        }
        let due: Vec<_> = self
            .bot_creations
            .open
            .iter()
            .filter(|(_, tracked)| tracked.due && !tracked.busy)
            .map(|(key, _)| *key)
            .collect();
        for key in due {
            self.step_bot_creation(key);
        }
    }

    fn step_bot_creation(&mut self, key: (UserScopeId, BotRequestId)) {
        let receipt = match self.repository.bot_creation(key.0, key.1) {
            Ok(Some(receipt)) if receipt.open() => receipt,
            Ok(_) => {
                self.bot_creations.open.remove(&key);
                return;
            }
            Err(error) => {
                tracing::warn!(target: "koru_ai::bots", request = %key.1, ?error, "a Bot creation could not be read");
                self.bot_creations.open.remove(&key);
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
                    Some(marked) => {
                        if let Some(tracked) = self.bot_creations.open.get_mut(&key) {
                            tracked.first = true;
                        }
                        (marked.record, CreationStep::Folder { first: true })
                    }
                    None => return,
                }
            }
            BotCreationPhase::FolderPending => {
                let first = self.bot_creations.open.get(&key).is_some_and(|t| t.first);
                (record.clone(), CreationStep::Folder { first })
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
        let Some(authority) = self.bot_authority.clone() else {
            return;
        };
        let job = ProviderAdminJob::BotCreationStep(CreationJob {
            key,
            expected,
            step,
            authority,
            folders: self.bot_creations.folders.clone(),
        });
        let Some(pool) = self.provider_admin.as_ref() else {
            return;
        };
        // A full pool leaves the step due; a freed slot runs it next pass.
        if pool.try_submit(job).is_ok()
            && let Some(tracked) = self.bot_creations.open.get_mut(&key)
        {
            tracked.busy = true;
            tracked.due = false;
        }
    }

    /// Move a creation on the writer, keeping the list exact. `None` (and
    /// a log line) when the receipt moved or the write failed.
    fn advance_creation(
        &mut self,
        receipt: &BotCreationReceipt,
        next: &BotCreationRecord,
        stage: BotReceiptStage,
    ) -> Option<BotCreationReceipt> {
        match self.repository.advance_bot_creation(
            receipt.record.user_scope,
            receipt.request,
            &receipt.record,
            next,
            stage,
            Timestamp::now().millis(),
        ) {
            Ok(moved) => {
                self.bot_creations.track(&moved, true);
                Some(moved)
            }
            Err(error) => {
                tracing::warn!(target: "koru_ai::bots", request = %receipt.request, ?error, "a Bot creation step could not be recorded");
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
        if let Some(tracked) = self.bot_creations.open.get_mut(&key) {
            tracked.busy = false;
            if matches!(step, CreationStep::Folder { .. }) {
                tracked.first = false;
            }
        }
        let receipt = match self.repository.bot_creation(key.0, key.1) {
            Ok(Some(receipt)) if receipt.open() && receipt.record == expected => receipt,
            Ok(Some(receipt)) => {
                self.bot_creations.track(&receipt, true);
                return;
            }
            _ => return,
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
                    FolderFailure::Invalid
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
                creations: open.into_iter().map(|receipt| receipt.view(None)).collect(),
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
