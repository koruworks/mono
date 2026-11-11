// SPDX-License-Identifier: MPL-2.0
//! Bot lifecycle commands, the holds they place, and the driver for Archive
//! and Trash (P1.7).
//!
//! The journal is the durable state; the writer only schedules. Archive and
//! Trash of a Bot are captured in one AI transaction (intent, members, the
//! existing pause receipt). The driver then moves it on in finite steps, each
//! owner call a job on the bounded provider-administration pool, each answer
//! applied on the writer before the next step:
//!
//! 1. `captured`: the pause receipt is acknowledged through the existing
//!    coordination, then the effects-off policy barrier is installed. Trash
//!    of an archived Bot has neither (it is already paused and settled) and
//!    goes straight to `settling`, which still asks the delegate.
//! 2. `settling`: work the owner had not admitted when the operation was
//!    captured is refused, never admitted: each attempt not yet ended is
//!    stopped as the person's Stop and its receipt resolved through the
//!    refusal fence, which may move the Bot's revision (the completion
//!    allows that, and nothing else, see `revision_held`). Nothing of a
//!    captured subject is submitted.
//!    Then the delegate is asked, every round, which of the Bot's admissions
//!    it has not settled, and each is stopped as the person's own Stop
//!    (O27, O37): Archive is the person's explicit action, so it ends a
//!    session even while a person holds it, and the task reads as stopped.
//!    A live conversation task goes through its own Stop lane, so its AI
//!    attempt records the Stop; anything else (an API-started task, queued
//!    work, which never starts) is stopped by the driver by key. An
//!    acknowledgement is not proof: the operation waits until the delegate
//!    reports nothing unsettled and the barrier is effective (native
//!    effects drained).
//! 3. One transaction archives the Bot (nothing is hidden or removed), or
//!    trashes it and puts each captured conversation not already in Trash
//!    into Trash under the operation.
//!
//! Trash has no Store and no delegate-side effect in this version (O41): it
//! hides the Bot and its conversations and leaves every page, Run record,
//! folder and credential where it is, which its result says
//! (`left_in_place`). No Store function and no delegate `Trash`/`Restore`
//! is called.
//!
//! Trash of one conversation captures only it and settles only its work,
//! with no pause and no Bot-wide barrier, so the Bot's other conversations
//! keep running: no submission of it is waiting (each is refused as above),
//! the delegate reports none
//! of its Runs unsettled (each it reports is stopped as the person's Stop),
//! no live task of it remains, and the delegate holds no native effect for
//! any of its Runs. Then one transaction puts it into Trash.
//!
//! A restart resumes from the journal's stage, never recaptures, and re-asks
//! every owner idempotently; nothing the driver remembers decides what has
//! settled. An operation that cannot progress is parked `incomplete` with
//! its reason; it is never retried on its own. The Bot's read names it, and
//! the same request, or one naming only its id, resumes it. Unarchive, the
//! Restore of a Trash and the Restore of a conversation's Trash are each
//! applied in one transaction with no owner call; the Bot comes back paused
//! and nothing starts. Purge refuses until its slice.
use super::*;
use koru_ai_sqlite::{BotHold, LifecycleCapture, LifecycleMemberDraft, LifecycleOperation};
use koru_ai_wire::bot_lifecycle_codes;
use koru_delegate_api::bots::{BotCommand as DelegateCommand, BotDelegatePort, BotReply};
use koru_delegate_core::{
    BotAdmissionView, BotExecutionFence, BotPolicyBarrier, BotPolicyBarrierKey,
    BotPolicyBarrierState, BotRunKey, NativeDrainReason, RunId,
};

type OperationKey = (UserScopeId, BotRequestId);

/// Consecutive rounds that made no progress before an operation is parked.
/// The waits double from 250 ms to 32 s, so this is a few minutes.
const PARK_AFTER: u32 = 12;
/// The most queued admissions one delegate read returns. A Bot holds at
/// most 50 queued (`BOT_PENDING_LIMIT`), so one page is normally all of them.
const UNSETTLED_PAGE: u16 = 64;
/// The most pages of a Bot's held native effects one conversation's
/// settling read asks for; past them the conversation counts as unsettled.
const EFFECT_PAGES: usize = 4;

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
    /// Ask the delegate what of the Bot it has not settled, then stop, as
    /// the person's Stop, each of those that is not a live conversation task
    /// of this writer (`live`), which stops through its own lane.
    Settle { bot: BotId, live: HashSet<RunId> },
    /// One conversation's settling read: the Bot's unsettled admissions and
    /// the native effects the delegate still holds for it. The writer
    /// narrows both to the conversation's own Runs, which only it can name.
    ChatRead { bot: BotId },
    /// The person's Stop for each of the conversation's Runs the last read
    /// reported unsettled that no live conversation task drives.
    Stop {
        bot: BotId,
        runs: Vec<(RunId, Option<BotExecutionFence>)>,
    },
}
pub(in crate::worker) struct LifecycleResult {
    key: OperationKey,
    outcome: Outcome,
}
enum Outcome {
    Policy(Result<BotPolicyBarrierState, koru_delegate_api::Failure>),
    Settle(Result<Unsettled, koru_delegate_api::Failure>),
    ChatRead(Result<ChatWork, koru_delegate_api::Failure>),
    /// The Stops went out; their answers are read again next round.
    Stopped,
}
/// The delegate's answer for one conversation's settling round, for the
/// whole Bot: the writer keeps what belongs to the conversation.
struct ChatWork {
    admissions: Vec<BotAdmissionView>,
    /// More queued work exists past the page.
    more: bool,
    /// The Run of every native effect the delegate still holds for the Bot.
    effects: Vec<RunId>,
    /// More held effects exist past the pages read.
    effects_more: bool,
}
/// The delegate's answer for one settling round.
struct Unsettled {
    /// Every Run it reported not settled, before this round's Stops.
    runs: Vec<RunId>,
    /// More queued work exists past the page.
    more: bool,
}

fn unavailable() -> koru_delegate_api::Failure {
    koru_delegate_api::Failure::new("bot_unavailable", "Bot lifecycle owner unavailable.", true)
}

impl LifecycleJob {
    pub(in crate::worker) fn run(self) -> LifecycleResult {
        let key = self.key;
        let failed = match &self.step {
            Step::Policy(_) => Outcome::Policy(Err(unavailable())),
            Step::Settle { .. } => Outcome::Settle(Err(unavailable())),
            Step::ChatRead { .. } => Outcome::ChatRead(Err(unavailable())),
            Step::Stop { .. } => Outcome::Stopped,
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| self.execute())).unwrap_or(failed);
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
                    // Whether a person holds a session (`held`) is not a
                    // gate: Archive is the person's own action and ends it.
                    for unsettled in &work.admissions {
                        let admission = &unsettled.admission;
                        if live.contains(&admission.run) {
                            continue;
                        }
                        // The person's Stop with the owner's current fence. A
                        // queued Run is cancelled without starting; an active
                        // one ends even under a takeover, and keeps its slot
                        // until the owner proves its process ended. An
                        // unconfirmed or refused answer is read again next
                        // round, not remembered.
                        let _ = self.delegate.execute(DelegateCommand::Stop {
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
                    })
                }))
            }
            Step::ChatRead { bot } => {
                let owner = delegate_owner(self.owner);
                Outcome::ChatRead(checked.ok_or_else(unavailable).and_then(|_| {
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
                    // Held native effects of the Bot, a bounded number of
                    // pages. Any of the conversation's keeps it unsettled.
                    let mut effects = Vec::new();
                    let mut after = None;
                    let mut effects_more = false;
                    for page in 0..EFFECT_PAGES {
                        let BotReply::NativePendingEffects { page: found } = self
                            .delegate
                            .execute(DelegateCommand::PendingNativeEffects {
                                owner,
                                bot: Some(bot),
                                after: after.take(),
                                limit: UNSETTLED_PAGE,
                            })?
                        else {
                            return Err(unavailable());
                        };
                        if found
                            .targets
                            .iter()
                            .any(|t| t.owner != owner || t.bot != bot)
                        {
                            return Err(unavailable());
                        }
                        effects.extend(found.targets.iter().map(|t| t.key.run));
                        match found.next {
                            Some(next) if page + 1 < EFFECT_PAGES => after = Some(next),
                            Some(_) => effects_more = true,
                            None => break,
                        }
                    }
                    Ok(ChatWork {
                        admissions: work.admissions.into_iter().map(|a| a.admission).collect(),
                        more: work.more,
                        effects,
                        effects_more,
                    })
                }))
            }
            Step::Stop { bot, runs } => {
                let owner = delegate_owner(self.owner);
                if checked.is_some() {
                    for (run, fence) in runs {
                        // The person's Stop with the owner's reported fence;
                        // whatever it answers, the next read says what ended.
                        let _ = self.delegate.execute(DelegateCommand::Stop {
                            key: BotRunKey { owner, bot, run },
                            fence,
                        });
                    }
                }
                Outcome::Stopped
            }
        }
    }
}

/// The conversation a Start/Continue receipt's submission names; a pause
/// receipt names none.
fn submission_chat(receipt: &BotAdmissionReceipt) -> Option<Uuid> {
    match receipt {
        BotAdmissionReceipt::Submission(r) => Some(r.intent.submission.provenance.chat_id),
        BotAdmissionReceipt::Pause(_) => None,
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
            // P1.8 creation reads: a completed creation stays readable
            // whatever the Bot's lifecycle is now.
            | BotCommand::ReadBotCreation { .. }
            | BotCommand::ListBotCreations { .. }
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
            // Changes to a creation, not to a Bot: one that made a Bot is
            // completed, and then Retry only reports it and Abandon is
            // `request_conflict`, so neither can change an archived or
            // trashed Bot. A creation's BotId has no Bot row until it
            // completes, and lifecycle acts only on Bots that exist.
            BotCommand::RetryBotCreation { .. } | BotCommand::AbandonBotCreation { .. } => None,
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
            BotCommand::Archive { .. } | BotCommand::Trash { .. } => {
                Some(self.archive_bot(context, command))
            }
            BotCommand::Restore { .. } => Some(self.restore_bot(context, command)),
            BotCommand::TrashChat { .. } => Some(self.trash_bot_chat(context, command)),
            BotCommand::RestoreChat { .. } => Some(self.restore_bot_chat(context, command)),
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

    /// An archived Bot's admission receipt reads as it was left. A trashed
    /// Bot's reads as missing once its owner is known, as every read of a
    /// trashed Bot does. An active Bot's goes through the ordinary admission
    /// path (`None`).
    fn read_archived_operation(
        &self,
        context: &CommandContext,
        request: BotRequestId,
        id: BotId,
    ) -> ApiResult<Option<BotResponse>> {
        match self.repository.bot_any_lifecycle(id).map_err(bot_failure)? {
            Some((_, BotLifecycle::Archived)) => {}
            Some((_, BotLifecycle::Trashed)) => {
                self.bot_of_owner(context, id)?;
                return Err(not_found());
            }
            _ => return Ok(None),
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

    /// A repeat of a lifecycle command under an operation that already
    /// exists: its progress. A parked operation is resumed by its id: the
    /// exact request does it, and so does one carrying only the same id (and
    /// Bot and conversation) under the same kind, so a client that found the
    /// id through the Bot's read need not reproduce the original revision.
    /// Anything else under that id is `request_conflict`.
    fn repeat_bot_lifecycle(
        &mut self,
        op: LifecycleOperation,
        (owner, bot): (BotOwner, BotId),
        kind: BotLifecycleKind,
        chat: Option<ChatId>,
        fingerprint: &str,
    ) -> ApiResult<LifecycleOperation> {
        let parked = op.kind == kind && op.stage == BotLifecycleStage::Incomplete;
        if op.bot != bot
            || op.owner != owner
            || op.chat != chat
            || (op.fingerprint != fingerprint && !parked)
        {
            return Err(bot_failure(BotError::Conflict));
        }
        if parked {
            self.repository
                .resume_bot_lifecycle(op.owner.user_scope, op.operation, Timestamp::now().millis())
                .map_err(bot_failure)?;
        }
        self.repository
            .bot_lifecycle_operation(op.owner.user_scope, op.operation)
            .map_err(bot_failure)?
            .ok_or_else(not_found)
    }

    /// Archive or Trash of a whole Bot: capture, or answer a repeat with its
    /// progress (see [`Self::repeat_bot_lifecycle`]). Nothing is captured
    /// twice. Both pause an active Bot and stop its work as the person's
    /// Stop; Trash then hides the Bot and its conversations and moves no
    /// page or Run record (O41). An archived Bot can be trashed: it is
    /// already paused and settled, and the delegate is still asked.
    fn archive_bot(
        &mut self,
        context: &CommandContext,
        command: &BotCommand,
    ) -> ApiResult<BotResponse> {
        let (kind, request_id, bot_id, expected_revision) = match command {
            BotCommand::Archive {
                request_id,
                bot_id,
                expected_revision,
            } => (
                BotLifecycleKind::Archive,
                request_id,
                bot_id,
                expected_revision,
            ),
            BotCommand::Trash {
                request_id,
                bot_id,
                expected_revision,
            } => (
                BotLifecycleKind::Trash,
                request_id,
                bot_id,
                expected_revision,
            ),
            _ => return Err(not_found()),
        };
        let (owner, bot, fingerprint) = self.lifecycle_subject(context, command, *bot_id)?;
        let existing = self
            .repository
            .bot_lifecycle_operation(owner.user_scope, *request_id)
            .map_err(bot_failure)?;
        if existing.is_none() {
            match (kind, bot.lifecycle) {
                // Nothing changes a trashed Bot but the Restore of its Trash.
                (_, BotLifecycle::Trashed) => return Err(hold_failure(BotHold::Trashed)),
                (BotLifecycleKind::Archive, BotLifecycle::Archived) => {
                    return Err(archived_failure());
                }
                _ => {}
            }
        }
        if self.bot_delegate.is_none() || self.provider_admin.is_none() {
            return Err(bot_failure(BotError::Unavailable));
        }
        let key = (owner.user_scope, *request_id);
        let now = Timestamp::now().millis();
        let operation = match existing {
            Some(op) => {
                self.repeat_bot_lifecycle(op, (owner, *bot_id), kind, None, &fingerprint)?
            }
            None => {
                let members = std::iter::once(LifecycleMemberDraft {
                    subject: BotLifecycleSubject::Bot,
                    reason: BotLifecycleReason::Bot,
                    prior_state: if bot.lifecycle == BotLifecycle::Archived {
                        BotLifecyclePriorState::Archived
                    } else {
                        BotLifecyclePriorState::Live
                    },
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
                        kind,
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

    /// Restore naming an Archive (Unarchive) or a whole-Bot Trash: one AI
    /// transaction, no owner call. Unarchive returns the Bot active; the
    /// Restore of a Trash returns it to the lifecycle it was trashed from
    /// (active, or archived) with exactly the conversations that Trash put
    /// into Trash. Either way it stays paused and nothing starts.
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
        let (owner, bot, fingerprint) = self.lifecycle_subject(context, command, *bot_id)?;
        let source = self
            .repository
            .bot_lifecycle_operation(owner.user_scope, *source_request_id)
            .map_err(bot_failure)?
            .filter(|source| source.bot == *bot_id && source.owner == owner)
            .ok_or_else(not_found)?;
        let restore: fn(
            &mut Repository,
            &LifecycleCapture,
        ) -> Result<LifecycleOperation, BotError> = match source.kind {
            BotLifecycleKind::Archive => Repository::unarchive_bot,
            BotLifecycleKind::Trash => Repository::restore_trashed_bot,
            BotLifecycleKind::PurgeBot | BotLifecycleKind::PurgeChat => {
                return Err(lifecycle_unavailable());
            }
            // A conversation's Trash is reversed by `restore_chat`.
            _ => return Err(bot_failure(BotError::Invalid)),
        };
        // A moving Archive or Trash is reversed once it completes or parks itself.
        let replay = self
            .repository
            .bot_lifecycle_operation(owner.user_scope, *request_id)
            .map_err(bot_failure)?
            .is_some();
        if !replay {
            // A trashed Bot comes back only through its Trash's Restore.
            if bot.lifecycle == BotLifecycle::Trashed && source.kind != BotLifecycleKind::Trash {
                return Err(hold_failure(BotHold::Trashed));
            }
            if !matches!(
                source.stage,
                BotLifecycleStage::Completed | BotLifecycleStage::Incomplete
            ) {
                return Err(hold_failure(BotHold::Pending));
            }
        }
        let operation = restore(
            &mut self.repository,
            &LifecycleCapture {
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
            },
        )
        .map_err(bot_failure)?;
        self.bot_lifecycle
            .driving
            .remove(&(owner.user_scope, *source_request_id));
        Ok(BotResponse::Lifecycle {
            operation: operation.progress(),
        })
    }

    /// Trash of one conversation: capture only it, or answer a repeat with
    /// its progress (see [`Self::repeat_bot_lifecycle`]). The Bot is not
    /// paused and gets no barrier; the open operation holds that conversation
    /// alone, and the driver settles only its work. The main conversation is
    /// trashed the same way: its association stays and no replacement is
    /// made (O7).
    fn trash_bot_chat(
        &mut self,
        context: &CommandContext,
        command: &BotCommand,
    ) -> ApiResult<BotResponse> {
        let BotCommand::TrashChat {
            request_id,
            bot_id,
            chat_id,
            expected_revision,
        } = command
        else {
            return Err(not_found());
        };
        let (owner, bot, fingerprint) = self.lifecycle_subject(context, command, *bot_id)?;
        if self
            .repository
            .bot_for_chat(*chat_id)
            .map_err(bot_failure)?
            != Some(*bot_id)
        {
            return Err(not_found());
        }
        let existing = self
            .repository
            .bot_lifecycle_operation(owner.user_scope, *request_id)
            .map_err(bot_failure)?;
        if existing.is_none() {
            match bot.lifecycle {
                BotLifecycle::Trashed => return Err(hold_failure(BotHold::Trashed)),
                BotLifecycle::Archived => return Err(archived_failure()),
                BotLifecycle::Active => {}
            }
            if self
                .repository
                .bot_chat_in_trash(*chat_id)
                .map_err(bot_failure)?
            {
                return Err(hold_failure(BotHold::Trashed));
            }
        }
        if self.bot_delegate.is_none() || self.provider_admin.is_none() {
            return Err(bot_failure(BotError::Unavailable));
        }
        let operation = match existing {
            Some(op) => self.repeat_bot_lifecycle(
                op,
                (owner, *bot_id),
                BotLifecycleKind::TrashChat,
                Some(*chat_id),
                &fingerprint,
            )?,
            None => self
                .repository
                .capture_bot_lifecycle(&LifecycleCapture {
                    operation: *request_id,
                    owner,
                    bot: *bot_id,
                    kind: BotLifecycleKind::TrashChat,
                    chat: Some(*chat_id),
                    fingerprint,
                    expected_revision: *expected_revision,
                    source: None,
                    members: vec![LifecycleMemberDraft {
                        subject: BotLifecycleSubject::Chat { chat_id: *chat_id },
                        reason: BotLifecycleReason::OwnedConversation,
                        prior_state: BotLifecyclePriorState::Live,
                        intended: BotLifecycleIntended::Hide,
                        retained: None,
                    }],
                    now: Timestamp::now().millis(),
                })
                .map_err(bot_failure)?,
        };
        if !matches!(
            operation.stage,
            BotLifecycleStage::Completed | BotLifecycleStage::Incomplete
        ) {
            self.bot_lifecycle
                .driving
                .entry((owner.user_scope, *request_id))
                .or_default();
        }
        Ok(BotResponse::Lifecycle {
            operation: operation.progress(),
        })
    }

    /// Restore of one conversation's Trash, completed or parked: one AI
    /// transaction removes exactly that operation's Trash row, so the same
    /// conversation and history come back. Nothing runs. A conversation that
    /// a whole-Bot Trash captured as already in Trash comes back only
    /// through this, its own operation's Restore.
    fn restore_bot_chat(
        &mut self,
        context: &CommandContext,
        command: &BotCommand,
    ) -> ApiResult<BotResponse> {
        let BotCommand::RestoreChat {
            request_id,
            bot_id,
            chat_id,
            expected_revision,
            source_request_id,
        } = command
        else {
            return Err(not_found());
        };
        let (owner, bot, fingerprint) = self.lifecycle_subject(context, command, *bot_id)?;
        let source = self
            .repository
            .bot_lifecycle_operation(owner.user_scope, *source_request_id)
            .map_err(bot_failure)?
            .filter(|source| source.bot == *bot_id && source.owner == owner)
            .ok_or_else(not_found)?;
        if source.kind != BotLifecycleKind::TrashChat || source.chat != Some(*chat_id) {
            return Err(bot_failure(BotError::Invalid));
        }
        let replay = self
            .repository
            .bot_lifecycle_operation(owner.user_scope, *request_id)
            .map_err(bot_failure)?
            .is_some();
        if !replay {
            match bot.lifecycle {
                BotLifecycle::Trashed => return Err(hold_failure(BotHold::Trashed)),
                BotLifecycle::Archived => return Err(archived_failure()),
                BotLifecycle::Active => {}
            }
            // A moving conversation Trash is reversed once it completes or parks.
            if !matches!(
                source.stage,
                BotLifecycleStage::Completed | BotLifecycleStage::Incomplete
            ) {
                return Err(hold_failure(BotHold::Pending));
            }
        }
        let operation = self
            .repository
            .restore_bot_chat(&LifecycleCapture {
                operation: *request_id,
                owner,
                bot: *bot_id,
                kind: BotLifecycleKind::RestoreChat,
                chat: Some(*chat_id),
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

    /// The native-policy evidence for an active Bot while an Archive or a
    /// Trash is unfinished: its own effects-off barrier, so no native
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
        // Archive, Trash and Trash of one conversation are driven; Restore
        // is applied at once, and Purge is refused at capture.
        if !matches!(
            op.kind,
            BotLifecycleKind::Archive | BotLifecycleKind::Trash | BotLifecycleKind::TrashChat
        ) || matches!(
            op.stage,
            BotLifecycleStage::Completed | BotLifecycleStage::Incomplete
        ) {
            self.bot_lifecycle.driving.remove(&key);
            return;
        }
        let Some(bot) = self.lifecycle_bot(&op) else {
            // The Bot left the lifecycle the operation was captured in.
            return self.wait_bot_lifecycle(key, BotLifecycleRefusal::RevisionChanged);
        };
        match (op.stage, op.kind) {
            (BotLifecycleStage::Captured, _) if op.pause_request.is_some() => {
                self.step_archive_barrier(key, &op, &bot)
            }
            // Nothing to pause: Trash of one conversation, or of an archived
            // Bot (already paused and settled). Settling still asks the owner.
            (BotLifecycleStage::Captured, _) => match self.repository.advance_bot_lifecycle(
                key.0,
                key.1,
                BotLifecycleStage::Captured,
                BotLifecycleStage::Settling,
                false,
                Timestamp::now().millis(),
            ) {
                Ok(true) => self.progressed_bot_lifecycle(key),
                _ => self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable),
            },
            (BotLifecycleStage::Settling, BotLifecycleKind::TrashChat) => {
                self.step_chat_settle(key, &op, &bot)
            }
            (BotLifecycleStage::Settling, _) => self.step_archive_settle(key, &op, &bot),
            _ => {
                self.bot_lifecycle.driving.remove(&key);
            }
        }
    }

    /// The Bot an operation acts on, while it still has the lifecycle the
    /// operation needs: a Trash keeps the one it was captured in (active, or
    /// archived); Archive and a conversation's Trash need an active Bot.
    fn lifecycle_bot(&self, op: &LifecycleOperation) -> Option<Bot> {
        let needed = match op.kind {
            BotLifecycleKind::Trash => op.prior_lifecycle,
            _ => BotLifecycle::Active,
        };
        match self.repository.bot_any_lifecycle(op.bot) {
            Ok(Some((bot, lifecycle))) if lifecycle == needed => Some(bot),
            _ => None,
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

    /// Work of the operation's subject (the whole Bot, or one conversation)
    /// that its owner had not admitted when the operation was captured:
    /// the Bot's Start/Continue receipts still waiting to reach the
    /// delegate. Lifecycle refuses it and never admits it. Each attempt not
    /// yet ended is stopped through its own Stop path ([`Self::stop_attempt`],
    /// the person's Stop, which also cancels a context job still in flight, so
    /// nothing can start a live task for it afterwards). Then its receipt,
    /// like one with no attempt (an API `start`), goes to the existing
    /// coordination, which never submits a held subject's receipt
    /// (`apply_result`) and so resolves it through the refusal fence: Missing
    /// after the fence is proof it was never admitted. `None` when there is
    /// none; `Some(false)` when the journal could not be read. A full page
    /// might hide one, so it counts as unresolved.
    fn refuse_unadmitted(
        &mut self,
        op: &LifecycleOperation,
        bot: &Bot,
        chat: Option<ChatId>,
    ) -> Option<bool> {
        let queued = match self
            .repository
            .bot_unresolved_submissions(op.bot, UNSETTLED_PAGE)
        {
            Ok(queued) => queued,
            Err(_) => return Some(false),
        };
        let mut unresolved = queued.len() >= usize::from(UNSETTLED_PAGE);
        for (scope, request) in queued {
            if scope != op.owner.user_scope {
                continue;
            }
            let receipt = match self
                .repository
                .bot_admission_receipt(op.owner, op.bot, request)
            {
                Ok(Some(BotAdmissionReceipt::Submission(receipt))) => receipt,
                Ok(_) => continue,
                Err(_) => return Some(false),
            };
            if chat
                .is_some_and(|chat| receipt.intent.submission.provenance.chat_id != chat.as_uuid())
            {
                continue;
            }
            if let Some(attempt) = super::admission::unended_attempt(&self.repository, &receipt) {
                // Whatever it answers, the receipt below is resolved the
                // same way, and the next round reads the attempt again.
                let _ = self.stop_attempt(attempt);
            }
            self.queue_bot_receipt(bot, BotAdmissionReceipt::Submission(receipt), None);
            unresolved = true;
        }
        unresolved.then_some(true)
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

    /// `settling`: refuse the work not admitted at capture, then ask the delegate
    /// what of the Bot is unsettled; the answer is applied in
    /// [`Self::complete_bot_lifecycle`].
    fn step_archive_settle(&mut self, key: OperationKey, op: &LifecycleOperation, bot: &Bot) {
        // A restart's conversation recovery is still loading interrupted
        // tasks. Not this operation's doing, so it does not count to parking.
        if self.bot_chat_recovery.is_some() {
            return self.defer_bot_lifecycle(key);
        }
        match self.refuse_unadmitted(op, bot, None) {
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
        // A live conversation task the delegate still runs is stopped as the
        // person's Stop through its own lane, whatever it already ended as
        // (running, interrupted, failed, natively cancelled), so a session a
        // person holds is ended too (O27, O37).
        let live = self.live_bot_tasks(op.bot);
        for run in &unsettled.runs {
            if let Some(attempt) = live.get(run) {
                self.stop_bot_chat_for_lifecycle(*attempt);
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
        let Some(bot) = self.lifecycle_bot(op) else {
            return self.wait_bot_lifecycle(key, BotLifecycleRefusal::RevisionChanged);
        };
        // An operation that paused the Bot finishes only once its
        // effects-off barrier is effective (native effects drained). Trash of
        // an archived Bot installed none: Archive already settled it.
        if op.policy_request.is_some()
            && op.policy_barrier != Some(koru_ai_core::bots::BotLifecycleBarrierState::Effective)
        {
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
        let now = Timestamp::now().millis();
        let completed = match op.kind {
            BotLifecycleKind::Trash => self.repository.complete_bot_trash(key.0, key.1, now),
            _ => self.repository.complete_bot_archive(key.0, key.1, now),
        };
        match completed {
            Ok(done) => {
                tracing::info!(target: "koru_ai::bots", operation = %key.1, kind = done.kind.token(), "Bot lifecycle operation completed");
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
            Outcome::Policy(Err(_)) | Outcome::Settle(Err(_)) | Outcome::ChatRead(Err(_)) => {
                self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable)
            }
            Outcome::Settle(Ok(unsettled)) => self.settle_bot_archive(key, &op, unsettled),
            Outcome::ChatRead(Ok(work)) => self.settle_bot_chat_trash(key, &op, work),
            // The round was already counted when its read was applied; the
            // next read says what the Stops ended.
            Outcome::Stopped => {}
        }
    }

    /// `settling` of one conversation's Trash: that conversation's work its
    /// owner had not admitted is refused first ([`Self::refuse_unadmitted`];
    /// the conversation's hold refuses new work, so the set closes). Then the
    /// delegate is asked what of the Bot is unsettled and which native
    /// effects it holds, and [`Self::settle_bot_chat_trash`] keeps the
    /// conversation's.
    fn step_chat_settle(&mut self, key: OperationKey, op: &LifecycleOperation, bot: &Bot) {
        if self.bot_chat_recovery.is_some() {
            return self.defer_bot_lifecycle(key);
        }
        let Some(chat) = op.chat else {
            return self.wait_bot_lifecycle(key, BotLifecycleRefusal::MemberConflict);
        };
        match self.refuse_unadmitted(op, bot, Some(chat)) {
            Some(true) => return self.wait_bot_lifecycle(key, BotLifecycleRefusal::WorkRunning),
            Some(false) => {
                return self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable);
            }
            None => {}
        }
        self.submit_bot_lifecycle(key, op, bot, Step::ChatRead { bot: op.bot });
    }

    /// The Runs of one conversation that this writer drives as live tasks,
    /// with the attempt each belongs to.
    fn live_chat_tasks(&self, bot: BotId, chat: ChatId) -> HashMap<RunId, InferenceAttemptId> {
        self.bot_chats
            .iter()
            .filter_map(|(id, live)| {
                let (run, ..) = live.lifecycle_view();
                (run.bot == bot && live.lifecycle_chat() == chat.as_uuid())
                    .then_some((run.run, *id))
            })
            .collect()
    }

    /// The delegate's account of the Bot's work, narrowed to one
    /// conversation by the AI submission behind each Run, and applied: its
    /// live task is stopped through its own Stop lane, any other of its
    /// unsettled Runs by the person's Stop by key (a queued one never
    /// starts), and a native effect still held for one of its Runs is handed
    /// to the existing drain. The sibling conversations' work is not
    /// touched. Once a round finds none of it, one transaction puts the
    /// conversation into Trash.
    fn settle_bot_chat_trash(
        &mut self,
        key: OperationKey,
        op: &LifecycleOperation,
        work: ChatWork,
    ) {
        let Some(chat) = op.chat else {
            return self.wait_bot_lifecycle(key, BotLifecycleRefusal::MemberConflict);
        };
        let target = chat.as_uuid();
        let mut unsettled = Vec::new();
        for admission in work.admissions {
            match self
                .repository
                .bot_admission_receipt(op.owner, op.bot, admission.key.request)
            {
                Ok(Some(receipt)) if submission_chat(&receipt) == Some(target) => {
                    unsettled.push(admission);
                }
                Ok(_) => {}
                Err(_) => {
                    return self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable);
                }
            }
        }
        let mut effects = Vec::new();
        for run in work.effects {
            match self
                .repository
                .bot_submission_for_run(op.owner, op.bot, run)
            {
                Ok(Some(receipt)) if receipt.intent.submission.provenance.chat_id == target => {
                    effects.push(run);
                }
                Ok(_) => {}
                Err(_) => {
                    return self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable);
                }
            }
        }
        let live = self.live_chat_tasks(op.bot, chat);
        let mut stop = Vec::new();
        for admission in &unsettled {
            match live.get(&admission.run) {
                Some(attempt) => {
                    self.stop_bot_chat_for_lifecycle(*attempt);
                }
                None => stop.push((admission.run, admission.fence.clone())),
            }
        }
        for run in &effects {
            self.queue_native_recovery(
                delegate_owner(op.owner),
                op.bot,
                Some(*run),
                None,
                NativeDrainReason::RunRecovery,
            );
        }
        // A live task the delegate already settled is still delivering its
        // history; the Trash waits for it too.
        let remaining = unsettled.len()
            + live.len()
            + effects.len()
            + usize::from(work.more)
            + usize::from(work.effects_more);
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
            if progressed {
                self.progressed_bot_lifecycle(key);
            } else {
                self.wait_bot_lifecycle(key, BotLifecycleRefusal::WorkRunning);
            }
            // Not after a park: a parked operation sends nothing more.
            if !stop.is_empty()
                && self.bot_lifecycle.driving.contains_key(&key)
                && let Some(bot) = self.lifecycle_bot(op)
            {
                self.submit_bot_lifecycle(
                    key,
                    op,
                    &bot,
                    Step::Stop {
                        bot: op.bot,
                        runs: stop,
                    },
                );
            }
            return;
        }
        // The delegate has just said none of it is unsettled; no submission
        // of it may be waiting to reach the owner either.
        match self.lifecycle_bot(op) {
            Some(bot) => {
                if self.refuse_unadmitted(op, &bot, Some(chat)).is_some() {
                    return self.wait_bot_lifecycle(key, BotLifecycleRefusal::WorkRunning);
                }
            }
            None => return self.wait_bot_lifecycle(key, BotLifecycleRefusal::RevisionChanged),
        }
        match self
            .repository
            .complete_bot_chat_trash(key.0, key.1, Timestamp::now().millis())
        {
            Ok(_) => {
                tracing::info!(target: "koru_ai::bots", operation = %key.1, "Bot conversation moved to Trash");
                self.bot_lifecycle.driving.remove(&key);
            }
            Err(_) => self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable),
        }
    }

    /// A Bot-level command on a held Bot, or a task naming a held
    /// conversation, refuses before it reaches its owner path.
    fn bot_command_hold(&self, context: &CommandContext, command: &BotCommand) -> ApiResult<()> {
        // Reconciling an admission is held only by Trash: it settles work,
        // so an unfinished operation leaves it available.
        let (bot, chat, trash_only) = match command {
            BotCommand::Start {
                bot_id, chat_id, ..
            }
            | BotCommand::Continue {
                bot_id, chat_id, ..
            } => (*bot_id, Some(*chat_id), false),
            BotCommand::CreateChat { bot_id, .. }
            | BotCommand::Update { bot_id, .. }
            | BotCommand::UpdateAccess { bot_id, .. }
            | BotCommand::Pause { bot_id, .. }
            | BotCommand::Resume { bot_id, .. } => (*bot_id, None, false),
            BotCommand::ReconcileOperation { bot_id, .. } => (*bot_id, None, true),
            _ => return Ok(()),
        };
        let hold = match chat {
            Some(chat) if self.repository.bot_for_chat(chat).map_err(bot_failure)? == Some(bot) => {
                self.repository.bot_chat_hold(chat)
            }
            _ => self.repository.bot_hold(bot),
        }
        .map_err(bot_failure)?;
        let Some(hold) = hold.filter(|hold| !trash_only || *hold == BotHold::Trashed) else {
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
/// change, see [`archived_change`]). A trashed Bot is proven only for its own
/// lifecycle commands (the Restore of its Trash, a repeat of that Trash, a
/// progress read), so its owner's workspace authority is inspected before
/// any of them answers; for everything else it proves nothing, as a missing
/// Bot does, and its holds answer.
pub(super) fn provable_bot(
    repository: &Repository,
    id: BotId,
    command: &WorkerCommand,
) -> Result<Option<Bot>, BotError> {
    if let WorkerCommand::Bots { request, .. } = command
        && (request.lifecycle_kind().is_some()
            || matches!(request, BotCommand::ReadLifecycleOperation { .. }))
        && let Some((bot, BotLifecycle::Trashed)) = repository.bot_any_lifecycle(id)?
    {
        return Ok(Some(bot));
    }
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
            "This Bot, or this conversation of it, is in Trash. Restore it to use it again.",
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
