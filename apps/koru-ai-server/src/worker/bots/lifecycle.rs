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
//! 2. `settling`: submissions the owner has not acknowledged are resolved
//!    first (the pause makes it refuse new admissions, so the set closes).
//!    Then the delegate is asked, every round, which of the Bot's admissions
//!    it has not settled. A person holding one of their sessions parks the
//!    operation as `incomplete/person_holding` before anything is sent (O27).
//!    Otherwise each is ended as an automatic interruption: a live
//!    conversation task through its own finishing lane, anything else (an
//!    API-started task, queued work) by the owner's `Interrupt`. Neither is
//!    the person's Stop: the owner's cleanup checks the session lease itself,
//!    so a person who takes the session after the read keeps it. The
//!    operation waits until the delegate reports nothing unsettled and the
//!    barrier is effective (native effects drained).
//! 3. One transaction archives the Bot. Nothing is hidden or removed.
//!
//! A restart resumes from the journal's stage, never recaptures, and re-asks
//! every owner idempotently; nothing the driver remembers decides what has
//! settled. An operation that cannot progress is parked `incomplete` with
//! its reason; it is never retried on its own. The Bot's read names it, and
//! the same request, or one naming only its id, resumes it. Unarchive is a
//! Restore naming the Archive, applied in one transaction with no owner
//! call. Trash, Restore of a Trash and Purge refuse until their slices.
use super::*;
use koru_ai_sqlite::{BotHold, LifecycleCapture, LifecycleMemberDraft, LifecycleOperation};
use koru_ai_wire::bot_lifecycle_codes;
use koru_delegate_api::bots::{BotCommand as DelegateCommand, BotDelegatePort, BotReply};
use koru_delegate_core::{
    BotPolicyBarrier, BotPolicyBarrierKey, BotPolicyBarrierState, BotRunKey, NativeDrainReason,
    RunId,
};

type OperationKey = (UserScopeId, BotRequestId);

/// Consecutive rounds that made no progress before an operation is parked.
/// The waits double from 250 ms to 32 s, so this is a few minutes.
const PARK_AFTER: u32 = 12;
/// The most unacknowledged submissions one settling round resolves.
const QUEUED_PAGE: u16 = 16;
/// The most queued admissions one delegate read returns. A Bot holds at
/// most 50 queued (`BOT_PENDING_LIMIT`), so one page is normally all of them.
const UNSETTLED_PAGE: u16 = 64;

/// What the writer drives. Nothing here is durable: the journal is, and the
/// delegate is the authority on what still runs.
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

/// One operation's scheduling. It holds no fact about the work itself.
#[derive(Default)]
struct Drive {
    busy: bool,
    due: Option<Instant>,
    /// Consecutive rounds with no progress; the operation parks at
    /// [`PARK_AFTER`].
    retries: u32,
    /// Consecutive rounds put off for a reason that is not the operation's:
    /// the provider-administration pool was full, or startup conversation
    /// recovery was still paging. They back off the same way, and never
    /// count toward parking.
    deferrals: u32,
    /// How much unsettled work the delegate reported last round, so a round
    /// in which some of it settled counts as progress.
    remaining: Option<usize>,
}
impl Drive {
    /// A round with no progress. `true` when waiting has stopped helping and
    /// the operation should park.
    fn wait(&mut self, now: Instant) -> bool {
        self.retries += 1;
        if self.retries >= PARK_AFTER {
            return true;
        }
        self.due = Some(now + backoff(self.retries));
        false
    }
    /// A round put off for a reason outside the operation: wait longer each
    /// time, but this alone never parks it.
    fn defer(&mut self, now: Instant) {
        self.deferrals = (self.deferrals + 1).min(7);
        self.due = Some(now + backoff(self.deferrals));
    }
    fn progressed(&mut self) {
        self.retries = 0;
        self.deferrals = 0;
        self.due = None;
    }
    fn due(&self, now: Instant) -> bool {
        !self.busy && self.due.is_none_or(|due| due <= now)
    }
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
    /// Ask the delegate what of the Bot it has not settled, then interrupt
    /// each of those that no live conversation task of this writer is
    /// already finishing (`live`). Nothing is sent when a person holds one.
    Settle { bot: BotId, live: HashSet<RunId> },
}
pub(in crate::worker) struct LifecycleResult {
    key: OperationKey,
    outcome: Outcome,
}
enum Outcome {
    Policy(Result<BotPolicyBarrierState, koru_delegate_api::Failure>),
    Settle(Result<Unsettled, koru_delegate_api::Failure>),
}
/// The delegate's answer for one settling round.
struct Unsettled {
    /// Every Run it reported not settled, before this round's interruptions.
    runs: Vec<RunId>,
    /// More queued work exists past the page.
    more: bool,
    /// A person holds (or asked for) one of these sessions; nothing was sent.
    held: bool,
}

fn unavailable() -> koru_delegate_api::Failure {
    koru_delegate_api::Failure::new("bot_unavailable", "Bot lifecycle owner unavailable.", true)
}

impl LifecycleJob {
    pub(in crate::worker) fn run(self) -> LifecycleResult {
        let key = self.key;
        let settle = matches!(self.step, Step::Settle { .. });
        let outcome = catch_unwind(AssertUnwindSafe(|| self.execute())).unwrap_or_else(|_| {
            if settle {
                Outcome::Settle(Err(unavailable()))
            } else {
                Outcome::Policy(Err(unavailable()))
            }
        });
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
            Step::Settle { bot, live } => {
                let owner = delegate_owner(self.owner);
                Outcome::Settle(checked.ok_or_else(unavailable).and_then(|_| {
                    let BotReply::UnsettledWork { work } =
                        self.delegate.execute(DelegateCommand::UnsettledWork {
                            owner,
                            bot,
                            limit: UNSETTLED_PAGE,
                        })?
                    else {
                        return Err(unavailable());
                    };
                    if work
                        .admissions
                        .iter()
                        .any(|a| a.admission.key.owner != owner || a.admission.key.bot != bot)
                    {
                        return Err(unavailable());
                    }
                    let runs = work.admissions.iter().map(|a| a.admission.run).collect();
                    // O27: a person holds a session. Nothing is sent at all.
                    if work.admissions.iter().any(|a| a.held) {
                        return Ok(Unsettled {
                            runs,
                            more: work.more,
                            held: true,
                        });
                    }
                    for unsettled in &work.admissions {
                        let admission = &unsettled.admission;
                        if live.contains(&admission.run) {
                            continue;
                        }
                        // An automatic interruption with the owner's current
                        // fence, never the person's Stop. A queued Run is
                        // interrupted without starting; an active one keeps its
                        // slot until the owner proves its process ended, under
                        // the session's own lease check. A pending or refused
                        // answer is read again next round, not remembered.
                        let _ = self.delegate.execute(DelegateCommand::Interrupt {
                            key: BotRunKey {
                                owner,
                                bot,
                                run: admission.run,
                            },
                            fence: admission.fence.clone(),
                        });
                    }
                    Ok(Unsettled {
                        runs,
                        more: work.more,
                        held: false,
                    })
                }))
            }
        }
    }
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

/// The override behind [`WorkerState::lifecycle_policy_barrier`], with its
/// two journal reads passed in. Any failure, or an open operation whose
/// barrier cannot be rebuilt, is an error: never "no operation".
fn lifecycle_evidence(
    open: Result<Option<OperationKey>, BotError>,
    read: impl FnOnce(UserScopeId, BotRequestId) -> Result<Option<LifecycleOperation>, BotError>,
    bot: &Bot,
) -> ApiResult<Option<BotPolicyBarrier>> {
    let Some((scope, operation)) = open.map_err(bot_failure)? else {
        return Ok(None);
    };
    let op = read(scope, operation)
        .map_err(bot_failure)?
        .ok_or_else(|| bot_failure(BotError::Storage))?;
    lifecycle_barrier(&op, bot)
        .map(Some)
        .ok_or_else(|| bot_failure(BotError::Storage))
}

fn backoff(round: u32) -> Duration {
    Duration::from_millis(250) * (1u32 << round.min(7))
}

/// The Bot commands that only read, and so are answered for an archived Bot
/// as for an active one (EXPERIENCE §3: Archive "retains its history in an
/// Archived filter"). Every other command naming an archived Bot answers
/// `bot_archived`. A new read-only Bot command is added here.
pub(super) fn bot_read_command(request: &BotCommand) -> bool {
    matches!(
        request,
        BotCommand::Read { .. }
            | BotCommand::ReadOperation { .. }
            | BotCommand::ReadLifecycleOperation { .. }
            | BotCommand::List { .. }
            | BotCommand::ListLifecycle { .. }
            // P1.8 reads: an archived Bot's conversations stay listable;
            // activity is read for active Bots only by its own query.
            | BotCommand::ListBotChats { .. }
            | BotCommand::ListActivity { .. }
    )
}

/// The archived Bot a change would touch, if the command is one: a Bot
/// command that is not a read and not a lifecycle command (which carry their
/// own rules), or any non-read command on one of its conversations.
/// Preparation captures nothing for it and dispatch answers `bot_archived`
/// once the owner's authority is checked.
pub(super) fn archived_change(
    repository: &Repository,
    command: &WorkerCommand,
) -> ApiResult<Option<BotId>> {
    let id = match command {
        WorkerCommand::Bots { request, .. } => match request {
            _ if bot_read_command(request) || request.lifecycle_kind().is_some() => None,
            BotCommand::CreateChat { bot_id, .. }
            | BotCommand::Update { bot_id, .. }
            | BotCommand::UpdateAccess { bot_id, .. }
            | BotCommand::Pause { bot_id, .. }
            | BotCommand::Resume { bot_id, .. }
            | BotCommand::Start { bot_id, .. }
            | BotCommand::Continue { bot_id, .. }
            | BotCommand::ReconcileOperation { bot_id, .. } => Some(*bot_id),
            _ => None,
        },
        _ if command.bot_read_only() => None,
        _ => match super::command_chat(repository, command)? {
            Some(chat) => repository.bot_for_chat(chat).map_err(bot_failure)?,
            None => None,
        },
    };
    Ok(match id {
        Some(id)
            if matches!(
                repository.bot_any_lifecycle(id).map_err(bot_failure)?,
                Some((_, BotLifecycle::Archived))
            ) =>
        {
            Some(id)
        }
        _ => None,
    })
}

pub(super) fn archived_failure() -> Failure {
    failure(
        bot_lifecycle_codes::ARCHIVED,
        "This Bot is archived. Its history can be read; unarchive it to change it.",
        false,
    )
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
            BotCommand::Read { bot_id } => Some(self.read_bot(context, *bot_id)),
            BotCommand::ReadOperation { request_id, bot_id } => self
                .read_archived_operation(context, *request_id, *bot_id)
                .transpose(),
            BotCommand::Archive { .. } => Some(self.archive_bot(context, command)),
            BotCommand::Restore { .. } => Some(self.restore_bot(context, command)),
            _ if command.lifecycle_kind().is_some() => Some(Err(lifecycle_unavailable())),
            _ => self
                .bot_command_hold(context, command)
                .and_then(|()| self.refuse_archived(context, command))
                .err()
                .map(Err),
        }
    }

    /// The Bot itself, active or archived, with its lifecycle and its open
    /// operation, if any, so a client that lost an operation's id can find
    /// it. Owner and current workspace authority come first.
    fn read_bot(&self, context: &CommandContext, id: BotId) -> ApiResult<BotResponse> {
        let mut bot = self.readable_owned_bot(context, id)?;
        bot.open_operation = self
            .repository
            .bot_open_lifecycle(id)
            .map_err(bot_failure)?;
        Ok(BotResponse::Bot { bot })
    }

    /// A Bot whose history its owner may read: active or archived, after the
    /// trusted owner and the current authority over its home workspace are
    /// checked. A trashed Bot is `not_found` once the owner is known (its
    /// workspace is not inspected for it), as a missing one is.
    pub(super) fn readable_owned_bot(&self, context: &CommandContext, id: BotId) -> ApiResult<Bot> {
        let (bot, lifecycle) = self.bot_of_owner(context, id)?;
        if lifecycle == BotLifecycle::Trashed {
            return Err(not_found());
        }
        self.bot_workspace_reachable(context, &bot)?;
        Ok(bot)
    }

    /// A Bot in any lifecycle, after the trusted owner and the current
    /// authority over its home workspace are checked.
    fn owned_bot(&self, context: &CommandContext, id: BotId) -> ApiResult<(Bot, BotLifecycle)> {
        let (bot, lifecycle) = self.bot_of_owner(context, id)?;
        self.bot_workspace_reachable(context, &bot)?;
        Ok((bot, lifecycle))
    }

    fn bot_workspace_reachable(&self, context: &CommandContext, bot: &Bot) -> ApiResult<()> {
        context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?
            .authority
            .workspace(bot.home_workspace)
            .map_err(bot_failure)?;
        Ok(())
    }

    /// A Bot in any lifecycle whose fixed owner is the trusted caller.
    fn bot_of_owner(&self, context: &CommandContext, id: BotId) -> ApiResult<(Bot, BotLifecycle)> {
        let owner = prepared_owner(context)?;
        let (bot, lifecycle) = self
            .repository
            .bot_any_lifecycle(id)
            .map_err(bot_failure)?
            .ok_or_else(not_found)?;
        if (BotOwner {
            user_scope: bot.user_scope,
            device: bot.owner_device,
        }) != owner
        {
            return Err(bot_failure(BotError::Forbidden));
        }
        Ok((bot, lifecycle))
    }

    /// An archived Bot's admission receipt reads as it was left. An active
    /// Bot's goes through the ordinary admission path (`None`).
    fn read_archived_operation(
        &self,
        context: &CommandContext,
        request: BotRequestId,
        id: BotId,
    ) -> ApiResult<Option<BotResponse>> {
        if !matches!(
            self.repository.bot_any_lifecycle(id).map_err(bot_failure)?,
            Some((_, BotLifecycle::Archived))
        ) {
            return Ok(None);
        }
        let (bot, _) = self.owned_bot(context, id)?;
        let owner = BotOwner {
            user_scope: bot.user_scope,
            device: bot.owner_device,
        };
        // An access change and an admission share the operation vocabulary.
        let operation = match self
            .repository
            .bot_access_receipt(owner, id, request)
            .map_err(bot_failure)?
        {
            Some(receipt) => receipt.operation(),
            None => self
                .repository
                .bot_admission_receipt(owner, id, request)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?
                .operation(),
        };
        Ok(Some(BotResponse::Operation { operation }))
    }

    /// A change to an archived Bot answers `bot_archived`, once its owner's
    /// authority over the Bot's workspace is checked.
    fn refuse_archived(&self, context: &CommandContext, command: &BotCommand) -> ApiResult<()> {
        let id = match command {
            _ if bot_read_command(command) => return Ok(()),
            BotCommand::CreateChat { bot_id, .. }
            | BotCommand::Update { bot_id, .. }
            | BotCommand::UpdateAccess { bot_id, .. }
            | BotCommand::Pause { bot_id, .. }
            | BotCommand::Resume { bot_id, .. }
            | BotCommand::Start { bot_id, .. }
            | BotCommand::Continue { bot_id, .. }
            | BotCommand::ReconcileOperation { bot_id, .. } => *bot_id,
            _ => return Ok(()),
        };
        if !matches!(
            self.repository.bot_any_lifecycle(id).map_err(bot_failure)?,
            Some((_, BotLifecycle::Archived))
        ) {
            return Ok(());
        }
        self.owned_bot(context, id)?;
        Err(archived_failure())
    }

    /// One lifecycle operation's progress. The journal is keyed by the
    /// trusted scope; an operation of another Bot or device reads as missing,
    /// never as someone else's progress, and the Bot's home workspace must
    /// still be the caller's.
    fn read_bot_lifecycle(
        &self,
        context: &CommandContext,
        request: BotRequestId,
        bot: BotId,
    ) -> ApiResult<BotResponse> {
        let owner = prepared_owner(context)?;
        let operation = self
            .repository
            .bot_lifecycle_operation(owner.user_scope, request)
            .map_err(bot_failure)?
            .filter(|operation| operation.bot == bot && operation.owner == owner)
            .ok_or_else(not_found)?;
        self.owned_bot(context, bot)?;
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
        let mut bots = self
            .repository
            .bots_in_lifecycle(owner, workspace, lifecycle)
            .map_err(bot_failure)?;
        self.with_open_operations(&mut bots)?;
        Ok(BotResponse::List { bots })
    }

    /// Fill in each listed Bot's open lifecycle operation.
    pub(super) fn with_open_operations(&self, bots: &mut [Bot]) -> ApiResult<()> {
        for bot in bots {
            bot.open_operation = self
                .repository
                .bot_open_lifecycle(bot.id)
                .map_err(bot_failure)?;
        }
        Ok(())
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
        let (bot, _) = self.owned_bot(context, bot_id)?;
        let fingerprint =
            blake3::hash(&serde_json::to_vec(command).map_err(|_| bot_failure(BotError::Invalid))?)
                .to_hex()
                .to_string();
        Ok((owner, bot, fingerprint))
    }

    /// Archive: capture, or answer a repeat with its progress. A parked
    /// Archive is resumed by its id: the exact request does it, and so does
    /// one carrying only the same id (and Bot), so a client that found the id
    /// through the Bot's read need not reproduce the original revision.
    /// Nothing is captured twice.
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
        let (owner, bot, fingerprint) = self.lifecycle_subject(context, command, *bot_id)?;
        let existing = self
            .repository
            .bot_lifecycle_operation(owner.user_scope, *request_id)
            .map_err(bot_failure)?;
        // A new Archive of a Bot that is already archived.
        if existing.is_none() && bot.lifecycle == BotLifecycle::Archived {
            return Err(archived_failure());
        }
        if self.bot_delegate.is_none() || self.provider_admin.is_none() {
            return Err(bot_failure(BotError::Unavailable));
        }
        let key = (owner.user_scope, *request_id);
        let now = Timestamp::now().millis();
        let operation = match existing {
            Some(op) => {
                let parked = op.kind == BotLifecycleKind::Archive
                    && op.stage == BotLifecycleStage::Incomplete;
                if op.bot != *bot_id
                    || op.owner != owner
                    || (op.fingerprint != fingerprint && !parked)
                {
                    return Err(bot_failure(BotError::Conflict));
                }
                if parked {
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

    /// The native-policy evidence for an active Bot while an Archive (or
    /// later Trash) is unfinished: its own effects-off barrier, so no native
    /// effect starts under the Bot being settled. `None` only when no such
    /// operation exists. A journal that cannot be read refuses (fails
    /// closed): the ordinary effects-allowed evidence is never the fallback.
    pub(in crate::worker) fn lifecycle_policy_barrier(
        &self,
        bot: &Bot,
    ) -> ApiResult<Option<BotPolicyBarrier>> {
        lifecycle_evidence(
            self.repository.open_bot_lifecycle(bot.id),
            |scope, operation| self.repository.bot_lifecycle_operation(scope, operation),
            bot,
        )
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
            .filter(|(_, drive)| drive.due(now))
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
        if drive.wait(Instant::now()) {
            self.park_bot_lifecycle(key, refusal);
        }
    }

    /// Put off for a reason that is not the operation's; never parks.
    fn defer_bot_lifecycle(&mut self, key: OperationKey) {
        if let Some(drive) = self.bot_lifecycle.driving.get_mut(&key) {
            drive.defer(Instant::now());
        }
    }

    /// Park as `incomplete` with its reason, and stop driving it only once
    /// the journal says so. A park that fails leaves the operation driven
    /// and retried, never dropped, since a dropped moving operation would sit
    /// undriven until a restart.
    fn park_bot_lifecycle(&mut self, key: OperationKey, refusal: BotLifecycleRefusal) {
        match self
            .repository
            .park_bot_lifecycle(key.0, key.1, refusal, Timestamp::now().millis())
        {
            Ok(parked) => {
                self.bot_lifecycle.driving.remove(&key);
                if parked {
                    tracing::info!(target: "koru_ai::bots", operation = %key.1, refusal = refusal.token(), "Bot lifecycle operation parked incomplete");
                }
            }
            Err(error) => {
                tracing::warn!(target: "koru_ai::bots", ?error, operation = %key.1, "a Bot lifecycle operation could not be parked; it is retried");
                if let Some(drive) = self.bot_lifecycle.driving.get_mut(&key) {
                    drive.defer(Instant::now());
                }
            }
        }
    }

    fn progressed_bot_lifecycle(&mut self, key: OperationKey) {
        if let Some(drive) = self.bot_lifecycle.driving.get_mut(&key) {
            drive.progressed();
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
            // The pool is full: a later round tries again, each one waiting
            // longer, without counting toward parking.
            Err(_) => self.defer_bot_lifecycle(key),
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

    /// Submissions of the Bot its owner has not acknowledged yet. Each is
    /// handed to the existing coordination (which submits or reconciles it;
    /// the paused owner refuses a new admission). `None` when there are none.
    fn unresolved_bot_submissions(&mut self, op: &LifecycleOperation, bot: &Bot) -> Option<bool> {
        let queued = match self
            .repository
            .bot_unresolved_submissions(op.bot, QUEUED_PAGE)
        {
            Ok(queued) => queued,
            Err(_) => return Some(false),
        };
        if queued.is_empty() {
            return None;
        }
        for (scope, request) in queued {
            if scope != op.owner.user_scope {
                continue;
            }
            if let Ok(Some(receipt)) = self
                .repository
                .bot_admission_receipt(op.owner, op.bot, request)
            {
                self.queue_bot_receipt(bot, receipt, None);
            }
        }
        Some(true)
    }

    /// The Runs of this Bot that this writer is driving as live conversation
    /// tasks, by Run, with the attempt each belongs to.
    fn live_bot_tasks(&self, bot: BotId) -> HashMap<RunId, InferenceAttemptId> {
        self.bot_chats
            .iter()
            .filter_map(|(id, chat)| {
                let (run, ..) = chat.lifecycle_view();
                (run.bot == bot).then_some((run.run, *id))
            })
            .collect()
    }

    /// `settling`: resolve unacknowledged submissions, then ask the delegate
    /// what of the Bot is unsettled; the answer is applied in
    /// [`Self::complete_bot_lifecycle`].
    fn step_archive_settle(&mut self, key: OperationKey, op: &LifecycleOperation, bot: &Bot) {
        // A restart's conversation recovery is still loading interrupted
        // tasks. Not this operation's doing, so it does not count to parking.
        if self.bot_chat_recovery.is_some() {
            return self.defer_bot_lifecycle(key);
        }
        match self.unresolved_bot_submissions(op, bot) {
            Some(true) => return self.wait_bot_lifecycle(key, BotLifecycleRefusal::WorkRunning),
            Some(false) => {
                return self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable);
            }
            None => {}
        }
        let live = self.live_bot_tasks(op.bot).into_keys().collect();
        self.submit_bot_lifecycle(key, op, bot, Step::Settle { bot: op.bot, live });
    }

    /// The delegate's account of the Bot's unsettled work, applied.
    fn settle_bot_archive(
        &mut self,
        key: OperationKey,
        op: &LifecycleOperation,
        unsettled: Unsettled,
    ) {
        if unsettled.held {
            // O27: never end what a person holds. Parked; resuming it once
            // they hand the session back finishes it.
            return self.park_bot_lifecycle(key, BotLifecycleRefusal::PersonHolding);
        }
        // A live conversation task the delegate still runs ends as an
        // automatic interruption through its own finishing lane.
        let live = self.live_bot_tasks(op.bot);
        for run in &unsettled.runs {
            if let Some(attempt) = live.get(run) {
                self.interrupt_bot_chat_for_lifecycle(*attempt);
            }
        }
        // Live tasks the delegate already settled are still delivering
        // their history; the Archive waits for them too.
        let remaining = unsettled.runs.len() + usize::from(unsettled.more) + live.len();
        if remaining > 0 {
            let progressed = self
                .bot_lifecycle
                .driving
                .get(&key)
                .and_then(|drive| drive.remaining)
                .is_some_and(|before| remaining < before);
            if let Some(drive) = self.bot_lifecycle.driving.get_mut(&key) {
                drive.remaining = Some(remaining);
            }
            return if progressed {
                self.progressed_bot_lifecycle(key);
            } else {
                self.wait_bot_lifecycle(key, BotLifecycleRefusal::WorkRunning)
            };
        }
        let Ok(Some(bot)) = self.repository.bot(op.bot) else {
            return self.wait_bot_lifecycle(key, BotLifecycleRefusal::RevisionChanged);
        };
        if op.policy_barrier != Some(koru_ai_core::bots::BotLifecycleBarrierState::Effective) {
            return match lifecycle_barrier(op, &bot) {
                Some(barrier) => self.submit_bot_lifecycle(key, op, &bot, Step::Policy(barrier)),
                None => self.wait_bot_lifecycle(key, BotLifecycleRefusal::MemberConflict),
            };
        }
        // The delegate has just said nothing is unsettled, and no
        // submission is waiting to reach it.
        if !matches!(self.repository.bot_unresolved_submissions(op.bot, 1), Ok(rows) if rows.is_empty())
        {
            return self.wait_bot_lifecycle(key, BotLifecycleRefusal::WorkRunning);
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
            Outcome::Policy(Err(_)) | Outcome::Settle(Err(_)) => {
                self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable)
            }
            Outcome::Settle(Ok(unsettled)) => self.settle_bot_archive(key, &op, unsettled),
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

/// The Bot a command is proven against: an active one, or an archived one,
/// so that the owner's authority over its workspace is inspected before an
/// archived Bot either answers (a read, its history being kept in the
/// Archived filter, EXPERIENCE §3) or refuses with `bot_archived` (any
/// change, see [`archived_change`]). A trashed or missing Bot proves nothing.
pub(super) fn provable_bot(
    repository: &Repository,
    id: BotId,
    _command: &WorkerCommand,
) -> Result<Option<Bot>, BotError> {
    readable_bot(repository, id)
}

/// A Bot whose history may be read: active or archived. `BotRepository::bot`
/// answers an inactive Bot as forbidden; this reads the lifecycle itself.
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
        BotHold::Pending | BotHold::Parked => matches!(
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
        // The same code: the subject is held either way. The Bot's read names
        // the parked operation, to resume it or reverse it.
        BotHold::Parked => failure(
            bot_lifecycle_codes::PENDING,
            "An archive, move to Trash or restore of this Bot stopped before it finished and is waiting for you: resume it or reverse it.",
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

#[cfg(test)]
mod tests;
