// SPDX-License-Identifier: MPL-2.0
//! Durable AI intent, with Store/IPC work confined to the existing bounded pool.
use super::*;
use koru_ai_core::{ProfileId, ReasoningLevel};
use koru_delegate_api::bots::{BotCommand as DelegateCommand, BotDelegatePort, BotReply};
use koru_delegate_core::{BotAdmissionBarrier, BotAdmissionState, BotSubmissionKey, RunId};

#[derive(Clone)]
pub(in crate::worker) enum Action {
    Reconcile,
    Submit,
    Pause(BotAdmissionBarrier),
    Fence {
        round: u32,
        barrier: BotAdmissionBarrier,
    },
    FencedReconcile {
        round: u32,
        barrier: BotAdmissionBarrier,
    },
}
pub(in crate::worker) struct BotCoordinationJob {
    owner: BotOwner,
    bot: BotId,
    request: BotRequestId,
    workspace: WorkspaceId,
    authority: Arc<dyn BotAuthority>,
    delegate: Arc<dyn BotDelegatePort>,
    action: Action,
    receipt: BotAdmissionReceipt,
    previous: Option<BotSubmissionKey>,
    ai_live: bool,
}
pub(in crate::worker) struct BotCoordinationResult {
    owner: BotOwner,
    bot: BotId,
    request: BotRequestId,
    action: Action,
    result: Result<BotReply, koru_delegate_api::Failure>,
}
fn key(r: &BotSubmissionReceipt) -> BotSubmissionKey {
    let p = &r.intent.submission.provenance;
    BotSubmissionKey {
        owner: p.owner,
        bot: p.bot,
        request: p.submission,
    }
}
impl BotCoordinationJob {
    pub(in crate::worker) fn run(self) -> BotCoordinationResult {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let checked = self
                .authority
                .inspect(&[self.workspace], &[])
                .map_err(|_| delegate_unavailable())?;
            if checked.owner != self.owner || checked.workspace(self.workspace).is_err() {
                return Err(delegate_unavailable());
            }
            let command = match (&self.action, &self.receipt) {
                (Action::Submit, BotAdmissionReceipt::Submission(r)) => {
                    if (r.intent.submission.provenance.ai_attempt_id.is_some() && !self.ai_live)
                        || (r.intent.submission.provenance.continued_from.is_some()
                            && self.previous.is_none())
                    {
                        return Err(delegate_unavailable());
                    }
                    if let Some(previous) = &self.previous {
                        match self.delegate.execute(DelegateCommand::Reconcile {
                            key: previous.clone(),
                        })? {
                            BotReply::Admission { admission }
                                if admission.key == *previous
                                    && Some(admission.run)
                                        == r.intent.submission.provenance.continued_from
                                    && matches!(
                                        admission.state,
                                        BotAdmissionState::Settled | BotAdmissionState::Refused
                                    ) => {}
                            _ => {
                                return Err(koru_delegate_api::Failure {
                                    code: "bot_previous_active".into(),
                                    message: "Previous task has not settled.".into(),
                                    retryable: false,
                                    details: Default::default(),
                                });
                            }
                        }
                    }
                    DelegateCommand::Submit {
                        submission: r.intent.submission.clone(),
                    }
                }
                (
                    Action::Reconcile | Action::FencedReconcile { .. },
                    BotAdmissionReceipt::Submission(r),
                ) => DelegateCommand::Reconcile { key: key(r) },
                (Action::Pause(barrier) | Action::Fence { barrier, .. }, _) => {
                    DelegateCommand::Barrier {
                        barrier: barrier.clone(),
                    }
                }
                _ => return Err(delegate_unavailable()),
            };
            self.delegate.execute(command)
        }))
        .unwrap_or_else(|_| Err(delegate_unavailable()));
        BotCoordinationResult {
            owner: self.owner,
            bot: self.bot,
            request: self.request,
            action: self.action,
            result,
        }
    }
}
fn delegate_unavailable() -> koru_delegate_api::Failure {
    koru_delegate_api::Failure {
        code: "bot_unavailable".into(),
        message: "Bot coordination is unavailable.".into(),
        retryable: true,
        details: Default::default(),
    }
}
fn owner(bot: &Bot) -> BotOwner {
    BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    }
}
fn request_identity(command: &BotCommand) -> Option<(BotId, BotRequestId)> {
    match command {
        BotCommand::Start {
            bot_id, request_id, ..
        }
        | BotCommand::Continue {
            bot_id, request_id, ..
        }
        | BotCommand::Pause {
            bot_id, request_id, ..
        }
        | BotCommand::Resume {
            bot_id, request_id, ..
        }
        | BotCommand::ReadOperation { bot_id, request_id }
        | BotCommand::ReconcileOperation { bot_id, request_id } => Some((*bot_id, *request_id)),
        _ => None,
    }
}
fn recovery_candidate(
    repository: &Repository,
    id: BotId,
    scope: UserScopeId,
    request: BotRequestId,
) -> Option<(Bot, BotAdmissionReceipt)> {
    let bot = repository.bot(id).ok().flatten()?;
    if bot.user_scope != scope {
        return None;
    }
    let receipt = repository
        .bot_admission_receipt(owner(&bot), id, request)
        .ok()
        .flatten()?;
    (receipt.operation().status == BotOperationStatus::Pending).then_some((bot, receipt))
}
impl WorkerState {
    pub(in crate::worker) fn bot_admission_command(
        &mut self,
        context: &CommandContext,
        command: &BotCommand,
    ) -> Option<ApiResult<BotResponse>> {
        let (bot_id, request_id) = request_identity(command)?;
        Some((|| {
            authorize(context, AiScope::ManageBots)?;
            let proof = context
                .prepared
                .as_ref()
                .ok_or_else(|| bot_failure(BotError::Unavailable))?;
            let bot = self
                .repository
                .bot(bot_id)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?;
            validate_owner(&proof.authority, &bot)?;
            let owner = owner(&bot);
            let fingerprint = blake3::hash(
                &serde_json::to_vec(command).map_err(|_| bot_failure(BotError::Invalid))?,
            )
            .to_hex()
            .to_string();
            if let Some(receipt) = self
                .repository
                .bot_admission_receipt(owner, bot_id, request_id)
                .map_err(bot_failure)?
            {
                let stored = match &receipt {
                    BotAdmissionReceipt::Submission(r) => &r.fingerprint,
                    BotAdmissionReceipt::Pause(r) => &r.fingerprint,
                };
                if !matches!(
                    command,
                    BotCommand::ReadOperation { .. } | BotCommand::ReconcileOperation { .. }
                ) && stored != &fingerprint
                {
                    return Err(bot_failure(BotError::Conflict));
                }
                if !matches!(command, BotCommand::ReadOperation { .. }) {
                    self.queue_bot_receipt(&bot, receipt.clone(), None);
                }
                return Ok(BotResponse::Operation {
                    operation: receipt.operation(),
                });
            }
            let now = Timestamp::now().millis();
            let receipt = match command {
                BotCommand::Start {
                    chat_id,
                    expected_revision,
                    destination,
                    task,
                    ..
                }
                | BotCommand::Continue {
                    chat_id,
                    expected_revision,
                    destination,
                    task,
                    ..
                } => {
                    if self.bot_delegate.is_none() {
                        return Err(bot_failure(BotError::Unavailable));
                    }
                    proof.runtime.clone()?;
                    task.validate(*destination).map_err(bot_failure)?;
                    if *destination != bot.home_workspace {
                        return Err(bot_failure(BotError::Forbidden));
                    }
                    let previous = match command {
                        BotCommand::Continue { previous_run, .. } => Some(*previous_run),
                        _ => None,
                    };
                    if let Some(run) = previous {
                        let prior = self
                            .repository
                            .bot_submission_for_run(owner, bot.id, run)
                            .map_err(bot_failure)?
                            .ok_or_else(not_found)?;
                        if prior.intent.submission.provenance.chat_id != chat_id.as_uuid() {
                            return Err(bot_failure(BotError::Forbidden));
                        }
                    }
                    let intent = make_intent(
                        &bot,
                        request_id,
                        *chat_id,
                        *destination,
                        task,
                        previous,
                        &proof.pins,
                        proof
                            .authority
                            .workspace(*destination)
                            .map_err(bot_failure)?,
                    )?;
                    BotAdmissionReceipt::Submission(
                        self.repository
                            .prepare_bot_submission(&intent, &fingerprint, *expected_revision, now)
                            .map_err(bot_failure)?,
                    )
                }
                BotCommand::Pause {
                    expected_revision, ..
                }
                | BotCommand::Resume {
                    expected_revision, ..
                } => BotAdmissionReceipt::Pause(
                    self.repository
                        .prepare_bot_pause(
                            owner,
                            bot_id,
                            request_id,
                            *expected_revision,
                            matches!(command, BotCommand::Pause { .. }),
                            &fingerprint,
                            now,
                        )
                        .map_err(bot_failure)?,
                ),
                _ => return Err(not_found()),
            };
            self.queue_bot_receipt(&bot, receipt.clone(), None);
            Ok(BotResponse::Operation {
                operation: receipt.operation(),
            })
        })())
    }
    pub(super) fn queue_bot_receipt(
        &mut self,
        bot: &Bot,
        receipt: BotAdmissionReceipt,
        override_action: Option<Action>,
    ) -> bool {
        let operation = receipt.operation();
        if operation.status != BotOperationStatus::Pending
            || self
                .bot_coordination
                .contains(&(bot.id, operation.request_id))
        {
            return false;
        }
        let (Some(authority), Some(delegate), Some(pool)) = (
            &self.bot_authority,
            &self.bot_delegate,
            &self.provider_admin,
        ) else {
            return false;
        };
        let action = override_action.unwrap_or_else(|| match &receipt {
            BotAdmissionReceipt::Pause(r) => Action::Pause(r.intent.barrier.clone()),
            BotAdmissionReceipt::Submission(r) => match &r.outcome {
                Some(BotExecutionOutcome::RefusalFence {
                    round,
                    barrier,
                    acknowledged: true,
                    ..
                }) => Action::FencedReconcile {
                    round: *round,
                    barrier: barrier.clone(),
                },
                Some(BotExecutionOutcome::RefusalFence { round, barrier, .. }) => Action::Fence {
                    round: *round,
                    barrier: barrier.clone(),
                },
                _ => Action::Reconcile,
            },
        });
        let previous = match &receipt {
            BotAdmissionReceipt::Submission(r) => r
                .intent
                .submission
                .provenance
                .continued_from
                .and_then(|run| {
                    self.repository
                        .bot_submission_for_run(owner(bot), bot.id, run)
                        .ok()
                        .flatten()
                })
                .map(|r| key(&r)),
            _ => None,
        };
        let ai_live = match &receipt {
            BotAdmissionReceipt::Submission(r) => ai_submission_live(&self.repository, r),
            _ => false,
        };
        let job = BotCoordinationJob {
            owner: owner(bot),
            bot: bot.id,
            request: operation.request_id,
            workspace: bot.home_workspace,
            authority: authority.clone(),
            delegate: delegate.clone(),
            action,
            receipt,
            previous,
            ai_live,
        };
        if pool
            .try_submit(ProviderAdminJob::BotCoordinate(job))
            .is_err()
        {
            self.bot_recovery_after = Some(0);
            return false;
        }
        self.bot_coordination.insert((bot.id, operation.request_id));
        true
    }
    pub(in crate::worker) fn complete_bot_coordination(&mut self, result: BotCoordinationResult) {
        self.bot_coordination.remove(&(result.bot, result.request));
        // A newly effective pause may unblock an older submission's refusal
        // fence. Give durable pending receipts one fresh, finite recovery pass.
        if matches!((&result.action,&result.result),(Action::Pause(expected),Ok(BotReply::Barrier{barrier})) if expected==barrier)
        {
            self.bot_recovery_after = Some(0);
        }
        self.apply_bot_coordination(result);
        self.recover_bot_operations();
    }
    fn apply_bot_coordination(&mut self, result: BotCoordinationResult) {
        let Ok(Some(bot)) = self.repository.bot(result.bot) else {
            return;
        };
        if owner(&bot) != result.owner {
            return;
        }
        let Ok(Some(receipt)) =
            self.repository
                .bot_admission_receipt(result.owner, result.bot, result.request)
        else {
            return;
        };
        if receipt.operation().status != BotOperationStatus::Pending {
            return;
        }
        // Lifecycle holds this task's conversation or Bot: its attempt, if
        // it has not ended, is stopped as the person's Stop before the
        // receipt is resolved, whichever path reconciled it (the operation's
        // own round, startup recovery, the task's own admission). A context
        // job still in flight then finds it ended and starts nothing (P1.7).
        if let BotAdmissionReceipt::Submission(r) = &receipt
            && held_by_lifecycle(&self.repository, r)
            && let Some(attempt) = unended_attempt(&self.repository, r)
        {
            let _ = self.stop_attempt(attempt);
        }
        let now = Timestamp::now().millis();
        let next = apply_result(&mut self.repository, result, &receipt, now);
        if let Ok(Some((receipt, action))) = next {
            self.queue_bot_receipt(&bot, receipt, action);
        }
    }
    /// Finite startup receipt walk. Each page is bounded; continuation follows a
    /// worker completion, never a document timer or UI read.
    pub(in crate::worker) fn recover_bot_operations(&mut self) {
        let Some(after) = self.bot_recovery_after else {
            return;
        };
        if self.bot_delegate.is_none()
            || self.bot_authority.is_none()
            || self.provider_admin.is_none()
        {
            self.bot_recovery_after = None;
            return;
        }
        if !self.bot_coordination.is_empty() {
            return;
        }
        let Ok(rows) = self.repository.pending_bot_operations(after, 16) else {
            self.bot_recovery_after = None;
            return;
        };
        if rows.is_empty() {
            self.bot_recovery_after = None;
            return;
        }
        for (row, id, scope, request) in rows {
            let previous = self.bot_recovery_after;
            self.bot_recovery_after = Some(row);
            let Some((bot, receipt)) = recovery_candidate(&self.repository, id, scope, request)
            else {
                continue;
            };
            if self.queue_bot_receipt(&bot, receipt, None) {
                return;
            }
            self.bot_recovery_after = previous;
            return; // Pool full: its next completion retries this same row.
        }
        // Schedule another bounded page without recursively occupying the writer.
        let _ = self.output.try_send(Envelope::BotRecover);
    }
}
fn apply_result(
    repository: &mut Repository,
    result: BotCoordinationResult,
    receipt: &BotAdmissionReceipt,
    now: i64,
) -> Result<Option<(BotAdmissionReceipt, Option<Action>)>, BotError> {
    match (&result.action, result.result, receipt) {
        (
            Action::Pause(expected),
            Ok(BotReply::Barrier { barrier }),
            BotAdmissionReceipt::Pause(_),
        ) if *expected == barrier => {
            repository.acknowledge_bot_pause(
                result.owner,
                result.bot,
                result.request,
                &barrier,
                now,
            )?;
            Ok(None)
        }
        (
            Action::Fence {
                round,
                barrier: expected,
            },
            Ok(BotReply::Barrier { barrier }),
            BotAdmissionReceipt::Submission(_),
        ) if *expected == barrier => {
            let r = repository.acknowledge_bot_refusal_fence(
                result.owner,
                result.bot,
                result.request,
                *round,
                &barrier,
                now,
            )?;
            Ok(Some((BotAdmissionReceipt::Submission(r), None)))
        }
        (
            Action::Reconcile | Action::Submit | Action::FencedReconcile { .. },
            Ok(BotReply::Admission { admission }),
            BotAdmissionReceipt::Submission(_),
        ) => {
            repository.acknowledge_bot_submission(
                result.owner,
                result.bot,
                result.request,
                &admission,
                now,
            )?;
            Ok(None)
        }
        (
            Action::FencedReconcile { round, barrier },
            Ok(BotReply::Missing),
            BotAdmissionReceipt::Submission(_),
        ) => {
            repository.reject_bot_submission_after_fence(
                result.owner,
                result.bot,
                result.request,
                *round,
                barrier,
                now,
            )?;
            Ok(None)
        }
        // Lifecycle refuses work its owner had not admitted when the
        // operation was captured, and never admits it: a submission of a
        // conversation (or a Bot) that an Archive or Trash holds is not
        // submitted, and falls to the refusal fence below (P1.7).
        (Action::Reconcile, Ok(BotReply::Missing), BotAdmissionReceipt::Submission(r))
            if (r.intent.submission.provenance.ai_attempt_id.is_none()
                || ai_submission_live(repository, r))
                && r.outcome.is_none()
                && !held_by_lifecycle(repository, r) =>
        {
            Ok(Some((receipt.clone(), Some(Action::Submit))))
        }
        (Action::Reconcile, Ok(BotReply::Missing), BotAdmissionReceipt::Submission(_))
        | (Action::Submit, Err(_), BotAdmissionReceipt::Submission(_)) => {
            let r = repository.prepare_bot_refusal_fence(
                result.owner,
                result.bot,
                result.request,
                BotAdmissionRefusal::Rejected,
                false,
                now,
            )?;
            Ok(Some((BotAdmissionReceipt::Submission(r), None)))
        }
        (Action::Fence { .. }, Err(error), BotAdmissionReceipt::Submission(_))
            if error.code == "bot_stale" =>
        {
            // Renewal requires a genuinely newer AI revision. An unchanged
            // peer reporting stale cannot create an infinite revision loop.
            let r = repository.prepare_bot_refusal_fence(
                result.owner,
                result.bot,
                result.request,
                BotAdmissionRefusal::Stale,
                true,
                now,
            )?;
            Ok(Some((BotAdmissionReceipt::Submission(r), None)))
        }
        _ => Ok(None), // Uncertain/unknown replies keep the durable intent and folder guard.
    }
}
/// Whether lifecycle holds the conversation a submission names, or its whole
/// Bot: an operation moving or parked, or either in Trash. A hold that
/// cannot be read counts as held: refusing is the safe answer.
fn held_by_lifecycle(repository: &Repository, receipt: &BotSubmissionReceipt) -> bool {
    let Some(chat) = ChatId::parse(&receipt.intent.submission.provenance.chat_id.to_string())
    else {
        return true;
    };
    !matches!(repository.bot_chat_hold(chat), Ok(None))
}
/// The AI attempt a submission belongs to, while it has not ended.
pub(super) fn unended_attempt(
    repository: &Repository,
    receipt: &BotSubmissionReceipt,
) -> Option<InferenceAttemptId> {
    let id = receipt.intent.submission.provenance.ai_attempt_id?;
    let id = InferenceAttemptId::parse(&id.to_string())?;
    matches!(repository.attempt(id), Ok(Some(attempt)) if !attempt.status.is_terminal())
        .then_some(id)
}
fn ai_submission_live(repository: &Repository, receipt: &BotSubmissionReceipt) -> bool {
    let Some(id) = receipt.intent.submission.provenance.ai_attempt_id else {
        return false;
    };
    let Some(id) = InferenceAttemptId::parse(&id.to_string()) else {
        return false;
    };
    matches!(repository.attempt(id), Ok(Some(attempt)) if !attempt.status.is_terminal())
        && matches!(repository.bot_replay_state(id), Ok(state) if state.stop_reason.is_none())
        && matches!(repository.bot_submission_for_attempt(id), Ok(Some(current)) if current.intent == receipt.intent)
}
pub(super) fn make_intent(
    bot: &Bot,
    request: BotRequestId,
    chat: ChatId,
    destination: WorkspaceId,
    task: &BotTask,
    previous: Option<RunId>,
    pins: &[koru_delegate_core::RunContextItem],
    profile: ProfileId,
) -> ApiResult<BotSubmissionIntent> {
    use koru_delegate_core as d;
    use koru_session_protocol as s;
    let agent = koru_ai_provider_agent::agent_kind_of(&bot.config.model.provider)
        .ok_or_else(|| bot_failure(BotError::Invalid))?;
    let model: s::ModelName = bot
        .config
        .model
        .requested_model
        .parse()
        .map_err(|_| bot_failure(BotError::Invalid))?;
    let requested_model = d::ModelName::new(&bot.config.model.requested_model)
        .map_err(|_| bot_failure(BotError::Invalid))?;
    let reasoning = match bot.config.model.default_reasoning {
        ReasoningChoice::Default => None,
        ReasoningChoice::Off => Some(s::LaunchReasoning::Off),
        ReasoningChoice::On => Some(s::LaunchReasoning::On),
        ReasoningChoice::Level(level) => Some(match level {
            ReasoningLevel::Minimal => s::LaunchReasoning::Minimal,
            ReasoningLevel::Low => s::LaunchReasoning::Low,
            ReasoningLevel::Medium => s::LaunchReasoning::Medium,
            ReasoningLevel::High => s::LaunchReasoning::High,
            ReasoningLevel::XHigh => s::LaunchReasoning::XHigh,
            ReasoningLevel::Max => s::LaunchReasoning::Max,
        }),
    };
    let required = std::iter::once(&bot.config.role)
        .chain(task.documents.iter())
        .map(|p| (p.workspace, p.document))
        .collect::<std::collections::BTreeSet<_>>();
    let captured = pins
        .iter()
        .filter_map(|p| match p {
            d::RunContextItem::Document {
                workspace,
                document,
                ..
            } => Some((*workspace, *document)),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    if required != captured || pins.len() != captured.len() {
        return Err(bot_failure(BotError::Forbidden));
    }
    let intent = BotSubmissionIntent {
        version: 1,
        submission: d::BotSubmission {
            provenance: d::BotProvenance {
                owner: d::BotOwner {
                    user_scope: bot.user_scope,
                    device: bot.owner_device,
                },
                bot: bot.id,
                submission: request,
                chat_id: chat.as_uuid(),
                ai_attempt_id: None,
                assistant_message_id: None,
                config_revision: bot.revision,
                policy_revision: bot.policy_revision,
                continued_from: previous,
            },
            policy: d::CapturedBotPolicy {
                workspace: destination,
                destination,
                access: match bot.access {
                    BotAccess::Default => d::BotAccess::Default,
                    BotAccess::Workspace => d::BotAccess::Workspace,
                },
                selected_pages: bot.selected_pages.iter().map(|p| p.document).collect(),
            },
            request: d::DelegationRequest {
                run: RunId::mint(),
                requested_by: d::ActorRef::Person { profile },
                objective: d::Objective::new(&task.text)
                    .map_err(|_| bot_failure(BotError::Invalid))?,
                agent: agent.profile,
                requested_model: Some(requested_model),
                scope: d::WorkspaceScope::BotFolder {
                    folder: d::BotWorkingFolder::new(bot.config.working_folder.clone())
                        .map_err(|_| bot_failure(BotError::Invalid))?,
                },
                context: pins.to_vec(),
                grants: [destination].into_iter().collect(),
                executing_device: bot.owner_device,
                origin: d::RunOrigin::Manual,
            },
            workspace: destination,
            input: s::PendingInput {
                idempotency_key: s::IdempotencyKey::mint(),
                text: task.text.clone(),
                attachment_ids: vec![],
                references: vec![],
                mode: bot
                    .config
                    .mode
                    .as_deref()
                    .map(str::parse)
                    .transpose()
                    .map_err(|_| bot_failure(BotError::Invalid))?,
                fast: bot.config.fast,
                reasoning,
                skills: vec![],
                model: Some(model),
            },
        },
    };
    intent.validate().map_err(bot_failure)?;
    Ok(intent)
}
#[cfg(test)]
mod tests;
