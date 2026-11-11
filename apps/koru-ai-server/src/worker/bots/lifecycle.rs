// SPDX-License-Identifier: MPL-2.0
//! Bot lifecycle commands, the holds they place, and the Archive driver (P1.7).
//!
//! The journal is the durable state; the writer only schedules. Archive is
//! captured in one AI transaction (intent, members, the existing pause
//! receipt). The driver then moves it on in finite steps, each owner call a
//! job on the bounded provider-administration pool, each answer applied on
//! the writer before the next step:
//!
//! 1. `captured`: the pause receipt is acknowledged through the existing
//!    coordination, then the effects-off policy barrier is installed.
//! 2. `settling`: a person holding a Run's session parks the operation as
//!    `incomplete/person_holding` before anything is stopped (O27). Otherwise
//!    every live task is stopped as the person's Stop stops it, queued work is
//!    stopped at its owner, each stopped Run is reconciled until its owner
//!    says it settled, and the barrier must be effective (native effects
//!    drained).
//! 3. One transaction archives the Bot. Nothing is hidden or removed.
//!
//! A restart resumes from the journal's stage, never recaptures, and re-asks
//! every owner idempotently. An operation that cannot progress is parked
//! `incomplete` with its reason; the same request resumes it. Unarchive is a
//! Restore naming the Archive, applied in one transaction with no owner call.
//! Trash, Restore of a Trash and Purge refuse explicitly until their slices.
use super::*;
use koru_ai_sqlite::{BotHold, LifecycleCapture, LifecycleMemberDraft, LifecycleOperation};
use koru_ai_wire::bot_lifecycle_codes;
use koru_delegate_api::bots::{BotCommand as DelegateCommand, BotDelegatePort, BotReply};
use koru_delegate_core::{
    BotAdmissionState, BotPolicyBarrier, BotPolicyBarrierKey, BotPolicyBarrierState, BotRunKey,
    BotSubmissionKey, NativeDrainReason, RunId,
};

type OperationKey = (UserScopeId, BotRequestId);

/// Consecutive steps that made no progress before an operation is parked.
/// The waits double from 250 ms to 32 s, so this is a few minutes.
const PARK_AFTER: u32 = 12;
/// The most unsettled queued submissions one settling round stops.
const QUEUED_PAGE: u16 = 16;

/// What the writer drives. Nothing here is durable: the journal is.
pub(in crate::worker) struct LifecycleDriver {
    driving: HashMap<OperationKey, Drive>,
    /// Restart resume: the next journal page, `None` once every page is read.
    recovery: Option<Option<(i64, BotRequestId)>>,
}
impl Default for LifecycleDriver {
    fn default() -> Self {
        Self {
            driving: HashMap::new(),
            recovery: Some(None),
        }
    }
}

#[derive(Default)]
struct Drive {
    busy: bool,
    due: Option<Instant>,
    retries: u32,
    /// Runs whose sessions were read and found not held by a person.
    free: HashSet<RunId>,
    /// Attempts this operation already stopped as the person's Stop.
    stopped: HashSet<InferenceAttemptId>,
    /// Queued submissions whose owner Stop was applied.
    queued: HashSet<BotRequestId>,
    /// Every Run this operation stopped, reconciled before completion.
    runs: HashMap<RunId, BotSubmissionKey>,
}

/// One finite owner step, run off the writer.
pub(in crate::worker) struct LifecycleJob {
    key: OperationKey,
    owner: BotOwner,
    workspace: WorkspaceId,
    authority: Arc<dyn BotAuthority>,
    delegate: Arc<dyn BotDelegatePort>,
    step: Step,
}
enum Step {
    /// Install (idempotently) the effects-off policy barrier.
    Policy(BotPolicyBarrier),
    /// Read each live Run: does a person hold its session?
    Holding(Vec<(BotRunKey, WorkspaceId)>),
    /// Stop queued work that has no live conversation task.
    StopQueued(Vec<(BotRequestId, BotRunKey)>),
    /// Reconcile each stopped Run: has its owner settled it?
    Verify(Vec<BotSubmissionKey>),
}
pub(in crate::worker) struct LifecycleResult {
    key: OperationKey,
    outcome: Outcome,
}
enum Outcome {
    Policy(Result<BotPolicyBarrierState, koru_delegate_api::Failure>),
    Holding(Result<(Vec<RunId>, bool), koru_delegate_api::Failure>),
    Stopped(Vec<BotRequestId>),
    Verified(Result<bool, koru_delegate_api::Failure>),
}

fn unavailable() -> koru_delegate_api::Failure {
    koru_delegate_api::Failure::new("bot_unavailable", "Bot lifecycle owner unavailable.", true)
}

impl LifecycleJob {
    pub(in crate::worker) fn run(self) -> LifecycleResult {
        let key = self.key;
        let outcome = catch_unwind(AssertUnwindSafe(|| self.execute()))
            .unwrap_or_else(|_| Outcome::Verified(Err(unavailable())));
        LifecycleResult { key, outcome }
    }
    fn execute(self) -> Outcome {
        // The owner's authority is read again before every owner call.
        let checked = self
            .authority
            .inspect(&[self.workspace], &[])
            .ok()
            .filter(|checked| {
                checked.owner == self.owner && checked.workspace(self.workspace).is_ok()
            });
        match self.step {
            Step::Policy(barrier) => {
                Outcome::Policy(checked.ok_or_else(unavailable).and_then(|_| {
                    match self.delegate.execute(DelegateCommand::SynchronizePolicy {
                        barrier: barrier.clone(),
                    })? {
                        BotReply::PolicyBarrier { receipt } if receipt.barrier == barrier => {
                            Ok(receipt.state)
                        }
                        _ => Err(unavailable()),
                    }
                }))
            }
            Step::Holding(runs) => {
                Outcome::Holding(checked.ok_or_else(unavailable).and_then(|_| {
                    let mut free = Vec::new();
                    for (key, workspace) in runs {
                        let reply = self.delegate.execute(DelegateCommand::Read {
                            key: key.clone(),
                            window: koru_delegate_api::wire::TimelineQuery {
                                from: 0,
                                len: 1,
                                workspace: Some(workspace),
                            },
                        })?;
                        let BotReply::Run { view } = reply else {
                            return Err(unavailable());
                        };
                        if person_holds(&view.header.status) {
                            return Ok((free, true));
                        }
                        free.push(key.run);
                    }
                    Ok((free, false))
                }))
            }
            Step::StopQueued(queued) => {
                let mut stopped = Vec::new();
                if checked.is_some() {
                    for (request, key) in queued {
                        // No fence: the owner cancels queued work; it refuses
                        // work that has a session, which its task settles.
                        if let Ok(BotReply::Applied) = self
                            .delegate
                            .execute(DelegateCommand::Stop { key, fence: None })
                        {
                            stopped.push(request);
                        }
                    }
                }
                Outcome::Stopped(stopped)
            }
            Step::Verify(keys) => {
                Outcome::Verified(checked.ok_or_else(unavailable).and_then(|_| {
                    for key in keys {
                        match self
                            .delegate
                            .execute(DelegateCommand::Reconcile { key: key.clone() })?
                        {
                            BotReply::Admission { admission }
                                if admission.key == key
                                    && matches!(
                                        admission.state,
                                        BotAdmissionState::Settled | BotAdmissionState::Refused
                                    ) => {}
                            // Never admitted: nothing of it can still run.
                            BotReply::Missing => {}
                            _ => return Ok(false),
                        }
                    }
                    Ok(true)
                }))
            }
        }
    }
}

/// A person holds the session, or asked for it: stopping would end what
/// they hold (O27).
fn person_holds(status: &koru_delegate_core::RunStatus) -> bool {
    matches!(
        status.control(),
        Some(
            koru_delegate_core::ControlState::Human { .. }
                | koru_delegate_core::ControlState::HandoverPending { .. }
        )
    )
}

fn delegate_owner(owner: BotOwner) -> koru_delegate_core::BotOwner {
    koru_delegate_core::BotOwner {
        user_scope: owner.user_scope,
        device: owner.device,
    }
}

/// The effects-off barrier a whole-Bot operation installs: its own minted
/// request, at the revision its pause produced.
fn lifecycle_barrier(op: &LifecycleOperation, bot: &Bot) -> Option<BotPolicyBarrier> {
    Some(BotPolicyBarrier {
        key: BotPolicyBarrierKey {
            owner: delegate_owner(op.owner),
            bot: op.bot,
            request: op.policy_request?,
        },
        config_revision: op.barrier_revision?,
        policy_revision: bot.policy_revision,
        policy: bot_native_policy(bot),
        effects_allowed: false,
    })
}

fn backoff(round: u32) -> Duration {
    Duration::from_millis(250) * (1u32 << round.min(7))
}

impl WorkerState {
    pub(in crate::worker) fn bot_lifecycle_command(
        &mut self,
        context: &CommandContext,
        command: &BotCommand,
    ) -> Option<ApiResult<BotResponse>> {
        match command {
            BotCommand::ReadLifecycleOperation { request_id, bot_id } => {
                Some(self.read_bot_lifecycle(context, *request_id, *bot_id))
            }
            BotCommand::ListLifecycle {
                workspace,
                lifecycle,
            } => Some(self.list_bot_lifecycle(context, *workspace, *lifecycle)),
            BotCommand::Archive { .. } => Some(self.archive_bot(context, command)),
            BotCommand::Restore { .. } => Some(self.restore_bot(context, command)),
            _ if command.lifecycle_kind().is_some() => Some(Err(lifecycle_unavailable())),
            _ => self.bot_command_hold(context, command).err().map(Err),
        }
    }

    fn read_bot_lifecycle(
        &self,
        context: &CommandContext,
        request: BotRequestId,
        bot: BotId,
    ) -> ApiResult<BotResponse> {
        let owner = prepared_owner(context)?;
        // The journal is keyed by the trusted scope; an operation of another
        // Bot or device reads as missing, never as someone else's progress.
        let operation = self
            .repository
            .bot_lifecycle_operation(owner.user_scope, request)
            .map_err(bot_failure)?
            .filter(|operation| operation.bot == bot && operation.owner == owner)
            .ok_or_else(not_found)?;
        Ok(BotResponse::Lifecycle {
            operation: operation.progress(),
        })
    }

    /// The Archived or Trash filter: the owner's Bots of a workspace it may
    /// currently see, in that lifecycle. Active Bots stay with `list`.
    fn list_bot_lifecycle(
        &self,
        context: &CommandContext,
        workspace: WorkspaceId,
        lifecycle: BotLifecycle,
    ) -> ApiResult<BotResponse> {
        let owner = prepared_owner(context)?;
        let proof = context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        proof.authority.workspace(workspace).map_err(bot_failure)?;
        if lifecycle == BotLifecycle::Active {
            return Err(failure(
                "invalid_request",
                "Active Bots are listed by the list command.",
                false,
            ));
        }
        let bots = self
            .repository
            .bots_in_lifecycle(owner, workspace, lifecycle)
            .map_err(bot_failure)?;
        Ok(BotResponse::List { bots })
    }

    /// The owner, Bot (in any lifecycle) and fingerprint of a lifecycle
    /// command, after the current workspace authority is checked.
    fn lifecycle_subject(
        &self,
        context: &CommandContext,
        command: &BotCommand,
        bot_id: BotId,
    ) -> ApiResult<(BotOwner, Bot, String)> {
        let owner = prepared_owner(context)?;
        let proof = context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
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
        let fingerprint =
            blake3::hash(&serde_json::to_vec(command).map_err(|_| bot_failure(BotError::Invalid))?)
                .to_hex()
                .to_string();
        Ok((owner, bot, fingerprint))
    }

    /// Archive: capture, or answer an exact repeat with its progress. A
    /// repeat of a parked Archive resumes it; nothing is captured twice.
    fn archive_bot(
        &mut self,
        context: &CommandContext,
        command: &BotCommand,
    ) -> ApiResult<BotResponse> {
        let BotCommand::Archive {
            request_id,
            bot_id,
            expected_revision,
        } = command
        else {
            return Err(not_found());
        };
        let (owner, _, fingerprint) = self.lifecycle_subject(context, command, *bot_id)?;
        if self.bot_delegate.is_none() || self.provider_admin.is_none() {
            return Err(bot_failure(BotError::Unavailable));
        }
        let key = (owner.user_scope, *request_id);
        let now = Timestamp::now().millis();
        let existing = self
            .repository
            .bot_lifecycle_operation(owner.user_scope, *request_id)
            .map_err(bot_failure)?;
        let operation = match existing {
            Some(op) => {
                if op.bot != *bot_id || op.owner != owner || op.fingerprint != fingerprint {
                    return Err(bot_failure(BotError::Conflict));
                }
                if op.stage == BotLifecycleStage::Incomplete {
                    self.repository
                        .resume_bot_lifecycle(owner.user_scope, *request_id, now)
                        .map_err(bot_failure)?;
                }
                self.repository
                    .bot_lifecycle_operation(owner.user_scope, *request_id)
                    .map_err(bot_failure)?
                    .ok_or_else(not_found)?
            }
            None => {
                let members = std::iter::once(LifecycleMemberDraft {
                    subject: BotLifecycleSubject::Bot,
                    reason: BotLifecycleReason::Bot,
                    prior_state: BotLifecyclePriorState::Live,
                    intended: BotLifecycleIntended::Hide,
                    retained: None,
                })
                .chain(
                    self.repository
                        .lifecycle_bot_chats(*bot_id)
                        .map_err(bot_failure)?
                        .into_iter()
                        .map(|(chat_id, trashed)| LifecycleMemberDraft {
                            subject: BotLifecycleSubject::Chat { chat_id },
                            reason: BotLifecycleReason::OwnedConversation,
                            prior_state: if trashed {
                                BotLifecyclePriorState::Trashed
                            } else {
                                BotLifecyclePriorState::Live
                            },
                            intended: if trashed {
                                BotLifecycleIntended::Retain
                            } else {
                                BotLifecycleIntended::Hide
                            },
                            retained: trashed.then_some(BotLifecycleRetained::AlreadyTrashed),
                        }),
                )
                .collect();
                self.repository
                    .capture_bot_lifecycle(&LifecycleCapture {
                        operation: *request_id,
                        owner,
                        bot: *bot_id,
                        kind: BotLifecycleKind::Archive,
                        chat: None,
                        fingerprint,
                        expected_revision: *expected_revision,
                        source: None,
                        members,
                        now,
                    })
                    .map_err(bot_failure)?
            }
        };
        if !matches!(
            operation.stage,
            BotLifecycleStage::Completed | BotLifecycleStage::Incomplete
        ) {
            self.bot_lifecycle.driving.entry(key).or_default();
        }
        Ok(BotResponse::Lifecycle {
            operation: operation.progress(),
        })
    }

    /// Unarchive (Restore naming an Archive): one AI transaction, no owner
    /// call. The Bot returns active and still paused. Reversing a Trash is a
    /// later slice and refuses explicitly.
    fn restore_bot(
        &mut self,
        context: &CommandContext,
        command: &BotCommand,
    ) -> ApiResult<BotResponse> {
        let BotCommand::Restore {
            request_id,
            bot_id,
            expected_revision,
            source_request_id,
        } = command
        else {
            return Err(not_found());
        };
        let (owner, _, fingerprint) = self.lifecycle_subject(context, command, *bot_id)?;
        let source = self
            .repository
            .bot_lifecycle_operation(owner.user_scope, *source_request_id)
            .map_err(bot_failure)?
            .filter(|source| source.bot == *bot_id && source.owner == owner)
            .ok_or_else(not_found)?;
        if source.kind != BotLifecycleKind::Archive {
            return Err(lifecycle_unavailable());
        }
        // A moving Archive is reversed once it completes or parks itself.
        let replay = self
            .repository
            .bot_lifecycle_operation(owner.user_scope, *request_id)
            .map_err(bot_failure)?
            .is_some();
        if !replay
            && !matches!(
                source.stage,
                BotLifecycleStage::Completed | BotLifecycleStage::Incomplete
            )
        {
            return Err(hold_failure(BotHold::Pending));
        }
        let operation = self
            .repository
            .unarchive_bot(&LifecycleCapture {
                operation: *request_id,
                owner,
                bot: *bot_id,
                kind: BotLifecycleKind::Restore,
                chat: None,
                fingerprint,
                expected_revision: *expected_revision,
                source: Some(*source_request_id),
                members: vec![],
                now: Timestamp::now().millis(),
            })
            .map_err(bot_failure)?;
        self.bot_lifecycle
            .driving
            .remove(&(owner.user_scope, *source_request_id));
        Ok(BotResponse::Lifecycle {
            operation: operation.progress(),
        })
    }

    /// The native-policy evidence for an active Bot: while an Archive (or
    /// later Trash) is unfinished, its own effects-off barrier, so no native
    /// effect starts under the Bot being settled; otherwise the ordinary one.
    pub(in crate::worker) fn lifecycle_policy_barrier(
        &self,
        bot: &Bot,
    ) -> Option<BotPolicyBarrier> {
        let (scope, operation) = self.repository.open_bot_lifecycle(bot.id).ok().flatten()?;
        let op = self
            .repository
            .bot_lifecycle_operation(scope, operation)
            .ok()
            .flatten()?;
        lifecycle_barrier(&op, bot)
    }

    /// Restart resume, one bounded page at a time, then each due operation's
    /// next step. Called from the writer loop; cheap when nothing is driven.
    pub(in crate::worker) fn schedule_bot_lifecycle(&mut self) {
        if self.bot_delegate.is_none() || self.provider_admin.is_none() {
            return;
        }
        if let Some(after) = self.bot_lifecycle.recovery {
            match self.repository.resumable_bot_lifecycle(after, 16) {
                Ok(rows) if rows.is_empty() => self.bot_lifecycle.recovery = None,
                Ok(rows) => {
                    for (at, scope, operation) in rows {
                        self.bot_lifecycle.recovery = Some(Some((at, operation)));
                        self.bot_lifecycle
                            .driving
                            .entry((scope, operation))
                            .or_default();
                    }
                }
                Err(_) => self.bot_lifecycle.recovery = None,
            }
        }
        if self.bot_lifecycle.driving.is_empty() {
            return;
        }
        let now = Instant::now();
        let due: Vec<OperationKey> = self
            .bot_lifecycle
            .driving
            .iter()
            .filter(|(_, drive)| !drive.busy && drive.due.is_none_or(|due| due <= now))
            .map(|(key, _)| *key)
            .collect();
        for key in due {
            self.step_bot_lifecycle(key);
        }
    }

    /// No progress this time: wait longer, or park once waiting stops helping.
    fn wait_bot_lifecycle(&mut self, key: OperationKey, refusal: BotLifecycleRefusal) {
        let Some(drive) = self.bot_lifecycle.driving.get_mut(&key) else {
            return;
        };
        drive.retries += 1;
        if drive.retries >= PARK_AFTER {
            self.bot_lifecycle.driving.remove(&key);
            if let Err(error) =
                self.repository
                    .park_bot_lifecycle(key.0, key.1, refusal, Timestamp::now().millis())
            {
                tracing::warn!(target: "koru_ai::bots", ?error, "a Bot lifecycle operation could not be parked");
            }
            tracing::info!(target: "koru_ai::bots", operation = %key.1, refusal = refusal.token(), "Bot lifecycle operation parked incomplete");
            return;
        }
        drive.due = Some(Instant::now() + backoff(drive.retries));
    }

    fn progressed_bot_lifecycle(&mut self, key: OperationKey) {
        if let Some(drive) = self.bot_lifecycle.driving.get_mut(&key) {
            drive.retries = 0;
            drive.due = None;
        }
    }

    fn submit_bot_lifecycle(
        &mut self,
        key: OperationKey,
        op: &LifecycleOperation,
        bot: &Bot,
        step: Step,
    ) {
        let (Some(authority), Some(delegate), Some(pool)) = (
            self.bot_authority.clone(),
            self.bot_delegate.clone(),
            self.provider_admin.as_ref(),
        ) else {
            return;
        };
        let job = LifecycleJob {
            key,
            owner: op.owner,
            workspace: bot.home_workspace,
            authority,
            delegate,
            step,
        };
        match pool.try_submit(ProviderAdminJob::BotLifecycle(job)) {
            Ok(()) => {
                if let Some(drive) = self.bot_lifecycle.driving.get_mut(&key) {
                    drive.busy = true;
                }
            }
            // The pool is full: the next loop tries again.
            Err(_) => {
                if let Some(drive) = self.bot_lifecycle.driving.get_mut(&key) {
                    drive.due = Some(Instant::now() + backoff(0));
                }
            }
        }
    }

    fn step_bot_lifecycle(&mut self, key: OperationKey) {
        let op = match self.repository.bot_lifecycle_operation(key.0, key.1) {
            Ok(Some(op)) => op,
            Ok(None) => {
                self.bot_lifecycle.driving.remove(&key);
                return;
            }
            Err(_) => return self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable),
        };
        // S4 drives Archive only; the other kinds are refused at capture.
        if op.kind != BotLifecycleKind::Archive
            || matches!(
                op.stage,
                BotLifecycleStage::Completed | BotLifecycleStage::Incomplete
            )
        {
            self.bot_lifecycle.driving.remove(&key);
            return;
        }
        let Ok(Some(bot)) = self.repository.bot(op.bot) else {
            // The Bot left the active lifecycle under the operation.
            return self.wait_bot_lifecycle(key, BotLifecycleRefusal::RevisionChanged);
        };
        match op.stage {
            BotLifecycleStage::Captured => self.step_archive_barrier(key, &op, &bot),
            BotLifecycleStage::Settling => self.step_archive_settle(key, &op, &bot),
            _ => {
                self.bot_lifecycle.driving.remove(&key);
            }
        }
    }

    /// `captured`: the pause is acknowledged through its own receipt, then
    /// the effects-off barrier is installed.
    fn step_archive_barrier(&mut self, key: OperationKey, op: &LifecycleOperation, bot: &Bot) {
        let Some(pause) = op.pause_request else {
            return self.wait_bot_lifecycle(key, BotLifecycleRefusal::MemberConflict);
        };
        match self
            .repository
            .bot_admission_receipt(op.owner, op.bot, pause)
        {
            Ok(Some(receipt)) => match receipt.operation().status {
                BotOperationStatus::Accepted => {}
                BotOperationStatus::Pending => {
                    // The existing pause coordination owns the Barrier call.
                    self.queue_bot_receipt(bot, receipt, None);
                    return self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable);
                }
                BotOperationStatus::Rejected => {
                    return self.wait_bot_lifecycle(key, BotLifecycleRefusal::MemberConflict);
                }
            },
            _ => return self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable),
        }
        match lifecycle_barrier(op, bot) {
            Some(barrier) => self.submit_bot_lifecycle(key, op, bot, Step::Policy(barrier)),
            None => self.wait_bot_lifecycle(key, BotLifecycleRefusal::MemberConflict),
        }
    }

    /// `settling`: refuse while a person holds a session, stop every live
    /// task and queued submission, prove each stopped Run settled, require
    /// the barrier effective, then archive.
    fn step_archive_settle(&mut self, key: OperationKey, op: &LifecycleOperation, bot: &Bot) {
        // A restart's conversation recovery is still finding interrupted tasks.
        if self.bot_chat_recovery.is_some() {
            return self.wait_bot_lifecycle(key, BotLifecycleRefusal::WorkRunning);
        }
        let live: Vec<(
            InferenceAttemptId,
            BotRunKey,
            WorkspaceId,
            bool,
            BotSubmissionKey,
        )> = self
            .bot_chats
            .iter()
            .filter_map(|(id, chat)| {
                let (run, workspace, session, submission) = chat.lifecycle_view();
                (run.bot == op.bot).then_some((*id, run, workspace, session, submission))
            })
            .collect();
        let Some(drive) = self.bot_lifecycle.driving.get_mut(&key) else {
            return;
        };
        // Before anything is stopped, every live session is read (O27).
        let unread: Vec<(BotRunKey, WorkspaceId)> = live
            .iter()
            .filter(|(id, run, _, session, _)| {
                *session && !drive.stopped.contains(id) && !drive.free.contains(&run.run)
            })
            .map(|(_, run, workspace, _, _)| (run.clone(), *workspace))
            .collect();
        if !unread.is_empty() {
            return self.submit_bot_lifecycle(key, op, bot, Step::Holding(unread));
        }
        let mut progressed = false;
        for (id, run, _, _, submission) in &live {
            let drive = self.bot_lifecycle.driving.get_mut(&key).expect("driven");
            if drive.stopped.insert(*id) {
                drive.runs.insert(run.run, submission.clone());
                progressed = true;
                // The person's Stop, on its reserved lane; the step machine
                // keeps the task until its owner applied the Stop.
                if let Err(error) = self.stop_attempt(*id) {
                    tracing::warn!(target: "koru_ai::bots", attempt = %id, code = %error.code, "an archived Bot's task could not be stopped yet");
                }
            }
        }
        if progressed {
            self.progressed_bot_lifecycle(key);
            return;
        }
        if !live.is_empty() {
            // Stopped; waiting for the owner to apply it and the task to end.
            return self.wait_bot_lifecycle(key, BotLifecycleRefusal::WorkRunning);
        }
        let queued = match self
            .repository
            .bot_unresolved_submissions(op.bot, QUEUED_PAGE)
        {
            Ok(queued) => queued,
            Err(_) => return self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable),
        };
        if !queued.is_empty() {
            let drive = self.bot_lifecycle.driving.get_mut(&key).expect("driven");
            let mut stop = Vec::new();
            let mut receipts = Vec::new();
            for (scope, request) in queued {
                if scope != op.owner.user_scope {
                    continue;
                }
                let Ok(Some(receipt)) = self
                    .repository
                    .bot_admission_receipt(op.owner, op.bot, request)
                else {
                    continue;
                };
                if let BotAdmissionReceipt::Submission(r) = &receipt {
                    let p = &r.intent.submission.provenance;
                    let submission = BotSubmissionKey {
                        owner: p.owner,
                        bot: p.bot,
                        request: p.submission,
                    };
                    drive
                        .runs
                        .insert(r.intent.submission.request.run, submission);
                    if !drive.queued.contains(&request) {
                        stop.push((
                            request,
                            BotRunKey {
                                owner: p.owner,
                                bot: p.bot,
                                run: r.intent.submission.request.run,
                            },
                        ));
                    }
                }
                receipts.push(receipt);
            }
            if !stop.is_empty() {
                return self.submit_bot_lifecycle(key, op, bot, Step::StopQueued(stop));
            }
            // Stopped: the existing coordination reconciles each receipt.
            for receipt in receipts {
                self.queue_bot_receipt(bot, receipt, None);
            }
            return self.wait_bot_lifecycle(key, BotLifecycleRefusal::WorkRunning);
        }
        let drive = self.bot_lifecycle.driving.get(&key).expect("driven");
        if !drive.runs.is_empty() {
            let keys = drive.runs.values().cloned().collect();
            return self.submit_bot_lifecycle(key, op, bot, Step::Verify(keys));
        }
        if op.policy_barrier != Some(koru_ai_core::bots::BotLifecycleBarrierState::Effective) {
            return match lifecycle_barrier(op, bot) {
                Some(barrier) => self.submit_bot_lifecycle(key, op, bot, Step::Policy(barrier)),
                None => self.wait_bot_lifecycle(key, BotLifecycleRefusal::MemberConflict),
            };
        }
        match self
            .repository
            .complete_bot_archive(key.0, key.1, Timestamp::now().millis())
        {
            Ok(_) => {
                tracing::info!(target: "koru_ai::bots", operation = %key.1, "Bot archived");
                self.bot_lifecycle.driving.remove(&key);
            }
            Err(BotError::RevisionConflict) => {
                self.wait_bot_lifecycle(key, BotLifecycleRefusal::RevisionChanged)
            }
            Err(_) => self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable),
        }
    }

    /// An owner step's answer, applied on the writer.
    pub(in crate::worker) fn complete_bot_lifecycle(&mut self, result: LifecycleResult) {
        let key = result.key;
        let Some(drive) = self.bot_lifecycle.driving.get_mut(&key) else {
            return;
        };
        drive.busy = false;
        // A parked or completed operation ignores a late answer.
        let op = match self.repository.bot_lifecycle_operation(key.0, key.1) {
            Ok(Some(op))
                if !matches!(
                    op.stage,
                    BotLifecycleStage::Completed | BotLifecycleStage::Incomplete
                ) =>
            {
                op
            }
            Ok(_) => {
                self.bot_lifecycle.driving.remove(&key);
                return;
            }
            Err(_) => return self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable),
        };
        let now = Timestamp::now().millis();
        match result.outcome {
            Outcome::Policy(Ok(state)) => {
                let effective = state == BotPolicyBarrierState::Effective;
                if !effective
                    && let Some(barrier) = self
                        .repository
                        .bot(op.bot)
                        .ok()
                        .flatten()
                        .and_then(|bot| lifecycle_barrier(&op, &bot))
                {
                    // Held native effects drain under the existing recovery.
                    self.queue_native_recovery(
                        barrier.key.owner,
                        barrier.key.bot,
                        None,
                        None,
                        NativeDrainReason::PolicyPending { key: barrier.key },
                    );
                }
                let moved = match op.stage {
                    BotLifecycleStage::Captured => self.repository.advance_bot_lifecycle(
                        key.0,
                        key.1,
                        BotLifecycleStage::Captured,
                        BotLifecycleStage::Settling,
                        effective,
                        now,
                    ),
                    BotLifecycleStage::Settling if effective => {
                        self.repository.advance_bot_lifecycle(
                            key.0,
                            key.1,
                            BotLifecycleStage::Settling,
                            BotLifecycleStage::Settling,
                            true,
                            now,
                        )
                    }
                    _ => Ok(false),
                };
                if matches!(moved, Ok(true)) {
                    self.progressed_bot_lifecycle(key);
                } else {
                    self.wait_bot_lifecycle(key, BotLifecycleRefusal::WorkRunning);
                }
            }
            Outcome::Policy(Err(_)) | Outcome::Verified(Err(_)) => {
                self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable)
            }
            Outcome::Holding(Ok((_, true))) => {
                // O27: never end what a person holds. Parked; the same
                // request retries once they hand the session back.
                self.bot_lifecycle.driving.remove(&key);
                let _ = self.repository.park_bot_lifecycle(
                    key.0,
                    key.1,
                    BotLifecycleRefusal::PersonHolding,
                    now,
                );
                tracing::info!(target: "koru_ai::bots", operation = %key.1, "Bot lifecycle refused: a person holds a session");
            }
            Outcome::Holding(Ok((free, false))) => {
                if let Some(drive) = self.bot_lifecycle.driving.get_mut(&key) {
                    drive.free.extend(free);
                }
                self.progressed_bot_lifecycle(key);
            }
            Outcome::Holding(Err(_)) => {
                self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable)
            }
            Outcome::Stopped(stopped) => {
                let any = !stopped.is_empty();
                if let Some(drive) = self.bot_lifecycle.driving.get_mut(&key) {
                    drive.queued.extend(stopped);
                }
                if any {
                    self.progressed_bot_lifecycle(key);
                } else {
                    self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable);
                }
            }
            Outcome::Verified(Ok(true)) => {
                if let Some(drive) = self.bot_lifecycle.driving.get_mut(&key) {
                    drive.runs.clear();
                }
                self.progressed_bot_lifecycle(key);
            }
            Outcome::Verified(Ok(false)) => {
                self.wait_bot_lifecycle(key, BotLifecycleRefusal::WorkRunning)
            }
        }
    }

    /// A Bot-level command on a held Bot, or a task naming a held
    /// conversation, refuses before it reaches its owner path.
    fn bot_command_hold(&self, context: &CommandContext, command: &BotCommand) -> ApiResult<()> {
        let (bot, chat) = match command {
            BotCommand::Start {
                bot_id, chat_id, ..
            }
            | BotCommand::Continue {
                bot_id, chat_id, ..
            } => (*bot_id, Some(*chat_id)),
            BotCommand::CreateChat { bot_id, .. }
            | BotCommand::Update { bot_id, .. }
            | BotCommand::UpdateAccess { bot_id, .. }
            | BotCommand::Pause { bot_id, .. }
            | BotCommand::Resume { bot_id, .. } => (*bot_id, None),
            _ => return Ok(()),
        };
        let hold = match chat {
            Some(chat) if self.repository.bot_for_chat(chat).map_err(bot_failure)? == Some(bot) => {
                self.repository.bot_chat_hold(chat)
            }
            _ => self.repository.bot_hold(bot),
        }
        .map_err(bot_failure)?;
        let Some(hold) = hold else {
            return Ok(());
        };
        // Ownership first: someone else's Bot never learns its lifecycle.
        if self.repository.bot_owner(bot).map_err(bot_failure)? != Some(prepared_owner(context)?) {
            return Err(bot_failure(BotError::Forbidden));
        }
        Err(hold_failure(hold))
    }
}

/// The Bot a command is proven against. An archived Bot's history stays
/// readable, as the approved experience keeps it ("retains its history in an
/// Archived filter"): a named read of its conversation, and a lifecycle
/// command naming it, are proven against the archived Bot. Everything else
/// sees only an active one, so a change to an archived Bot reads as missing.
pub(super) fn provable_bot(
    repository: &Repository,
    id: BotId,
    command: &WorkerCommand,
) -> Result<Option<Bot>, BotError> {
    let admits = command.bot_read_only()
        || matches!(
            command,
            WorkerCommand::Bots {
                request: BotCommand::Restore { .. } | BotCommand::ReadLifecycleOperation { .. },
                ..
            }
        );
    // `BotRepository::bot` answers an inactive Bot as forbidden; this
    // reads the lifecycle itself, so an inactive Bot is simply not provable.
    Ok(match repository.bot_any_lifecycle(id)? {
        Some((bot, BotLifecycle::Active)) => Some(bot),
        Some((bot, BotLifecycle::Archived)) if admits => Some(bot),
        _ => None,
    })
}

/// A read whose proof names an archived Bot's conversation (see
/// [`provable_bot`]); a change never reaches a read with such a proof,
/// because the guard refuses it first.
pub(super) fn readable_bot(repository: &Repository, id: BotId) -> Result<Option<Bot>, BotError> {
    Ok(match repository.bot_any_lifecycle(id)? {
        Some((bot, BotLifecycle::Active | BotLifecycle::Archived)) => Some(bot),
        _ => None,
    })
}

fn prepared_owner(context: &CommandContext) -> ApiResult<BotOwner> {
    authorize(context, AiScope::ManageBots)?;
    let proof = context
        .prepared
        .as_ref()
        .ok_or_else(|| bot_failure(BotError::Unavailable))?;
    proof.authority.owner().map_err(bot_failure)
}

/// Whether a held conversation refuses this command. A trashed one refuses
/// everything but Stop; a pending one refuses what would add to or change
/// it, while reads, denial and Stop stay available to settle it.
///
/// Attachment staging changes the conversation's own rows, so both holds
/// refuse every staging command (admission, a workspace copy, the row, its
/// removal) before any bytes are prepared. Reading a staged or sent
/// attachment (`ResolveAttachments`) is a read: a pending hold keeps it.
pub(super) fn hold_refuses(command: &WorkerCommand, hold: BotHold) -> bool {
    match hold {
        BotHold::Trashed => !matches!(command, WorkerCommand::CancelAttempt { .. }),
        BotHold::Pending => matches!(
            command,
            WorkerCommand::SendTurn { .. }
                | WorkerCommand::RetryTurn { .. }
                | WorkerCommand::SteerAttempt { .. }
                | WorkerCommand::BranchChat { .. }
                | WorkerCommand::UpdateChatModel { .. }
                | WorkerCommand::UpdateChatAgentSettings { .. }
                | WorkerCommand::ApproveToolCall { .. }
                | WorkerCommand::AttachmentAdmission { .. }
                | WorkerCommand::WorkspaceCopyAdmission { .. }
                | WorkerCommand::StageAttachment { .. }
                | WorkerCommand::RemoveStagedAttachment { .. }
        ),
    }
}

pub(super) fn hold_failure(hold: BotHold) -> Failure {
    match hold {
        BotHold::Trashed => failure(
            bot_lifecycle_codes::TRASHED,
            "This Bot conversation is in Trash. Restore it to use it again.",
            false,
        ),
        BotHold::Pending => failure(
            bot_lifecycle_codes::PENDING,
            "This Bot conversation is being archived, moved to Trash or restored.",
            false,
        ),
    }
}

fn lifecycle_unavailable() -> Failure {
    failure(
        "unsupported_capability",
        "Bot lifecycle changes are not available yet.",
        false,
    )
}
