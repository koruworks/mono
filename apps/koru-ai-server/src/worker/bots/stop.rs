// SPDX-License-Identifier: MPL-2.0
//! `stop_task`: the person's own Stop for a task started through the API
//! (`start`/`continue`), named by that operation's request.
//!
//! Such a task has no reply, so a conversation's Stop never reaches it. The
//! writer checks the caller owns the Bot and that the request names one of
//! its API tasks; a job on the interactive queue of the Stop lane
//! (`koru-ai-provider-control`) asks the execution owner for the task's
//! admission and sends the delegate's `Stop` with the fence the owner
//! reported: the same lease-free Stop the conversation's Stop and Archive
//! send, which cancels a queued Run before it starts and ends a running one
//! even while a person holds its session. Never the automatic `Interrupt`.
//! Nothing is stored on the AI side: the owner's ledger is what settles.
use super::*;
use koru_delegate_api::bots::{BotCommand as DelegateCommand, BotDelegatePort, BotReply};
use koru_delegate_core::{BotAdmissionState, BotRunKey, BotSubmissionKey, RunId};

/// How long a Stop may take from the writer's dispatch to its answer, both
/// owner calls included; as `read_run`'s deadline, at most half the caller's
/// own `RESPONSE_TIMEOUT`, so an owner that does not answer is answered
/// `bot_unavailable` (retryable), not a worker timeout. Shorter under test.
#[cfg(not(test))]
pub(in crate::worker) const TASK_STOP_DEADLINE: Duration = Duration::from_secs(15);
#[cfg(test)]
pub(in crate::worker) const TASK_STOP_DEADLINE: Duration = Duration::from_secs(3);
const _: () = assert!(TASK_STOP_DEADLINE.as_millis() * 2 <= RESPONSE_TIMEOUT.as_millis());

/// How many times a Stop the owner refused for its fence (the Run started,
/// or moved to a new attempt, between the admission read and the Stop) reads
/// the admission again and retries.
const FENCE_ROUNDS: usize = 3;

/// The pause before a Stop its owner applied but could not yet confirm
/// (`bot_stop_unconfirmed`: the Run is cancelled, its record not yet durably
/// settled) is read and sent again, until the deadline. Seen against the
/// real owner in the scripted native journey: a running Run's first Stop is
/// usually unconfirmed.
const UNCONFIRMED_PAUSE: Duration = Duration::from_millis(100);

/// A `stop_task` the writer has checked: what the job needs to check
/// authority again and ask the execution owner.
struct CheckedStop {
    authority: Arc<dyn BotAuthority>,
    owner: BotOwner,
    /// The workspace the task's submission names, where its Run's record is.
    workspace: WorkspaceId,
    key: BotSubmissionKey,
    run: RunId,
    delegate: Arc<dyn BotDelegatePort>,
}

/// One `stop_task`, off the writer on the Stop lane's interactive queue,
/// answered to its caller directly. Holds no writer state.
pub(in crate::worker) struct BotTaskStopJob {
    stop: CheckedStop,
    deadline: Instant,
    reply: SyncSender<ApiResult<BotResponse>>,
}

fn unavailable() -> Failure {
    failure(
        "bot_unavailable",
        "The Bot's execution owner did not answer the Stop. Try again.",
        true,
    )
}

impl BotTaskStopJob {
    pub(in crate::worker) fn run(self) {
        let answer = if Instant::now() >= self.deadline {
            Err(unavailable())
        } else {
            catch_unwind(AssertUnwindSafe(|| self.stop.stop(self.deadline)))
                .unwrap_or_else(|_| Err(bot_failure(BotError::Unavailable)))
        };
        let _ = self.reply.send(answer);
    }
}

impl CheckedStop {
    fn answer(&self, outcome: BotTaskStopOutcome) -> BotResponse {
        BotResponse::TaskStop {
            stop: BotTaskStop {
                request_id: self.key.request,
                bot_id: self.key.bot,
                run_id: self.run,
                outcome,
            },
        }
    }

    fn within(deadline: Instant) -> ApiResult<Duration> {
        let within = deadline.saturating_duration_since(Instant::now());
        if within.is_zero() {
            return Err(unavailable());
        }
        Ok(within)
    }

    fn stop(&self, deadline: Instant) -> ApiResult<BotResponse> {
        let checked = self
            .authority
            .inspect(&[self.workspace], &[])
            .map_err(bot_failure)?;
        if checked.owner != self.owner {
            return Err(bot_failure(BotError::Forbidden));
        }
        checked.workspace(self.workspace).map_err(bot_failure)?;
        // A Stop the owner applied but has not confirmed: once the task
        // reads settled, it is this Stop that ended it.
        let mut sent = false;
        let mut fenced = 0;
        loop {
            // The owner's own record of the task, now: its state and the
            // fence its Stop must name (none while it is queued).
            let admission = match self.delegate.execute_within(
                DelegateCommand::Reconcile {
                    key: self.key.clone(),
                },
                Self::within(deadline)?,
            ) {
                Ok(BotReply::Admission { admission })
                    if admission.key == self.key && admission.run == self.run =>
                {
                    admission
                }
                // The owner holds no such task: it never says it exists
                // elsewhere.
                Ok(BotReply::Missing) => return Err(not_found()),
                Ok(_) => return Err(unavailable()),
                Err(refused) => return Err(owner_failure(&refused)),
            };
            if matches!(
                admission.state,
                BotAdmissionState::Settled | BotAdmissionState::Refused
            ) {
                return Ok(self.answer(if sent {
                    BotTaskStopOutcome::Stopped
                } else {
                    BotTaskStopOutcome::AlreadySettled
                }));
            }
            match self.delegate.execute_within(
                DelegateCommand::Stop {
                    key: BotRunKey {
                        owner: self.key.owner,
                        bot: self.key.bot,
                        run: self.run,
                    },
                    fence: admission.fence,
                },
                Self::within(deadline)?,
            ) {
                Ok(BotReply::Applied) => return Ok(self.answer(BotTaskStopOutcome::Stopped)),
                Ok(_) => return Err(unavailable()),
                // The Run started, or moved on, since the read: read again.
                Err(refused)
                    if matches!(refused.code.as_str(), "bot_fenced" | "bot_fence_required") =>
                {
                    fenced += 1;
                    if fenced >= FENCE_ROUNDS {
                        return Err(unavailable());
                    }
                }
                // Applied, not yet confirmed: read and send again shortly.
                // Past the deadline the caller is answered retryable.
                Err(refused) if refused.code == "bot_stop_unconfirmed" => {
                    sent = true;
                    std::thread::sleep(UNCONFIRMED_PAUSE.min(Self::within(deadline)?));
                }
                Err(refused) => return Err(owner_failure(&refused)),
            }
        }
    }
}

/// The execution owner's refusal, in the Bots API's codes. The AI owner has
/// already checked the caller owns this Bot and that the request is this
/// Bot's API task, so a Run the owner does not hold (`unknown_run`) or binds
/// differently (`bot_owner_mismatch`) is `not_found`; anything else is
/// `bot_unavailable`, retryable: the same Stop sent again reads the owner's
/// state afresh and answers what it finds. (An unconfirmed Stop is sent
/// again within the deadline before it comes to that.)
fn owner_failure(refused: &koru_delegate_api::Failure) -> Failure {
    match refused.code.as_str() {
        "unknown_run" | "bot_owner_mismatch" => not_found(),
        _ => unavailable(),
    }
}

impl WorkerState {
    /// `stop_task`: checked here, sent off the writer. Every refusal and
    /// every answer that needs no owner call is answered here.
    pub(in crate::worker) fn stop_bot_task(
        &mut self,
        context: CommandContext,
        request: BotCommand,
        reply: SyncSender<ApiResult<BotResponse>>,
    ) {
        let job = match self.bot_task_stop(&context, &request) {
            Ok(Ok(job)) => job,
            Ok(Err(answer)) => {
                let _ = reply.send(Ok(answer));
                return;
            }
            Err(error) => {
                // A task still being delivered: offer it now, so the
                // person's retry finds it admitted.
                if error.code == BotError::Settling.code() {
                    self.bot_recovery_retry.soon(Instant::now());
                }
                let _ = reply.send(Err(error));
                return;
            }
        };
        let job = BotTaskStopJob {
            stop: job,
            deadline: Instant::now() + TASK_STOP_DEADLINE,
            reply,
        };
        let Some(pool) = self.provider_admin.as_ref() else {
            let _ = job.reply.send(Err(bot_failure(BotError::Unavailable)));
            return;
        };
        if let Err(error) = pool.try_submit(ProviderAdminJob::BotTaskStop(job))
            && let ProviderAdminJob::BotTaskStop(job) = error.job
        {
            let _ = job.reply.send(Err(error.failure));
        }
    }

    /// The owner first (another owner's Bot is `wrong_owner`), then the
    /// Bot's workspace, then the task: one of this Bot's `start`/`continue`
    /// operations with no conversation attempt, or `not_found`. A Stop only
    /// reduces work, so no hold refuses it: a paused, archived or trashed
    /// Bot, one mid-operation and a conversation in Trash are all answered.
    /// `Ok(Err(answer))` when no owner call is needed.
    fn bot_task_stop(
        &self,
        context: &CommandContext,
        request: &BotCommand,
    ) -> ApiResult<Result<CheckedStop, BotResponse>> {
        let BotCommand::StopTask { request_id, bot_id } = *request else {
            return Err(bot_failure(BotError::Invalid));
        };
        authorize(context, AiScope::ManageBots)?;
        let proof = context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        let owner = proof.authority.owner().map_err(bot_failure)?;
        let (bot, _) = self
            .repository
            .bot_any_lifecycle(bot_id)
            .map_err(bot_failure)?
            .ok_or_else(not_found)?;
        if (BotOwner {
            user_scope: bot.user_scope,
            device: bot.owner_device,
        }) != owner
        {
            return Err(bot_failure(BotError::Forbidden));
        }
        proof
            .authority
            .workspace(bot.home_workspace)
            .map_err(bot_failure)?;
        let receipt = self
            .repository
            .bot_api_task(owner, bot_id, request_id)
            .map_err(bot_failure)?
            .ok_or_else(not_found)?;
        let run = receipt.intent.submission.request.run;
        match receipt.stage {
            // Refused before its owner admitted it: nothing of it ran or
            // will run, and its owner is not asked.
            koru_ai_core::bots::BotReceiptStage::Rejected => {
                return Ok(Err(BotResponse::TaskStop {
                    stop: BotTaskStop {
                        request_id,
                        bot_id,
                        run_id: run,
                        outcome: BotTaskStopOutcome::NeverAdmitted,
                    },
                }));
            }
            // Still being delivered to its owner, which may not hold it yet:
            // a Stop could not reach it. Retryable once it is accepted.
            koru_ai_core::bots::BotReceiptStage::Prepared => {
                return Err(bot_failure(BotError::Settling));
            }
            _ => {}
        }
        let unavailable = || {
            failure(
                "bot_unavailable",
                "The Bot's execution owner is not available.",
                true,
            )
        };
        let authority = self.bot_authority.clone().ok_or_else(unavailable)?;
        let delegate = self.bot_delegate.clone().ok_or_else(unavailable)?;
        Ok(Ok(CheckedStop {
            authority,
            owner,
            workspace: receipt.intent.submission.workspace,
            key: BotSubmissionKey {
                owner: koru_delegate_core::BotOwner {
                    user_scope: owner.user_scope,
                    device: owner.device,
                },
                bot: bot_id,
                request: request_id,
            },
            run,
            delegate,
        }))
    }
}
