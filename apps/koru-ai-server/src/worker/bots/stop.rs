// SPDX-License-Identifier: MPL-2.0
//! `stop_task`: the person's own Stop for a task started through the API
//! (`start`/`continue`), named by that operation's request.
//!
//! Such a task has no reply, so a conversation's Stop never reaches it. The
//! writer checks the caller owns the Bot, that its prepared proof reaches the
//! workspace the task's Run is recorded in, and that the request names one of
//! its API tasks. A job on the task Stops' own lane
//! (`koru-ai-provider-task-stop`) then makes one attempt, every call bounded
//! by one deadline and none a Store call: the owner's `Reconcile` (the task's
//! state and current fence), its `Stop` with that fence (the same lease-free
//! Stop a conversation's Stop and Archive send, never the automatic
//! `Interrupt`), and, once the Stop is applied, a one-entry `Read` of the
//! Run's status to tell the person's cancellation from a Run that ended by
//! itself in between. It never waits or sends again by itself: a Stop the
//! owner has not confirmed yet is answered `stopping`, and the caller asks
//! again. Nothing is stored on the AI side; the owner's ledger settles.
use super::*;
use koru_delegate_api::bots::{BotCommand as DelegateCommand, BotDelegatePort, BotReply};
use koru_delegate_api::wire::TimelineQuery;
use koru_delegate_core::{BotAdmissionState, BotRunKey, BotSubmissionKey, RunId, RunStatus};

/// How long a Stop may take from the writer's dispatch to its answer, every
/// owner call included; as `read_run`'s deadline, at most half the caller's
/// own `RESPONSE_TIMEOUT`, so an owner that does not answer is answered
/// `bot_unavailable` (retryable), not a worker timeout. Shorter under test.
#[cfg(not(test))]
pub(in crate::worker) const TASK_STOP_DEADLINE: Duration = Duration::from_secs(15);
#[cfg(test)]
pub(in crate::worker) const TASK_STOP_DEADLINE: Duration = Duration::from_secs(3);
const _: () = assert!(TASK_STOP_DEADLINE.as_millis() * 2 <= RESPONSE_TIMEOUT.as_millis());

/// The least of the deadline a Stop is sent with. With less left it is not
/// sent at all: an owner call cut off by the deadline after the owner applied
/// it would be answered as if nothing had been sent.
#[cfg(not(test))]
pub(in crate::worker) const STOP_SEND_MARGIN: Duration = Duration::from_secs(2);
#[cfg(test)]
pub(in crate::worker) const STOP_SEND_MARGIN: Duration = Duration::from_millis(500);

/// Task Stops waiting for their lane beside the one it is running; one more
/// is answered `ai_worker_saturated` at once.
pub(in crate::worker) const TASK_STOP_QUEUE_CAPACITY: usize = 4;

/// How many times a Stop the owner refused for its fence (the Run started,
/// or moved to a new attempt, between the admission read and the Stop) reads
/// the admission again, at once, before answering retryable.
const FENCE_ROUNDS: usize = 3;

/// A `stop_task` the writer has checked: what the job needs to ask the
/// execution owner. The writer's prepared proof already reached `workspace`.
struct CheckedStop {
    /// The workspace the task's submission names, where its Run's record is.
    workspace: WorkspaceId,
    key: BotSubmissionKey,
    run: RunId,
    delegate: Arc<dyn BotDelegatePort>,
}

/// One `stop_task`, off the writer on its own lane, answered to its caller
/// directly. Holds no writer state.
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

    fn run_key(&self) -> BotRunKey {
        BotRunKey {
            owner: self.key.owner,
            bot: self.key.bot,
            run: self.run,
        }
    }

    fn stop(&self, deadline: Instant) -> ApiResult<BotResponse> {
        for _ in 0..FENCE_ROUNDS {
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
                return Ok(self.answer(BotTaskStopOutcome::AlreadySettled));
            }
            let within = Self::within(deadline)?;
            if within < STOP_SEND_MARGIN {
                return Err(unavailable());
            }
            match self.delegate.execute_within(
                DelegateCommand::Stop {
                    key: self.run_key(),
                    fence: admission.fence,
                },
                within,
            ) {
                Ok(BotReply::Applied) => return Ok(self.answer(self.applied(deadline))),
                Ok(_) => return Err(unavailable()),
                // The Run started, or moved on, since the read: read again.
                Err(refused)
                    if matches!(refused.code.as_str(), "bot_fenced" | "bot_fence_required") => {}
                // Delivered: the owner recorded the person's cancellation
                // and has not yet settled the Run. The caller asks again.
                Err(refused) if settling(&refused) => {
                    return Ok(self.answer(BotTaskStopOutcome::Stopping));
                }
                Err(refused) => return Err(owner_failure(&refused)),
            }
        }
        Err(unavailable())
    }

    /// What an applied Stop did: the Run's status says whether it is the
    /// person's cancellation (`stopped`) or a Run that ended by itself
    /// between the admission read and the Stop (`already_settled`: the
    /// owner's Stop of an ended Run only retires it). A status that cannot be
    /// read in time leaves `stopped`: the Stop was applied either way.
    fn applied(&self, deadline: Instant) -> BotTaskStopOutcome {
        let Ok(within) = Self::within(deadline) else {
            return BotTaskStopOutcome::Stopped;
        };
        match self.delegate.execute_within(
            DelegateCommand::Read {
                key: self.run_key(),
                window: TimelineQuery {
                    from: 0,
                    len: 1,
                    workspace: Some(self.workspace),
                },
            },
            within,
        ) {
            Ok(BotReply::Run { view })
                if view.header.run == self.run
                    && view.header.status.is_terminal()
                    && view.header.status != RunStatus::Cancelled =>
            {
                BotTaskStopOutcome::AlreadySettled
            }
            _ => BotTaskStopOutcome::Stopped,
        }
    }
}

/// The owner's answers that mean the Stop was delivered and the Run is
/// recorded as the person's cancellation, its settling still owed:
/// `bot_stop_unconfirmed` (the process's end not yet proven) and
/// `bot_native_effects_pending` (native effects to drain before it
/// retires), both from `Orchestrator::bot_stop` after the Cancel is applied;
/// and `bot_delivery_pending`, which the owner's Stop does not answer today
/// (only `AcknowledgeDelivery` does), listed so it would never read as
/// silence.
fn settling(refused: &koru_delegate_api::Failure) -> bool {
    matches!(
        refused.code.as_str(),
        "bot_stop_unconfirmed" | "bot_native_effects_pending" | "bot_delivery_pending"
    )
}

/// The execution owner's other refusals, in the Bots API's codes. The AI
/// owner has already checked the caller owns this Bot and that the request
/// is this Bot's API task, so a Run the owner does not hold (`unknown_run`)
/// or binds differently (`bot_owner_mismatch`) is `not_found`; anything else
/// is `bot_unavailable`, retryable: the same Stop sent again reads the
/// owner's state afresh and answers what it finds.
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
    /// operations with no conversation attempt, or `not_found`; then the
    /// workspace its Run is recorded in, from the same prepared proof
    /// (`start` requires the Bot's home, which never changes, so it is the
    /// workspace preparation inspected; any other is `wrong_owner`). No
    /// Store call is made off the writer for a Stop. A Stop only
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
        let workspace = receipt.intent.submission.workspace;
        proof.authority.workspace(workspace).map_err(bot_failure)?;
        let delegate = self.bot_delegate.clone().ok_or_else(|| {
            failure(
                "bot_unavailable",
                "The Bot's execution owner is not available.",
                true,
            )
        })?;
        Ok(Ok(CheckedStop {
            workspace,
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

#[cfg(test)]
impl BotTaskStopJob {
    /// A Stop of a task of nobody in particular, for the lane tests.
    pub(in crate::worker) fn fixture(
        delegate: Arc<dyn BotDelegatePort>,
        deadline: Instant,
        reply: SyncSender<ApiResult<BotResponse>>,
    ) -> Self {
        Self {
            stop: CheckedStop {
                workspace: WorkspaceId::new(),
                key: BotSubmissionKey {
                    owner: koru_delegate_core::BotOwner {
                        user_scope: UserScopeId::new(),
                        device: DeviceId::from_uuid(Uuid::new_v4()),
                    },
                    bot: BotId::mint(),
                    request: BotRequestId::mint(),
                },
                run: RunId::mint(),
                delegate,
            },
            deadline,
            reply,
        }
    }
}
