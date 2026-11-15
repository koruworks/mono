// SPDX-License-Identifier: MPL-2.0
//! Writer continuations never hold a writer/Store handle across external work.
use super::job::{Evidence, FreshCreate, Phase};
use super::*;

fn owner(subject: &RunBotSubject) -> BotOwner {
    BotOwner {
        user_scope: subject.provenance.owner.user_scope,
        device: subject.provenance.owner.device,
    }
}
fn policy_request(subject: &RunBotSubject) -> policy::NativePolicyRequest {
    policy::NativePolicyRequest {
        run: subject.run,
        provenance: subject.provenance.clone(),
        captured: subject.captured.clone(),
        target: None,
    }
}

impl WorkerState {
    fn retained_run_bot_subject(&self, requested: &RunBotSubject) -> ApiResult<RoutineToolSubject> {
        if !requested.provenance.valid()
            || !requested.captured.valid()
            || requested.generation.get() == 0
        {
            return Err(invalid());
        }
        let subject = self
            .repository
            .routine_tool_subject(owner(requested), requested.provenance.bot, requested.run)
            .map_err(tool_failure)?;
        let p = &requested.provenance;
        let s = &subject.receipt.intent.submission;
        if s.provenance != *p
            || s.policy != requested.captured
            || requested.captured.workspace != subject.metadata.home_workspace
            || p.config_revision > subject.metadata.revision
            || p.policy_revision > subject.metadata.policy_revision
        {
            return Err(denied());
        }
        if let Some(id) = p.ai_attempt_id {
            let attempt_id = InferenceAttemptId::parse(&id.to_string()).ok_or_else(invalid)?;
            let attempt = self
                .repository
                .attempt(attempt_id)
                .map_err(ai_failure)?
                .ok_or_else(not_found)?;
            if attempt.status.is_terminal()
                || attempt.chat_id.map(|id| id.as_uuid()) != Some(p.chat_id)
                || attempt.assistant_message_id.map(|id| id.as_uuid()) != p.assistant_message_id
                || self
                    .repository
                    .bot_replay_state(attempt_id)
                    .map_err(ai_failure)?
                    .stop_reason
                    .is_some()
            {
                return Err(denied());
            }
        }
        Ok(subject)
    }
    fn retained_tool_create(
        &self,
        requested: &RunRoutineRequest,
    ) -> ApiResult<Option<BotRoutineCreationReceipt>> {
        let Some(command) = requested
            .operation
            .create_command(requested.subject.provenance.bot)
        else {
            return Ok(None);
        };
        let (input, fingerprint) = create_body(&command)?;
        let Some((creation, receipt)) = self
            .repository
            .routine_tool_creation_by_request(owner(&requested.subject), input.bot, input.request)
            .map_err(tool_failure)?
        else {
            return Ok(None);
        };
        if creation.identity.command_fingerprint != fingerprint
            || !input.matches_retained(&creation.identity)
        {
            return Err(tool_failure(BotError::Conflict));
        }
        Ok(Some(receipt))
    }
    pub(in crate::worker) fn start_run_routine(
        &mut self,
        request: RunRoutineRequest,
        reply: SyncSender<ApiResult<RunRoutineResult>>,
    ) {
        let prepared = (|| {
            request.operation.validate(request.subject.provenance.bot)?;
            if request.cancellation.is_cancelled() || Instant::now() >= request.deadline {
                return Err(timed_out());
            }
            let ticket = self.routine_listings.acquire()?;
            let subject = self.retained_run_bot_subject(&request.subject)?;
            let fresh = if matches!(request.operation, RunRoutineOperation::Create(_))
                && self.retained_tool_create(&request)?.is_none()
            {
                if subject.metadata.lifecycle != BotLifecycle::Active {
                    return Err(denied());
                }
                if subject.metadata.pending_change.is_some() {
                    return Err(tool_failure(BotError::Settling));
                }
                let bot = self
                    .repository
                    .bot(subject.metadata.id)
                    .map_err(tool_failure)?
                    .ok_or_else(not_found)?;
                let routine = policy::routine::snapshot(
                    &self.repository,
                    &bot,
                    &policy_request(&request.subject),
                )?;
                Some(FreshCreate { bot, routine })
            } else {
                None
            };
            Ok(Box::new(RunRoutineJob {
                request,
                subject,
                ticket,
                authority: self.bot_authority.clone().ok_or_else(unavailable_failure)?,
                delegate: self.bot_delegate.clone().ok_or_else(unavailable_failure)?,
                phase: Phase::Caller,
                progress: None,
                fresh,
                value: None,
                committed: false,
                reply: reply.clone(),
            }))
        })();
        match prepared {
            Ok(job) => self.submit_run_routine(job),
            Err(error) => {
                let _ = reply.send(Err(caller_failure(error)));
            }
        }
    }
    fn submit_run_routine(&mut self, job: Box<RunRoutineJob>) {
        if let Err(error) = job.live() {
            job.refuse(error);
            return;
        }
        let Some(pool) = &self.provider_admin else {
            job.refuse(unavailable_failure());
            return;
        };
        match pool.try_submit(ProviderAdminJob::BotRoutineModel(job)) {
            Ok(()) => {}
            Err(AdminSubmitError {
                job: ProviderAdminJob::BotRoutineModel(job),
                failure,
            }) => job.refuse(failure),
            Err(_) => unreachable!("Routine setup returned another job"),
        }
    }
    fn current_run_routine(&self, job: &RunRoutineJob) -> ApiResult<()> {
        job.live()?;
        if !self.routine_listings.owns(&job.ticket)
            || self.retained_run_bot_subject(&job.request.subject)? != job.subject
        {
            return Err(denied());
        }
        Ok(())
    }
    fn tool_creation_status(
        &self,
        job: &RunRoutineJob,
        request: BotRequestId,
    ) -> ApiResult<RunRoutineResult> {
        let m = &job.subject.metadata;
        let (creation, _) = self
            .repository
            .routine_tool_creation_by_request(m.owner, m.id, request)
            .map_err(tool_failure)?
            .ok_or_else(not_found)?;
        Ok(RunRoutineResult::Creation(
            self.routine_tool_status(m, &creation),
        ))
    }
    fn run_routine_command(&mut self, job: &mut RunRoutineJob) -> ApiResult<()> {
        let m = &job.subject.metadata;
        job.value = match &job.request.operation {
            RunRoutineOperation::Create(_) => {
                // Resolve a concurrent exact winner before any fresh source
                // recheck or allocation. The actual writer rechecks once more.
                let receipt = if let Some(receipt) = self.retained_tool_create(&job.request)? {
                    receipt
                } else {
                    let fresh = job
                        .fresh
                        .as_ref()
                        .ok_or_else(|| tool_failure(BotError::Conflict))?;
                    let current = self.repository.bot(m.id).map_err(tool_failure)?;
                    policy::routine::recheck(
                        &self.repository,
                        current.as_ref(),
                        &fresh.bot,
                        &policy_request(&job.request.subject),
                        fresh.routine.as_deref(),
                    )?;
                    let command = job
                        .request
                        .operation
                        .create_command(m.id)
                        .ok_or_else(invalid)?;
                    let (input, fingerprint) = create_body(&command)?;
                    job.live()?;
                    let receipt =
                        self.prepare_routine_creation_intent(m.owner, &input, fingerprint)?;
                    job.committed = true;
                    receipt
                };
                Some(RunRoutineResult::Created((&receipt).into()))
            }
            RunRoutineOperation::ReadCreation { request_id } => {
                Some(self.tool_creation_status(job, *request_id)?)
            }
            RunRoutineOperation::RetryCreation {
                original_request_id,
                observed_token,
            } => {
                self.retry_routine_creation_intent(
                    m.owner,
                    m.id,
                    *original_request_id,
                    *observed_token,
                )?;
                job.committed = true;
                Some(self.tool_creation_status(job, *original_request_id)?)
            }
            RunRoutineOperation::List {
                after_schedule,
                limit,
            } => {
                job.progress = Some(listing::ListingProgress::new(
                    &self.repository,
                    m.owner,
                    m.id,
                    *after_schedule,
                    *limit,
                )?);
                None
            }
        };
        Ok(())
    }
    pub(in crate::worker) fn complete_run_routine(
        &mut self,
        completion: Box<RunRoutineCompletion>,
    ) {
        let RunRoutineCompletion { mut job, result } = *completion;
        let next = (|| -> ApiResult<bool> {
            self.current_run_routine(&job)?;
            let evidence = result?;
            let phase = std::mem::replace(&mut job.phase, Phase::Reply);
            match (phase, evidence) {
                (Phase::Caller, Evidence::Caller) => {
                    self.routine_listings
                        .bind_owner(job.subject.metadata.owner)?;
                    self.run_routine_command(&mut job)?;
                }
                (Phase::Item(_), Evidence::Item(observation)) => {
                    job.progress
                        .as_mut()
                        .ok_or_else(unavailable_failure)?
                        .observe(observation)?;
                }
                (Phase::Reply, Evidence::Reply) => {
                    if let Some(progress) = &mut job.progress {
                        job.value = Some(RunRoutineResult::Routines(
                            progress.finish(&self.repository)?,
                        ));
                    }
                    self.current_run_routine(&job)?;
                    return Ok(true);
                }
                _ => return Err(unavailable_failure()),
            }
            if let Some(progress) = &mut job.progress {
                if let Some(creation) = progress.next(&self.repository, || {
                    job.ticket.valid()
                        && !job.request.cancellation.is_cancelled()
                        && Instant::now() < job.request.deadline
                })? {
                    job.phase = Phase::Item(Box::new(creation));
                    return Ok(false);
                }
            }
            job.phase = Phase::Reply;
            Ok(false)
        })();
        match next {
            Ok(false) => self.submit_run_routine(job),
            Ok(true) => {
                if let Some(value) = job.value.take() {
                    let _ = job.reply.send(Ok(value));
                } else {
                    job.refuse(unavailable_failure());
                }
            }
            Err(error) => job.refuse(error),
        }
    }
}
