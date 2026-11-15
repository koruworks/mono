// SPDX-License-Identifier: MPL-2.0
//! Scoped Memory reads on the existing person-preparation lane.
use super::super::*;
use super::{RunMemoryReadRequest, RunMemoryReadResult, cursor};
use koru_delegate_api::bots::{BotCommand as DelegateCommand, BotDelegatePort, BotReply};
// Named by the tests only since the read accepts either Bot classification.
#[cfg(test)]
use koru_delegate_core::RunToolAuthority;
#[cfg(test)]
mod tests;

#[derive(Default)]
pub(in crate::worker) struct MemoryReads {
    serial: u64,
    /// Every accepted read runs synchronously on the single FIFO preparation
    /// thread, which sends its completion before receiving the next job. A
    /// high-water serial therefore needs no pending-request map. Shutdown drops
    /// this entire state; rejected enqueues never consume a serial.
    completed: u64,
    key: Option<(BotOwner, Arc<cursor::CursorKey>)>,
}
impl MemoryReads {
    pub(in crate::worker) fn invalidate(&mut self) {
        self.key = None;
    }
    fn key(&mut self, owner: BotOwner) -> Arc<cursor::CursorKey> {
        if self
            .key
            .as_ref()
            .is_none_or(|(current, _)| *current != owner)
        {
            self.key = Some((owner, Arc::new(cursor::CursorKey::new(owner))));
        }
        self.key.as_ref().expect("installed key").1.clone()
    }
}

enum Subject {
    Owner,
    Run {
        request: RunMemoryReadRequest,
        capture: BotLibraryCapture,
    },
}
enum Reply {
    Owner(SyncSender<ApiResult<BotResponse>>),
    Run(SyncSender<ApiResult<RunMemoryReadResult>>),
}
impl Reply {
    fn send(self, result: ApiResult<RunMemoryReadResult>) {
        match self {
            Self::Owner(reply) => {
                let _ = reply.send(result.map(|result| match result {
                    RunMemoryReadResult::List(page) => BotResponse::MemoryList { page },
                    RunMemoryReadResult::Detail(page) => BotResponse::MemoryDetail { page },
                }));
            }
            Self::Run(reply) => {
                let _ = reply.send(result);
            }
        }
    }
}

pub(in crate::worker) struct MemoryReadJob {
    serial: u64,
    bot: Bot,
    owner: BotOwner,
    binding: Option<BotMemoryBinding>,
    subject: Subject,
    operation: MemoryReadOperation,
    cancellation: Cancellation,
    deadline: Instant,
    key: Arc<cursor::CursorKey>,
    authority: Arc<dyn BotAuthority>,
    delegate: Option<Arc<dyn BotDelegatePort>>,
    reply: Reply,
}
pub(in crate::worker) struct MemoryReadResult {
    job: MemoryReadJob,
    result: ApiResult<ReadPage>,
}
struct ReadPage {
    page: MemoryPageResult,
    scope: Option<cursor::Scope>,
    lifetime: Option<(u64, u64)>,
}

fn memory_failure(error: MemoryAuthorityError) -> Failure {
    failure(
        error.code(),
        "The Memory read could not be completed.",
        matches!(
            error,
            MemoryAuthorityError::Authority(BotError::Unavailable | BotError::Storage)
        ),
    )
}
fn cursor_failure(error: cursor::CursorError) -> Failure {
    failure(
        match error {
            cursor::CursorError::Invalid => "invalid_memory_cursor",
            cursor::CursorError::Expired => "memory_cursor_expired",
        },
        "Restart this Memory read from its first page.",
        false,
    )
}
fn operation(operation: &MemoryReadOperation) -> (MemoryView, Option<&str>, MemoryReadPosition) {
    match operation {
        MemoryReadOperation::List {
            view,
            cursor,
            limit,
        } => (
            *view,
            cursor.as_deref(),
            MemoryReadPosition::List {
                after: 0,
                limit: *limit,
            },
        ),
        MemoryReadOperation::Detail {
            view,
            cursor,
            fact_id,
        } => (
            *view,
            cursor.as_deref(),
            MemoryReadPosition::Detail {
                fact_id: *fact_id,
                node_ordinal: 0,
                start_utf8: 0,
            },
        ),
    }
}

impl MemoryReadJob {
    fn live(&self) -> ApiResult<()> {
        if self.cancellation.is_cancelled() || Instant::now() >= self.deadline {
            return Err(unavailable_failure());
        }
        Ok(())
    }
    fn generation(&self) -> ApiResult<()> {
        self.live()?;
        if let Subject::Run { request, .. } = &self.subject {
            let delegate = self.delegate.as_ref().ok_or_else(unavailable_failure)?;
            match delegate.execute_within(
                DelegateCommand::ResolveRunAuthority {
                    run: request.run,
                    generation: request.generation,
                },
                Duration::from_secs(5),
            ) {
                // A read stays available while a decision waits (O147).
                Ok(BotReply::RunAuthority { authority })
                    if authority.bot_subject().is_some_and(|(provenance, policy)| {
                        *provenance == request.provenance && *policy == request.captured
                    }) => {}
                Err(_) => return Err(unavailable_failure()),
                _ => return Err(bot_failure(BotError::Forbidden)),
            }
        }
        self.live()
    }
    pub(in crate::worker) fn run(self) -> MemoryReadResult {
        let result = catch_unwind(AssertUnwindSafe(|| self.read()))
            .unwrap_or_else(|_| Err(unavailable_failure()));
        MemoryReadResult { job: self, result }
    }
    fn read(&self) -> ApiResult<ReadPage> {
        self.operation.validate_bounds().map_err(bot_failure)?;
        self.generation()?;
        let proof = self
            .authority
            .inspect(&[self.bot.home_workspace], &[])
            .map_err(bot_failure)?;
        if proof.owner != self.owner {
            return Err(bot_failure(BotError::Forbidden));
        }
        let profile = proof
            .workspace(self.bot.home_workspace)
            .map_err(bot_failure)?;
        let (view, token, position) = operation(&self.operation);
        let capture = match &self.subject {
            Subject::Owner => None,
            Subject::Run { capture, .. } => Some(capture),
        };
        if let Some(capture) = capture {
            capture
                .validate(self.bot.home_workspace)
                .map_err(bot_failure)?;
            if matches!(capture.memory, BotCapturedMemory::Absent {})
                && capture.index_digest != *blake3::hash(&[]).as_bytes()
            {
                return Err(memory_failure(MemoryAuthorityError::CaptureUnavailable));
            }
            if matches!(capture.memory, BotCapturedMemory::Captured { .. })
                && self.binding.is_none()
            {
                return Err(memory_failure(MemoryAuthorityError::CaptureUnavailable));
            }
        }
        let captured_absent = view == MemoryView::Captured
            && capture
                .is_some_and(|capture| matches!(capture.memory, BotCapturedMemory::Absent {}));
        let page = if captured_absent || self.binding.is_none() {
            if token.is_some() {
                return Err(cursor_failure(cursor::CursorError::Invalid));
            }
            ReadPage {
                page: match self.operation {
                    MemoryReadOperation::List { .. } => MemoryPageResult::List {
                        page: BotMemoryListPage::Absent {},
                        next: None,
                    },
                    MemoryReadOperation::Detail { .. } => MemoryPageResult::Detail {
                        page: BotMemoryDetailPage::Absent {},
                        next: None,
                    },
                },
                scope: None,
                lifetime: None,
            }
        } else {
            let binding = self.binding.as_ref().expect("checked binding");
            let subject = match &self.subject {
                Subject::Owner => cursor::Subject::Owner {},
                Subject::Run { request, .. } => cursor::Subject::Run {
                    run: request.run,
                    generation: request.generation,
                    submission: request.provenance.submission,
                },
            };
            let scope = cursor::Scope {
                owner: self.owner,
                bot: self.bot.id,
                profile,
                page: binding.page.clone(),
                creation_request: binding.creation_request,
                view,
                subject,
            };
            let captured_version = if view == MemoryView::Captured {
                let capture = capture.ok_or_else(|| bot_failure(BotError::Forbidden))?;
                let BotCapturedMemory::Captured {
                    version,
                    page,
                    creation_request,
                    ..
                } = &capture.memory
                else {
                    return Err(memory_failure(MemoryAuthorityError::CaptureUnavailable));
                };
                if *page != binding.page || *creation_request != binding.creation_request {
                    return Err(memory_failure(MemoryAuthorityError::CaptureUnavailable));
                }
                Some(*version)
            } else {
                None
            };
            let (snapshot, position, lifetime) = if let Some(token) = token {
                let next = self
                    .key
                    .open(token, &scope, &self.operation)
                    .map_err(cursor_failure)?;
                if captured_version.is_some_and(|version| version != next.version) {
                    return Err(cursor_failure(cursor::CursorError::Invalid));
                }
                (
                    MemorySnapshot::Exact(next.version),
                    next.position,
                    Some(next.lifetime),
                )
            } else {
                (
                    captured_version
                        .map(MemorySnapshot::Exact)
                        .unwrap_or(MemorySnapshot::Current),
                    position,
                    None,
                )
            };
            // Authenticate a continuation before any historical body lookup.
            if view == MemoryView::Captured {
                self.authority
                    .replay_memory_index(
                        self.owner,
                        profile,
                        self.bot.home_workspace,
                        capture.expect("captured version validated"),
                        Some(binding),
                        &self.cancellation,
                    )
                    .map_err(memory_failure)?;
            }
            let page = self
                .authority
                .read_memory_page(
                    self.owner,
                    profile,
                    binding,
                    snapshot,
                    position,
                    &self.cancellation,
                )
                .map_err(memory_failure)?;
            ReadPage {
                page,
                scope: Some(scope),
                lifetime,
            }
        };
        let proof = self
            .authority
            .inspect(&[self.bot.home_workspace], &[])
            .map_err(bot_failure)?;
        if proof.owner != self.owner
            || proof
                .workspace(self.bot.home_workspace)
                .map_err(bot_failure)?
                != profile
        {
            return Err(bot_failure(BotError::Forbidden));
        }
        self.generation()?;
        Ok(page)
    }
}

impl WorkerState {
    fn memory_run_snapshot(
        &self,
        request: &RunMemoryReadRequest,
    ) -> ApiResult<(Bot, BotLibraryCapture, Option<BotMemoryBinding>)> {
        let p = &request.provenance;
        let owner = BotOwner {
            user_scope: p.owner.user_scope,
            device: p.owner.device,
        };
        let bot = self
            .repository
            .bot(p.bot)
            .map_err(bot_failure)?
            .ok_or_else(not_found)?;
        if bot.user_scope != owner.user_scope || bot.owner_device != owner.device {
            return Err(bot_failure(BotError::Forbidden));
        }
        let receipt = self
            .repository
            .bot_submission_for_run(owner, p.bot, request.run)
            .map_err(bot_failure)?
            .ok_or_else(not_found)?;
        if receipt.intent.submission.provenance != *p
            || receipt.intent.submission.policy != request.captured
            || receipt.intent.submission.workspace != bot.home_workspace
        {
            return Err(bot_failure(BotError::Forbidden));
        }
        let capture = self
            .repository
            .bot_library_capture(owner, p.bot, p.submission)
            .map_err(bot_failure)?
            .ok_or_else(|| memory_failure(MemoryAuthorityError::CaptureUnavailable))?;
        let binding = self
            .repository
            .bot_library(owner, p.bot)
            .map_err(bot_failure)?
            .binding;
        Ok((bot, capture, binding))
    }

    pub(in crate::worker) fn request_run_memory_read(
        &mut self,
        request: RunMemoryReadRequest,
        reply: SyncSender<ApiResult<RunMemoryReadResult>>,
        deadline: Instant,
    ) {
        if !BOT_MEMORY_AVAILABLE {
            let _ = reply.send(Err(failure(
                "unsupported_capability",
                "Bot Memory is not available yet.",
                false,
            )));
            return;
        }
        self.enqueue_run_memory_read(request, reply, deadline);
    }

    pub(in crate::worker) fn enqueue_run_memory_read(
        &mut self,
        request: RunMemoryReadRequest,
        reply: SyncSender<ApiResult<RunMemoryReadResult>>,
        deadline: Instant,
    ) {
        let prepared = (|| {
            request.operation.validate_bounds().map_err(bot_failure)?;
            let (bot, capture, binding) = self.memory_run_snapshot(&request)?;
            let owner = BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device,
            };
            let key = self.memory_reads.key(owner);
            Ok(MemoryReadJob {
                serial: 0,
                bot,
                owner,
                binding,
                operation: request.operation.clone(),
                cancellation: request.cancellation.clone(),
                deadline,
                key,
                authority: self.bot_authority.clone().ok_or_else(unavailable_failure)?,
                delegate: self.bot_delegate.clone(),
                subject: Subject::Run { request, capture },
                reply: Reply::Run(reply.clone()),
            })
        })();
        match prepared {
            Ok(job) => self.submit_memory_read(job),
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    pub(in crate::worker) fn read_owner_memory(
        &mut self,
        context: CommandContext,
        command: BotCommand,
        reply: SyncSender<ApiResult<BotResponse>>,
    ) {
        let prepared = (|| {
            authorize(&context, AiScope::ManageBots)?;
            if let Some(error) = closed_memory_context_failure(&context, &command) {
                return Err(error);
            }
            let (bot_id, operation) = match command {
                BotCommand::ListMemory {
                    bot_id,
                    cursor,
                    limit,
                } => (
                    bot_id,
                    MemoryReadOperation::List {
                        view: MemoryView::Current,
                        cursor,
                        limit,
                    },
                ),
                BotCommand::ReadMemoryFact {
                    bot_id,
                    cursor,
                    fact_id,
                } => (
                    bot_id,
                    MemoryReadOperation::Detail {
                        view: MemoryView::Current,
                        cursor,
                        fact_id,
                    },
                ),
                _ => return Err(bot_failure(BotError::Invalid)),
            };
            operation.validate_bounds().map_err(bot_failure)?;
            let proof = context.prepared.as_ref().ok_or_else(unavailable_failure)?;
            let bot = lifecycle::readable_bot(&self.repository, bot_id)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?;
            validate_owner(&proof.authority, &bot)?;
            let owner = BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device,
            };
            let binding = self
                .repository
                .bot_library(owner, bot_id)
                .map_err(bot_failure)?
                .binding;
            let key = self.memory_reads.key(owner);
            Ok(MemoryReadJob {
                serial: 0,
                bot,
                owner,
                binding,
                subject: Subject::Owner,
                operation,
                cancellation: Cancellation::default(),
                deadline: Instant::now() + RESPONSE_TIMEOUT,
                key,
                authority: self.bot_authority.clone().ok_or_else(unavailable_failure)?,
                delegate: None,
                reply: Reply::Owner(reply.clone()),
            })
        })();
        match prepared {
            Ok(job) => self.submit_memory_read(job),
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    fn submit_memory_read(&mut self, mut job: MemoryReadJob) {
        let Some(serial) = self.memory_reads.serial.checked_add(1) else {
            job.reply.send(Err(unavailable_failure()));
            return;
        };
        job.serial = serial;
        let Some(pool) = self.provider_admin.as_ref() else {
            job.reply.send(Err(unavailable_failure()));
            return;
        };
        match pool.try_submit(ProviderAdminJob::BotMemoryRead(job)) {
            Ok(()) => self.memory_reads.serial = serial,
            Err(AdminSubmitError {
                job: ProviderAdminJob::BotMemoryRead(job),
                failure,
            }) => job.reply.send(Err(failure)),
            Err(_) => unreachable!("submitted Memory read returned another job"),
        }
    }

    pub(in crate::worker) fn complete_memory_read(&mut self, result: MemoryReadResult) {
        let MemoryReadResult { job, result } = result;
        let checked = (|| {
            if job.serial <= self.memory_reads.completed
                || job.serial > self.memory_reads.serial
                || self
                    .memory_reads
                    .key
                    .as_ref()
                    .is_none_or(|(owner, key)| *owner != job.owner || !Arc::ptr_eq(key, &job.key))
            {
                return Err(unavailable_failure());
            }
            job.live()?;
            let (current, binding) = match &job.subject {
                Subject::Owner => {
                    let current = lifecycle::readable_bot(&self.repository, job.bot.id)
                        .map_err(bot_failure)?
                        .ok_or_else(not_found)?;
                    let binding = self
                        .repository
                        .bot_library(job.owner, job.bot.id)
                        .map_err(bot_failure)?
                        .binding;
                    (current, binding)
                }
                Subject::Run { request, capture } => {
                    let (current, current_capture, binding) = self.memory_run_snapshot(request)?;
                    if current_capture != *capture {
                        return Err(memory_failure(MemoryAuthorityError::CaptureUnavailable));
                    }
                    (current, binding)
                }
            };
            if current.revision != job.bot.revision
                || current.policy_revision != job.bot.policy_revision
                || current.home_workspace != job.bot.home_workspace
                || current.lifecycle != job.bot.lifecycle
                || current.user_scope != job.owner.user_scope
                || current.owner_device != job.owner.device
                || binding != job.binding
            {
                return Err(bot_failure(BotError::Conflict));
            }
            let mut page = result?;
            let output = match &mut page.page {
                MemoryPageResult::List { page: body, next } => {
                    if let Some(next) = next {
                        let BotMemoryListPage::Observed {
                            version,
                            continuation,
                            ..
                        } = body
                        else {
                            return Err(bot_failure(BotError::Storage));
                        };
                        *continuation = Some(
                            job.key
                                .issue(
                                    page.scope.as_ref().ok_or_else(unavailable_failure)?,
                                    *version,
                                    next,
                                    page.lifetime,
                                )
                                .map_err(cursor_failure)?,
                        );
                    }
                    RunMemoryReadResult::List(body.clone())
                }
                MemoryPageResult::Detail { page: body, next } => {
                    if let Some(next) = next {
                        let BotMemoryDetailPage::Observed {
                            version,
                            continuation,
                            ..
                        } = body
                        else {
                            return Err(bot_failure(BotError::Storage));
                        };
                        *continuation = Some(
                            job.key
                                .issue(
                                    page.scope.as_ref().ok_or_else(unavailable_failure)?,
                                    *version,
                                    next,
                                    page.lifetime,
                                )
                                .map_err(cursor_failure)?,
                        );
                    }
                    RunMemoryReadResult::Detail(body.clone())
                }
            };
            let bytes = match &output {
                RunMemoryReadResult::List(page) => serde_json::to_vec(page),
                RunMemoryReadResult::Detail(page) => serde_json::to_vec(page),
            }
            .map_err(|_| bot_failure(BotError::Storage))?;
            if bytes.len() > 4096 {
                return Err(bot_failure(BotError::ContextTooLarge));
            }
            job.live()?;
            Ok(output)
        })();
        self.memory_reads.completed = self.memory_reads.completed.max(job.serial);
        job.reply.send(checked);
    }
}
