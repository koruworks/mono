// SPDX-License-Identifier: MPL-2.0
//! Four purpose-specific phases. Receipts never borrow fresh effect authority.
use super::super::*;
use koru_delegate_api::bots::{BotCommand as DelegateCommand, BotDelegatePort, BotReply};
use koru_delegate_core::{
    AttemptGeneration, BotPolicyBarrier, BotProvenance, CapturedBotPolicy, DocumentVersionId,
    MemoryBaseResolution, MemoryBaseSelector, MemoryEditAction, MemoryEditAllocation,
    MemoryEditAuthorityV1, MemoryEditSubjectV1, RunToolAuthority,
};
use koru_mcp_server::native::memory::{
    MemoryEditRecipe, MemoryRecipeOperation, validate_memory_text,
};
mod contract;
pub use contract::*;
#[cfg(test)]
mod tests;

pub(in crate::worker) struct MemoryWrites {
    serial: u64,
    completed: u64,
    epoch: Arc<()>,
    owner: Option<BotOwner>,
}
impl Default for MemoryWrites {
    fn default() -> Self {
        Self {
            serial: 0,
            completed: 0,
            epoch: Arc::new(()),
            owner: None,
        }
    }
}
impl MemoryWrites {
    pub(in crate::worker) fn invalidate(&mut self) {
        self.epoch = Arc::new(());
        self.owner = None;
    }
    fn observe(&mut self, owner: BotOwner) {
        if self.owner != Some(owner) {
            self.invalidate();
            self.owner = Some(owner);
        }
    }
}

pub(in crate::worker) enum Work {
    Subject,
    Base {
        selection: MemoryBaseSelection,
        operation: MemoryRecipeOperation,
    },
    Ensure {
        allocation: MemoryEditAllocation,
    },
    Authority {
        allocation: MemoryEditAllocation,
    },
}
impl Work {
    fn fresh(&self) -> bool {
        !matches!(self, Self::Subject)
    }
    fn allocation(&self) -> Option<&MemoryEditAllocation> {
        match self {
            Self::Base {
                selection: MemoryBaseSelection::Allocated { allocation },
                ..
            }
            | Self::Ensure { allocation }
            | Self::Authority { allocation } => Some(allocation),
            _ => None,
        }
    }
}
pub(in crate::worker) enum WriteValue {
    Subject(MemoryWriteSubjectEvidence),
    Base(MemoryBaseEvidence),
    Ensure(RunMemoryEnsureResult),
    Authority(MemoryWriteAuthorityEvidence),
}
#[derive(Clone)]
struct Snapshot {
    bot: Bot,
    capture: BotLibraryCapture,
    binding: Option<BotMemoryBinding>,
    creation: Option<BotMemoryCreation>,
    barrier: Option<BotPolicyBarrier>,
}
impl Snapshot {
    fn owner(&self) -> BotOwner {
        BotOwner {
            user_scope: self.bot.user_scope,
            device: self.bot.owner_device,
        }
    }
    fn authority(
        &self,
        request: &RunMemoryWriteSubjectRequest,
    ) -> ApiResult<MemoryEditAuthorityV1> {
        let binding = self
            .binding
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::NotFound))?;
        Ok(MemoryEditAuthorityV1 {
            version: 1,
            owner: request.provenance.owner,
            bot: self.bot.id,
            submission: request.provenance.submission,
            captured_config_revision: request.provenance.config_revision,
            creation_request: binding.creation_request,
            workspace: binding.page.workspace,
            memory_document: binding.page.document,
            action: request.action,
        })
    }
}
pub(in crate::worker) struct MemoryWriteJob {
    serial: u64,
    epoch: Arc<()>,
    snapshot: Snapshot,
    request: RunMemoryWriteSubjectRequest,
    work: Work,
    authority: Arc<dyn BotAuthority>,
    delegate: Option<Arc<dyn BotDelegatePort>>,
    deadline: Instant,
    reply: SyncSender<ApiResult<WriteValue>>,
}
pub(in crate::worker) struct MemoryWriteResult {
    job: MemoryWriteJob,
    result: ApiResult<Prepared>,
}
enum Prepared {
    Value(WriteValue),
    Ensure,
}
fn closed() -> Failure {
    failure(
        "unsupported_capability",
        "Bot Memory writes are not available yet.",
        false,
    )
}
fn memory_failure(error: MemoryAuthorityError) -> Failure {
    failure(
        error.code(),
        "The Memory write could not be prepared.",
        matches!(
            error,
            MemoryAuthorityError::Authority(BotError::Unavailable | BotError::Storage)
        ),
    )
}

impl MemoryWriteJob {
    fn live(&self) -> ApiResult<()> {
        if self.request.cancellation.is_cancelled() || Instant::now() >= self.deadline {
            Err(unavailable_failure())
        } else {
            Ok(())
        }
    }
    fn generation(&self) -> ApiResult<()> {
        self.live()?;
        if !self.work.fresh() {
            return Ok(());
        }
        let delegate = self.delegate.as_ref().ok_or_else(unavailable_failure)?;
        match delegate
            .execute_within(
                DelegateCommand::ResolveRunAuthority {
                    run: self.request.run,
                    generation: self.request.generation,
                },
                Duration::from_secs(5),
            )
            .map_err(|_| unavailable_failure())?
        {
            BotReply::RunAuthority {
                authority: RunToolAuthority::Bot { provenance, policy },
            } if provenance == self.request.provenance && policy == self.request.captured => {}
            _ => return Err(bot_failure(BotError::Forbidden)),
        }
        if let Some(allocation) = self.work.allocation() {
            let key = koru_delegate_core::BotSubmissionKey {
                owner: self.request.provenance.owner,
                bot: self.request.provenance.bot,
                request: self.request.provenance.submission,
            };
            let fence = match delegate
                .execute_within(
                    DelegateCommand::Reconcile { key: key.clone() },
                    Duration::from_secs(5),
                )
                .map_err(|_| unavailable_failure())?
            {
                BotReply::Admission { admission }
                    if admission.key == key && admission.run == self.request.run =>
                {
                    admission
                        .fence
                        .ok_or_else(|| bot_failure(BotError::Forbidden))?
                }
                _ => return Err(bot_failure(BotError::Forbidden)),
            };
            if fence.owner != self.request.provenance.owner
                || fence.bot != self.request.provenance.bot
                || fence.run != allocation.key.run
                || fence.attempt != allocation.key.attempt
                || fence.generation != allocation.key.generation
                || fence.turn != Some(allocation.key.turn)
            {
                return Err(bot_failure(BotError::Forbidden));
            }
            match delegate
                .execute_within(
                    DelegateCommand::EffectFence {
                        fence: fence.clone(),
                    },
                    Duration::from_secs(5),
                )
                .map_err(|_| unavailable_failure())?
            {
                BotReply::EffectFence {
                    fence: found,
                    policy,
                } if found == fence && policy == self.request.captured => {}
                _ => return Err(bot_failure(BotError::Forbidden)),
            }
        }
        self.live()
    }
    fn profile(&self) -> ApiResult<koru_ai_core::ProfileId> {
        self.live()?;
        let pages: Vec<_> = if self.work.fresh() {
            self.snapshot
                .binding
                .as_ref()
                .map(|b| b.page.clone())
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        let proof = self
            .authority
            .inspect(&[self.snapshot.bot.home_workspace], &pages)
            .map_err(bot_failure)?;
        if proof.owner != self.snapshot.owner() {
            return Err(bot_failure(BotError::Forbidden));
        }
        for page in pages {
            proof.page(&page).map_err(bot_failure)?;
        }
        let profile = proof
            .workspace(self.snapshot.bot.home_workspace)
            .map_err(bot_failure)?;
        self.live()?;
        Ok(profile)
    }
    fn allocation(&self, allocation: &MemoryEditAllocation) -> ApiResult<()> {
        if !allocation.valid()
            || allocation.subject != self.request.subject()
            || allocation.key.run != self.request.run
            || allocation.key.generation != self.request.generation
        {
            return Err(bot_failure(BotError::Forbidden));
        }
        let delegate = self.delegate.as_ref().ok_or_else(unavailable_failure)?;
        match delegate
            .execute_within(
                DelegateCommand::ReadMemoryEditAllocation {
                    run: allocation.key.run,
                    operation_id: allocation.key.operation_id,
                    caller_fingerprint: allocation.caller_fingerprint,
                },
                Duration::from_secs(5),
            )
            .map_err(|_| unavailable_failure())?
        {
            BotReply::MemoryEditAllocation { allocation: found } if found == *allocation => {}
            _ => return Err(bot_failure(BotError::Conflict)),
        }
        self.live()
    }
    pub(in crate::worker) fn run(self) -> MemoryWriteResult {
        let result = catch_unwind(AssertUnwindSafe(|| self.prepare()))
            .unwrap_or_else(|_| Err(unavailable_failure()));
        MemoryWriteResult { job: self, result }
    }
    fn prepare(&self) -> ApiResult<Prepared> {
        self.live()?;
        self.generation()?;
        let profile = self.profile()?;
        if let Some(allocation) = self.work.allocation() {
            self.allocation(allocation)?;
        }
        let value = match &self.work {
            Work::Subject => Prepared::Value(WriteValue::Subject(MemoryWriteSubjectEvidence {
                subject: self.request.subject(),
                profile,
                config_revision: self.snapshot.bot.revision,
                policy_revision: self.snapshot.bot.policy_revision,
                new_work_barrier: self.snapshot.barrier.clone(),
            })),
            Work::Base {
                selection,
                operation,
            } => Prepared::Value(WriteValue::Base(self.base(profile, selection, operation)?)),
            Work::Ensure { allocation } => {
                if self.request.action != MemoryEditAction::Remember
                    || !matches!(allocation.base, MemoryBaseSelector::OriginalSeed {})
                    || allocation.ensure.is_none()
                {
                    return Err(bot_failure(BotError::Invalid));
                }
                Prepared::Ensure
            }
            Work::Authority { allocation } => {
                let expected = self.snapshot.authority(&self.request)?;
                if !matches!(&allocation.resolution, MemoryBaseResolution::Resolved { authority, .. } if *authority == expected)
                {
                    return Err(bot_failure(BotError::Conflict));
                }
                Prepared::Value(WriteValue::Authority(MemoryWriteAuthorityEvidence {
                    authority: expected,
                    profile,
                    barrier: self
                        .snapshot
                        .barrier
                        .clone()
                        .ok_or_else(|| bot_failure(BotError::Forbidden))?,
                }))
            }
        };
        self.generation()?;
        if self.profile()? != profile {
            return Err(bot_failure(BotError::Forbidden));
        }
        self.live()?;
        Ok(value)
    }
    fn base(
        &self,
        profile: koru_ai_core::ProfileId,
        selection: &MemoryBaseSelection,
        operation: &MemoryRecipeOperation,
    ) -> ApiResult<MemoryBaseEvidence> {
        contract::validate_operation(self.request.action, operation)?;
        let binding = self
            .snapshot
            .binding
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::NotFound))?;
        let selector = match selection {
            MemoryBaseSelection::UnallocatedHistory { version } => {
                MemoryBaseSelector::History { version: *version }
            }
            MemoryBaseSelection::Allocated { allocation } => {
                let fingerprint = memory_write_fingerprint(
                    &allocation.subject,
                    allocation.key.run,
                    allocation.key.operation_id,
                    allocation.base,
                    operation,
                )?;
                if fingerprint != allocation.caller_fingerprint {
                    return Err(bot_failure(BotError::Conflict));
                }
                match operation {
                    MemoryRecipeOperation::Correct { fact, .. }
                    | MemoryRecipeOperation::Forget { fact }
                        if *fact != allocation.fact_id =>
                    {
                        return Err(bot_failure(BotError::Conflict));
                    }
                    _ => {}
                }
                allocation.base
            }
        };
        let evidence = match selector {
            MemoryBaseSelector::History { version } => {
                if version.as_uuid().is_nil() {
                    return Err(bot_failure(BotError::Invalid));
                }
                self.authority
                    .memory_edit_history(
                        self.snapshot.owner(),
                        profile,
                        binding,
                        version,
                        &self.request.cancellation,
                    )
                    .map_err(memory_failure)?
            }
            MemoryBaseSelector::OriginalSeed {} => {
                let creation = self
                    .snapshot
                    .creation
                    .as_ref()
                    .ok_or_else(|| bot_failure(BotError::NotFound))?;
                if creation.state != BotMemoryState::Bound {
                    return Err(bot_failure(BotError::Settling));
                }
                self.authority
                    .memory_original_seed_history(
                        self.snapshot.owner(),
                        profile,
                        &creation.identity,
                        binding,
                        &self.request.cancellation,
                    )
                    .map_err(memory_failure)?
            }
        };
        self.live()?;
        let version = match selector {
            MemoryBaseSelector::History { version } => version,
            MemoryBaseSelector::OriginalSeed {} => {
                DocumentVersionId::from_uuid(evidence.id.as_uuid())
            }
        };
        let (mut expected_revision, recipe) =
            crate::bots::memory_edit_recipe(binding.page.clone(), version, evidence, operation)
                .map_err(memory_failure)?;
        let authority = self.snapshot.authority(&self.request)?;
        if let MemoryBaseSelection::Allocated { allocation } = selection {
            if let MemoryBaseResolution::Resolved {
                authority: saved,
                version: saved_version,
                expected_revision: saved_revision,
            } = &allocation.resolution
            {
                if *saved != authority
                    || *saved_version != version
                    || !koru_delegate_core::same_native_edit_revision(
                        saved_revision,
                        &expected_revision,
                    )
                    .map_err(|_| bot_failure(BotError::Invalid))?
                {
                    return Err(bot_failure(BotError::Conflict));
                }
                expected_revision = saved_revision.clone();
            }
        }
        Ok(MemoryBaseEvidence {
            authority,
            version,
            expected_revision,
            recipe,
        })
    }
}

impl WorkerState {
    fn memory_write_snapshot(
        &self,
        request: &RunMemoryWriteSubjectRequest,
        work: &Work,
    ) -> ApiResult<Snapshot> {
        let fresh = work.fresh();
        let p = &request.provenance;
        if !request.subject().valid() || !request.captured.valid() {
            return Err(bot_failure(BotError::Invalid));
        }
        let owner = BotOwner {
            user_scope: p.owner.user_scope,
            device: p.owner.device,
        };
        let bot = if fresh {
            self.repository.bot(p.bot)
        } else {
            lifecycle::readable_bot(&self.repository, p.bot)
        }
        .map_err(bot_failure)?
        .ok_or_else(not_found)?;
        if bot.user_scope != owner.user_scope || bot.owner_device != owner.device {
            return Err(bot_failure(BotError::Forbidden));
        }
        let receipt = self
            .repository
            .bot_readable_submission_for_run(owner, p.bot, request.run)
            .map_err(bot_failure)?
            .ok_or_else(not_found)?;
        if receipt.intent.submission.provenance != *p
            || receipt.intent.submission.policy != request.captured
            || receipt.intent.submission.workspace != bot.home_workspace
        {
            return Err(bot_failure(BotError::Forbidden));
        }
        if fresh {
            if let Some(id) = p.ai_attempt_id {
                let id = InferenceAttemptId::parse(&id.to_string())
                    .ok_or_else(|| bot_failure(BotError::Invalid))?;
                let attempt = self
                    .repository
                    .attempt(id)
                    .map_err(ai_failure)?
                    .ok_or_else(not_found)?;
                if attempt.status.is_terminal()
                    || attempt.chat_id.map(|id| id.as_uuid()) != Some(p.chat_id)
                    || attempt.assistant_message_id.map(|id| id.as_uuid()) != p.assistant_message_id
                    || self
                        .repository
                        .bot_replay_state(id)
                        .map_err(ai_failure)?
                        .stop_reason
                        .is_some()
                    || self
                        .repository
                        .bot_submission_for_attempt(id)
                        .map_err(ai_failure)?
                        .is_none_or(|current| current.intent != receipt.intent)
                {
                    return Err(bot_failure(BotError::Forbidden));
                }
            }
        }
        let capture = self
            .repository
            .bot_library_capture(owner, p.bot, p.submission)
            .map_err(bot_failure)?
            .ok_or_else(|| memory_failure(MemoryAuthorityError::CaptureUnavailable))?;
        capture.validate(bot.home_workspace).map_err(bot_failure)?;
        if capture.owner != owner
            || capture.bot != p.bot
            || capture.submission != p.submission
            || capture.config_revision != p.config_revision
            || matches!(capture.memory, BotCapturedMemory::Absent {})
                && capture.index_digest != *blake3::hash(&[]).as_bytes()
        {
            return Err(memory_failure(MemoryAuthorityError::CaptureUnavailable));
        }
        let barrier = if bot.lifecycle == BotLifecycle::Active
            && !bot.paused
            && self
                .repository
                .bot_memory_mutation_ready(owner, bot.id)
                .map_err(bot_failure)?
        {
            Some(policy::barrier(&bot)?)
        } else {
            None
        };
        if fresh && barrier.is_none() {
            return Err(bot_failure(BotError::Settling));
        }
        let (binding, creation) = if fresh {
            let library = self
                .repository
                .bot_library(owner, bot.id)
                .map_err(bot_failure)?;
            if let BotCapturedMemory::Captured {
                page,
                creation_request,
                ..
            } = &capture.memory
            {
                if library.binding.as_ref().is_none_or(|binding| {
                    binding.page != *page || binding.creation_request != *creation_request
                }) {
                    return Err(memory_failure(MemoryAuthorityError::CaptureUnavailable));
                }
            }
            (library.binding, library.creation)
        } else {
            (None, None)
        };
        if !matches!(work, Work::Ensure { .. }) {
            if let Some(allocation) = work.allocation() {
                if matches!(allocation.base, MemoryBaseSelector::OriginalSeed {}) {
                    let ensure = allocation
                        .ensure
                        .as_ref()
                        .ok_or_else(|| bot_failure(BotError::Invalid))?;
                    let (seed, fingerprint) = super::commands::ensure_body(
                        &bot,
                        ensure.request_id,
                        ensure.expected_revision,
                        &ensure.title,
                    )?;
                    let proven = self
                        .repository
                        .bot_memory_creation_receipt(
                            owner,
                            bot.id,
                            ensure.request_id,
                            ensure.expected_revision,
                            &seed,
                            &fingerprint,
                        )
                        .map_err(bot_failure)?
                        .ok_or_else(|| bot_failure(BotError::Settling))?;
                    if proven.state != BotMemoryState::Bound
                        || creation.as_ref() != Some(&proven)
                        || binding.as_ref().is_none_or(|binding| {
                            binding.creation_request != proven.identity.creation_request
                                || binding.page.document != proven.identity.candidate
                                || binding.page.workspace != proven.identity.workspace
                        })
                    {
                        return Err(bot_failure(BotError::Conflict));
                    }
                }
            }
        }
        Ok(Snapshot {
            bot,
            capture,
            binding,
            creation,
            barrier,
        })
    }
    pub(in crate::worker) fn request_memory_write(
        &mut self,
        request: RunMemoryWriteSubjectRequest,
        work: Work,
        reply: SyncSender<ApiResult<WriteValue>>,
        deadline: Instant,
    ) {
        if !BOT_MEMORY_WRITES_AVAILABLE {
            let _ = reply.send(Err(closed()));
            return;
        }
        self.enqueue_memory_write(request, work, reply, deadline);
    }
    pub(in crate::worker) fn enqueue_memory_write(
        &mut self,
        request: RunMemoryWriteSubjectRequest,
        work: Work,
        reply: SyncSender<ApiResult<WriteValue>>,
        deadline: Instant,
    ) {
        let prepared = (|| {
            if request.cancellation.is_cancelled() || Instant::now() >= deadline {
                return Err(unavailable_failure());
            }
            if let Work::Base { operation, .. } = &work {
                contract::validate_operation(request.action, operation)?;
            }
            let snapshot = self.memory_write_snapshot(&request, &work)?;
            let authority = self.bot_authority.clone().ok_or_else(unavailable_failure)?;
            self.memory_writes.observe(snapshot.owner());
            let serial = self
                .memory_writes
                .serial
                .checked_add(1)
                .ok_or_else(unavailable_failure)?;
            Ok(MemoryWriteJob {
                serial,
                epoch: self.memory_writes.epoch.clone(),
                snapshot,
                request,
                work,
                authority,
                delegate: self.bot_delegate.clone(),
                deadline,
                reply: reply.clone(),
            })
        })();
        match prepared {
            Err(error) => {
                let _ = reply.send(Err(error));
            }
            Ok(job) => {
                let serial = job.serial;
                let Some(pool) = self.provider_admin.as_ref() else {
                    let _ = reply.send(Err(unavailable_failure()));
                    return;
                };
                match pool.try_submit(ProviderAdminJob::BotMemoryWrite(job)) {
                    Ok(()) => self.memory_writes.serial = serial,
                    Err(AdminSubmitError {
                        job: ProviderAdminJob::BotMemoryWrite(job),
                        failure,
                    }) => {
                        let _ = job.reply.send(Err(failure));
                    }
                    Err(_) => unreachable!("Memory write submission returned another job"),
                }
            }
        }
    }
    pub(in crate::worker) fn complete_memory_write(&mut self, completed: MemoryWriteResult) {
        let MemoryWriteResult { job, result } = completed;
        let checked = (|| {
            job.live()?;
            if job.serial <= self.memory_writes.completed
                || job.serial > self.memory_writes.serial
                || !Arc::ptr_eq(&job.epoch, &self.memory_writes.epoch)
                || self.memory_writes.owner != Some(job.snapshot.owner())
            {
                return Err(unavailable_failure());
            }
            let current = self.memory_write_snapshot(&job.request, &job.work)?;
            if current.bot.revision != job.snapshot.bot.revision
                || current.bot.policy_revision != job.snapshot.bot.policy_revision
                || current.bot.lifecycle != job.snapshot.bot.lifecycle
                || current.bot.home_workspace != job.snapshot.bot.home_workspace
                || current.capture != job.snapshot.capture
                || current.binding != job.snapshot.binding
                || current.creation != job.snapshot.creation
                || current.barrier != job.snapshot.barrier
            {
                return Err(bot_failure(BotError::Conflict));
            }
            match result? {
                Prepared::Value(value) => Ok(value),
                Prepared::Ensure => {
                    let Work::Ensure { allocation } = &job.work else {
                        return Err(bot_failure(BotError::Invalid));
                    };
                    let ensure = allocation
                        .ensure
                        .as_ref()
                        .ok_or_else(|| bot_failure(BotError::Invalid))?;
                    job.live()?;
                    let creation = self.prepare_memory_creation_intent(
                        current.owner(),
                        &current.bot,
                        ensure.request_id,
                        ensure.expected_revision,
                        &ensure.title,
                    )?;
                    let binding = self
                        .repository
                        .bot_library(current.owner(), current.bot.id)
                        .map_err(bot_failure)?
                        .binding;
                    Ok(WriteValue::Ensure(RunMemoryEnsureResult {
                        creation_request: creation.identity.creation_request,
                        state: creation.state,
                        observed_updated_at_ms: creation.updated_at_ms,
                        binding,
                    }))
                }
            }
        })();
        self.memory_writes.completed = self.memory_writes.completed.max(job.serial);
        let _ = job.reply.send(checked);
    }
}
