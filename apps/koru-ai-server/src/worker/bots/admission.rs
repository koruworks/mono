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
    /// The task a continuation continues was refused by its owner behind
    /// its fence (its receipt is `rejected`): the AI side's own durable
    /// proof that it never ran, so its owner is not asked whether it
    /// settled. A Retry of a reply whose task the owner refused as stale is
    /// such a continuation.
    previous_never_admitted: bool,
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
                            && self.previous.is_none()
                            && !self.previous_never_admitted)
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
        let prior = match &receipt {
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
                }),
            _ => None,
        };
        // A prior task its owner refused behind its fence never ran: the
        // owner holds nothing of it to ask about (it would answer Missing).
        let previous_never_admitted = prior
            .as_ref()
            .is_some_and(|r| r.stage == koru_ai_core::bots::BotReceiptStage::Rejected);
        let previous = prior.filter(|_| !previous_never_admitted).map(|r| key(&r));
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
            previous_never_admitted,
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
        let (owner, bot, request) = (result.owner, result.bot, result.request);
        match &result.result {
            Ok(_) => self.bot_recovery_retry.answered(Instant::now()),
            Err(error) if error.retryable => self.bot_recovery_retry.away = true,
            Err(_) => {}
        }
        let answered = result.result.is_ok();
        if answered
            || matches!(
                result.action,
                Action::Submit | Action::Pause(_) | Action::Fence { .. }
            )
        {
            self.request_bot_boot_refresh();
        }
        let refusal = result.result.as_ref().err().map(|error| error.code.clone());
        // A pause or resume being delivered: what its owner answered, for the
        // Bot read's `pending_change`.
        if let Action::Pause(_) = &result.action {
            self.bot_delivery_answers
                .record(bot, request, refusal.as_deref());
        }
        match self.apply_bot_coordination(result) {
            // The owner definitely refused the pause or resume as conflicting
            // and the same change was renewed under a request the AI owner
            // made itself (`settle_refused_bot_pause`). It is still owed: the
            // read names the new request with the answer that renewed it,
            // and it is offered on the backed-off recovery pass, never at
            // once, so an owner that keeps refusing is asked at most once a
            // pass (250 ms doubling to 32 s), not in a loop.
            Some(renewed) => {
                self.bot_delivery_answers.record(bot, request, None);
                self.bot_delivery_answers
                    .record(bot, renewed, refusal.as_deref());
                self.bot_recovery_retry.held.insert((bot, renewed));
                self.bot_recovery_retry.failed(Instant::now());
            }
            None => self.note_bot_coordination(owner, bot, request),
        }
        // The owner answered for this Bot (its owed pause acknowledged, among
        // others): an operation of it parked for the owner's silence takes up
        // its work again (P1.7).
        if answered {
            self.owner_answered_bot_lifecycle(bot);
        }
        self.recover_bot_operations();
    }
    /// A receipt its owner did not settle this time (unreachable, refusing,
    /// or an answer that decides nothing yet) and that nothing queued again
    /// stays owed: the recovery pass runs again after a backoff, so a pause
    /// left by a reversed operation, a refusal waiting for its fence, or a
    /// receipt a restart found is offered again once the owner can answer.
    /// A settled one lets the backoff start over.
    fn note_bot_coordination(&mut self, owner: BotOwner, bot: BotId, request: BotRequestId) {
        if self.bot_coordination.contains(&(bot, request)) {
            return;
        }
        let owed = matches!(
            self.repository.bot_admission_receipt(owner, bot, request),
            Ok(Some(receipt)) if receipt.operation().status == BotOperationStatus::Pending
        );
        if owed {
            self.bot_recovery_retry.failed(Instant::now());
        } else {
            self.bot_recovery_retry.settled();
            self.bot_delivery_answers.record(bot, request, None);
        }
    }
    /// The Bot's change still being delivered to its execution owner, for its
    /// read: the oldest pending pause, resume or access change (an indexed
    /// read), with the last failed attempt's code this worker saw. `None`
    /// when nothing is pending.
    pub(super) fn bot_pending_change(&self, bot: BotId) -> ApiResult<Option<BotPendingChange>> {
        Ok(self
            .repository
            .bot_pending_change(bot)
            .map_err(bot_failure)?
            .map(|(request_id, kind, since_ms)| BotPendingChange {
                request_id,
                kind,
                since_ms,
                last_answer: self.bot_delivery_answers.of(bot, request_id),
            }))
    }
    /// The backed-off recovery pass is due: walk the pending receipts again.
    pub(in crate::worker) fn retry_bot_operations(&mut self) {
        self.bot_recovery_retry.at = None;
        // This pass offers the renewed pauses held for it.
        self.bot_recovery_retry.held.clear();
        self.bot_recovery_retry.last = Some(Instant::now());
        self.bot_recovery_after.get_or_insert(0);
        self.recover_bot_operations();
        // Operations parked because their owner did not answer ask it again.
        self.probe_parked_bot_lifecycle();
    }
    /// Apply an owner's answer to a receipt. `Some` names the pause or
    /// resume renewed under a new request when the owner definitely refused
    /// it as conflicting; the caller offers that one on the backed-off pass.
    fn apply_bot_coordination(&mut self, result: BotCoordinationResult) -> Option<BotRequestId> {
        let Ok(Some(bot)) = self.repository.bot(result.bot) else {
            return None;
        };
        if owner(&bot) != result.owner {
            return None;
        }
        let Ok(Some(receipt)) =
            self.repository
                .bot_admission_receipt(result.owner, result.bot, result.request)
        else {
            return None;
        };
        if receipt.operation().status != BotOperationStatus::Pending {
            return None;
        }
        // Lifecycle holds this task's conversation or Bot: its attempt, if
        // it has not ended, is stopped as the person's Stop before the
        // receipt is resolved, whichever path reconciled it (the operation's
        // own round, startup recovery, the task's own admission). A context
        // job still in flight then finds it ended and starts nothing (P1.7).
        if let BotAdmissionReceipt::Submission(r) = &receipt
            && held_by_lifecycle(&self.repository, r) == Some(true)
            && let Some(attempt) = unended_attempt(&self.repository, r)
        {
            let _ = self.stop_attempt(attempt);
        }
        // A Send still in its context window (its attempt live, no live task
        // yet) is submitted by its own path once its live task exists
        // (`start_bot_chat_context`), never by recovery or a client's
        // reconcile: a Run admitted with nothing yet driving it could
        // outlive an operation that captures its Bot.
        if matches!(
            (&result.action, &result.result),
            (Action::Reconcile, Ok(BotReply::Missing))
        ) && let BotAdmissionReceipt::Submission(r) = &receipt
            && r.outcome.is_none()
            && let Some(attempt) = unended_attempt(&self.repository, r)
            && !self.bot_chats.contains_key(&attempt)
        {
            return None;
        }
        let renewing = matches!((&result.action, &result.result), (Action::Pause(_), Err(_)));
        let now = Timestamp::now().millis();
        let next = apply_result(&mut self.repository, result, &receipt, now);
        match next {
            Ok(Some((BotAdmissionReceipt::Pause(renewed), _))) if renewing => {
                Some(renewed.intent.barrier.request)
            }
            Ok(Some((receipt, action))) => {
                self.queue_bot_receipt(&bot, receipt, action);
                None
            }
            _ => None,
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
            if self.bot_recovery_retry.held.contains(&(id, request)) {
                continue;
            }
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
    // What a submission its owner does not hold may do: the one decision
    // that can submit, taken on the writer from the durable state.
    let missing = match (&result.action, &result.result, receipt) {
        (Action::Reconcile, Ok(BotReply::Missing), BotAdmissionReceipt::Submission(r)) => {
            Some(missing_submission(repository, result.bot, r))
        }
        _ => None,
    };
    let stale = matches!(
        (&result.action, &result.result),
        (Action::Submit, Err(error)) if error.code == "bot_stale"
    );
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
        // submitted, and falls to the refusal fence below (P1.7). Neither is
        // one of a Bot the AI side holds paused or whose pause is pending.
        (Action::Reconcile, Ok(BotReply::Missing), BotAdmissionReceipt::Submission(_))
            if missing == Some(Missing::Submit) =>
        {
            Ok(Some((receipt.clone(), Some(Action::Submit))))
        }
        // A hold or a Bot state that could not be read decides nothing: the
        // receipt is neither submitted nor fenced, and stays owed.
        (Action::Reconcile, Ok(BotReply::Missing), BotAdmissionReceipt::Submission(_))
            if missing == Some(Missing::Defer) =>
        {
            Ok(None)
        }
        (Action::Reconcile, Ok(BotReply::Missing), BotAdmissionReceipt::Submission(_))
        | (Action::Submit, Err(_), BotAdmissionReceipt::Submission(_)) => {
            let r = repository.prepare_bot_refusal_fence(
                result.owner,
                result.bot,
                result.request,
                // A Submit the owner refused because the Bot's revision
                // moved after the task was prepared (`bot_stale`) keeps that
                // reason: nothing ran, and nothing about the task itself is
                // wrong, so it may be sent again once at the current
                // revision (`chat/resend.rs`). Every other refusal is
                // `rejected`.
                if stale {
                    BotAdmissionRefusal::Stale
                } else {
                    BotAdmissionRefusal::Rejected
                },
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
        // An owed pause or resume the owner definitely refused never lands
        // as it is (`settle_refused_bot_pause`): with another barrier at its
        // epoch or a newer one (`bot_conflict`, `bot_stale`) the AI side's
        // state is offered again at the next revision; to a host that will
        // never take a barrier for this owner (`bot_owner_mismatch`,
        // `bot_invalid`) it is settled rejected, and the Bot stays as AI
        // recorded it. Anything else (unreachable, no ledger yet) stays
        // pending and is offered again.
        (Action::Pause(_), Err(error), BotAdmissionReceipt::Pause(_))
            if matches!(
                error.code.as_str(),
                "bot_conflict" | "bot_stale" | "bot_owner_mismatch" | "bot_invalid"
            ) =>
        {
            let renew = matches!(error.code.as_str(), "bot_conflict" | "bot_stale");
            Ok(repository
                .settle_refused_bot_pause(result.owner, result.bot, result.request, renew, now)?
                .map(|next| (BotAdmissionReceipt::Pause(next), None)))
        }
        _ => Ok(None), // Uncertain/unknown replies keep the durable intent and folder guard.
    }
}
/// What a Reconcile answering Missing lets a submission do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Missing {
    /// Offer it to its owner.
    Submit,
    /// Refuse it through the refusal fence.
    Refuse,
    /// Decide later: its hold or its Bot could not be read.
    Defer,
}
/// A submission is offered only while nothing holds it back: no outcome yet
/// (a refusal fence, once written, is final), its AI attempt still live, no
/// lifecycle hold on its conversation or Bot, and the AI side admitting work
/// of the Bot (active, not paused, no pause or resume still pending).
fn missing_submission(repository: &Repository, bot: BotId, r: &BotSubmissionReceipt) -> Missing {
    let live =
        r.intent.submission.provenance.ai_attempt_id.is_none() || ai_submission_live(repository, r);
    match (
        held_by_lifecycle(repository, r),
        repository.bot_admitting(bot),
    ) {
        (None, _) | (_, Err(_)) => Missing::Defer,
        (Some(false), Ok(true)) if live && r.outcome.is_none() => Missing::Submit,
        _ => Missing::Refuse,
    }
}
/// Whether lifecycle holds the conversation a submission names, or its whole
/// Bot: an operation moving or parked, or either in Trash. `None` when the
/// hold cannot be read, which decides neither way.
fn held_by_lifecycle(repository: &Repository, receipt: &BotSubmissionReceipt) -> Option<bool> {
    let Some(chat) = ChatId::parse(&receipt.intent.submission.provenance.chat_id.to_string())
    else {
        // A receipt naming no conversation AI knows is never offered.
        return Some(true);
    };
    repository
        .bot_chat_hold(chat)
        .ok()
        .map(|hold| hold.is_some())
}
/// When the recovery pass runs again after a receipt stayed owed: 250 ms,
/// doubling to 32 s, until one settles. Nothing here is durable: a restart
/// runs the pass anyway.
#[derive(Default)]
pub(in crate::worker) struct AdmissionRetry {
    at: Option<Instant>,
    round: u32,
    /// When the last pass began: a pass brought forward never starts
    /// sooner than a second after it, whoever asks.
    last: Option<Instant>,
    /// The owner was unreachable when it was last asked: the first answer
    /// it gives again brings the pass forward, once.
    away: bool,
    /// Pauses and resumes renewed after their owner refused them as
    /// conflicting, waiting for the next pass: a walk already under way
    /// passes them by, so each is offered once a pass at most.
    held: HashSet<(BotId, BotRequestId)>,
}
impl AdmissionRetry {
    fn failed(&mut self, now: Instant) {
        if self.at.is_none() {
            self.at = Some(now + Duration::from_millis(250) * (1u32 << self.round.min(7)));
            self.round = (self.round + 1).min(7);
        }
    }
    fn settled(&mut self) {
        if self.at.is_none() {
            self.round = 0;
        }
    }
    /// Something is owed to an owner that did not answer (an operation
    /// parked for its silence): the pass runs again after the backoff, and
    /// sooner once the owner answers anything.
    pub(in crate::worker) fn owner_away(&mut self, now: Instant) {
        self.away = true;
        self.failed(now);
    }
    /// Bring the next pass forward to now, but never sooner than a second
    /// after the last one began, so a client's retries or a busy owner
    /// cannot make it run faster than that.
    pub(in crate::worker) fn soon(&mut self, now: Instant) {
        let earliest = self
            .last
            .map_or(now, |last| last + Duration::from_secs(1))
            .max(now);
        self.at = Some(self.at.map_or(earliest, |at| at.min(earliest)));
    }
    /// The owner answered something: whatever is owed is offered again soon,
    /// rather than at the end of its backoff (an owner back after an outage).
    pub(in crate::worker) fn answered(&mut self, now: Instant) {
        if self.away && self.at.is_some() {
            self.away = false;
            self.soon(now);
        }
    }
    pub(in crate::worker) fn due(&self, now: Instant) -> bool {
        self.at.is_some_and(|at| at <= now)
    }
}
/// The code of the last failed or refused delivery of each pending pause,
/// resume or access change, as the writer saw it since it started: what the
/// Bot read reports as its `pending_change.last_answer`. An answer that is
/// not a failure clears it. Nothing here is durable; at most one entry is
/// kept per change still being delivered.
#[derive(Default)]
pub(in crate::worker) struct DeliveryAnswers(HashMap<(BotId, BotRequestId), String>);
impl DeliveryAnswers {
    pub(in crate::worker) fn record(
        &mut self,
        bot: BotId,
        request: BotRequestId,
        failure: Option<&str>,
    ) {
        match failure {
            Some(code) => {
                self.0.insert((bot, request), code.to_owned());
            }
            None => {
                self.0.remove(&(bot, request));
            }
        }
    }
    fn of(&self, bot: BotId, request: BotRequestId) -> Option<String> {
        self.0.get(&(bot, request)).cloned()
    }
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
