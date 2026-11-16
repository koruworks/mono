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
//! 2. `settling` starts by pausing the Bot's Routines (P4.3), on every
//!    whole-Bot path. Each captured Routine is paused by the delegate's
//!    schedule coordinator, with the revision last read, and its member is
//!    acknowledged only on the coordinator's own answer, never from this
//!    driver's read of the Schedule. The Bot's admission barrier is already
//!    paused when this starts, and the coordinator refuses Enable while it
//!    is, so no Routine is on inside a completed Archive or Trash: an Enable
//!    that raced either landed before this Pause, which turns it off, or is
//!    handled after it and is refused. A Routine another computer owns, or
//!    one whose owner does not answer, parks the operation.
//!    Then work the owner had not admitted when the operation was
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
//! and nothing starts. Its Routines keep the state they have: a Restore
//! turns none on and pauses none, and its answer names each Routine whose
//! access no longer reads and, for an operation that had parked, each one
//! not confirmed paused. Purge refuses until its slice.
use super::*;
use koru_ai_sqlite::{
    BotHold, LifecycleCapture, LifecycleMemberDraft, LifecycleOperation, LifecycleRoutineAnswer,
    ParkedCursor,
};
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
/// A safety stop for one conversation's settling read of the Bot's held
/// native effects, which otherwise follows the delegate's pages to the end:
/// at 64 a page this is 65,536 effects. Past it, or on a cursor that does not
/// advance, the conversation counts as unsettled rather than loop forever.
const EFFECT_PAGE_STOP: usize = 1024;

/// What the writer drives. Nothing here is durable: the journal is, and the
/// delegate is the authority on what still runs.
pub(in crate::worker) struct LifecycleDriver {
    driving: HashMap<OperationKey, Drive>,
    /// Restart resume: the next journal page, `None` once every page is read.
    recovery: Option<Option<(i64, BotRequestId)>>,
    /// The one probe outstanding, while a probe pass is under way: the
    /// operation parked for an owner that did not answer whose owner is
    /// being asked whether it answers again, and how many more parked
    /// operations this pass may still ask (see
    /// [`WorkerState::probe_parked_bot_lifecycle`]).
    probing: Option<(OperationKey, u16)>,
    /// Where the probe walk carries on: just past the last parked operation
    /// it reached, `None` to start (again) from the first.
    probe_after: Option<ParkedCursor>,
    /// The first wait of a round without progress ([`ROUND_UNIT`]; a test
    /// may shorten it so twelve rounds take a second instead of minutes).
    unit: Duration,
}
impl Default for LifecycleDriver {
    fn default() -> Self {
        Self {
            driving: HashMap::new(),
            recovery: Some(None),
            probing: None,
            probe_after: None,
            unit: ROUND_UNIT,
        }
    }
}
#[cfg(test)]
impl LifecycleDriver {
    pub(in crate::worker) fn with_unit(unit: Duration) -> Self {
        Self {
            unit,
            ..Self::default()
        }
    }
}
/// Parked operations one probe pass asks about at most, one at a time.
const PROBE_PAGE: u16 = 16;

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
    /// recovery was still paging. They back off the same way, and park the
    /// operation once they reach [`PARK_AFTER`] too, so an operation always
    /// parks in the end and can be reversed: a recovery walk whose owner is
    /// silent would otherwise hold it moving for good (the fourth lifecycle
    /// review's F3).
    deferrals: u32,
    /// How much unsettled work the delegate reported last round, so a round
    /// in which some of it settled counts as progress.
    remaining: Option<usize>,
}
impl Drive {
    /// A round with no progress. `true` when waiting has stopped helping and
    /// the operation should park.
    fn wait(&mut self, now: Instant, unit: Duration) -> bool {
        self.retries += 1;
        if self.retries >= PARK_AFTER {
            return true;
        }
        self.due = Some(now + backoff(unit, self.retries));
        false
    }
    /// A round put off for a reason outside the operation: wait longer each
    /// time. `true` once that has gone on for [`PARK_AFTER`] rounds: the
    /// operation parks as its owner not answering.
    fn defer(&mut self, now: Instant, unit: Duration) -> bool {
        self.deferrals = self.deferrals.saturating_add(1);
        self.due = Some(now + backoff(unit, self.deferrals));
        self.deferrals >= PARK_AFTER
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
    /// Ask the schedule coordinator to pause each of these Routines, with the
    /// definition revision last read.
    PauseRoutines(Vec<BotRoutineCreationIdentity>),
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
    /// Whether the owner of an operation parked for its silence answers
    /// again: the read every settling round makes (`UnsettledWork`), and
    /// nothing else. An answer resumes the operation as a person's Continue
    /// would, and its own rounds then stop what is unsettled.
    Probe { bot: BotId },
}
pub(in crate::worker) struct LifecycleResult {
    key: OperationKey,
    outcome: Outcome,
}
enum Outcome {
    Policy(Result<BotPolicyBarrierState, koru_delegate_api::Failure>),
    /// The coordinator's answer for each Routine asked.
    Routines(Vec<(koru_delegate_core::ScheduleId, RoutinePause)>),
    Settle(Result<Unsettled, koru_delegate_api::Failure>),
    ChatRead(Result<ChatWork, koru_delegate_api::Failure>),
    /// The Stops went out; their answers are read again next round.
    Stopped,
    /// The owner of a parked operation answered (or not).
    Probe(Probed),
}
/// The schedule coordinator's answer to one Routine's Pause. Only this
/// settles a Routine member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RoutinePause {
    /// It paused the Routine, or found it already paused.
    Paused,
    /// No live Schedule: it was deleted or retired.
    Gone,
    /// Another computer owns it, or a handoff is pending: asking again here
    /// will not pause it.
    Elsewhere,
    /// No answer, or none this driver can act on.
    NoAnswer,
}
/// The most times one Routine's Pause is sent again after its revision moved.
const ROUTINE_PAUSE_ATTEMPTS: usize = 3;
/// The longest one Routine's Pause waits on the coordinator.
const ROUTINE_PAUSE_WITHIN: Duration = Duration::from_secs(15);

/// Pause one Routine through its coordinator. The Schedule is read only to
/// learn the revision to name; what it reads never settles the member.
fn pause_routine(
    authority: &dyn BotAuthority,
    delegate: &dyn BotDelegatePort,
    owner: BotOwner,
    profile: koru_ai_core::ProfileId,
    identity: &BotRoutineCreationIdentity,
) -> RoutinePause {
    let cancellation = Cancellation::default();
    let route = identity.route();
    // Unknown until read. A wrong guess is answered as a revision conflict,
    // or as not found, by the coordinator: never acted on here.
    let mut revision = 1;
    for _ in 0..ROUTINE_PAUSE_ATTEMPTS {
        if let Ok(RoutineOverviewObservation::Current {
            definition_revision,
            ..
        }) = authority.read_routine_overview(owner, profile, identity, &cancellation)
        {
            revision = definition_revision;
        }
        let answer = delegate.execute_within(
            DelegateCommand::ControlRoutine {
                route: route.clone(),
                expected_revision: revision,
                action: koru_delegate_api::bots::RoutineControlAction::Pause {},
            },
            ROUTINE_PAUSE_WITHIN,
        );
        match answer {
            Ok(BotReply::RoutineControlled {
                route: echoed,
                lifecycle: koru_delegate_core::RoutineControlLifecycle::Paused,
                ..
            }) if echoed == route => return RoutinePause::Paused,
            Ok(_) => return RoutinePause::NoAnswer,
            Err(error) => match error.code.as_str() {
                // The coordinator found it already paused.
                "schedule_state_conflict" => return RoutinePause::Paused,
                // Its revision moved: read it again and send again.
                "schedule_revision_conflict" => {}
                "schedule_not_found" | "schedule_retired" => return RoutinePause::Gone,
                "schedule_not_owner" | "schedule_handoff_pending" => {
                    return RoutinePause::Elsewhere;
                }
                _ => return RoutinePause::NoAnswer,
            },
        }
    }
    RoutinePause::NoAnswer
}

/// What one probe of a parked operation's owner found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probed {
    /// The execution owner answered: the operation takes up its work again.
    Answered,
    /// The execution owner did not answer: the recovery pass asks again.
    OwnerAway,
    /// Nothing was asked: the owner no longer has authority over the Bot's
    /// workspace. Asking again sooner would not change that, so this keeps
    /// nothing armed; any later pass still reaches the operation in turn.
    NotAsked,
}
/// The delegate's answer for one conversation's settling round, for the
/// whole Bot: the writer keeps what belongs to the conversation.
struct ChatWork {
    admissions: Vec<BotAdmissionView>,
    /// More queued work exists past the page.
    more: bool,
    /// Each Run with a native effect the delegate still holds for the Bot,
    /// once.
    effects: HashSet<RunId>,
    /// The pages could not be read to their end (see [`EFFECT_PAGE_STOP`]).
    effects_cut: bool,
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
            Step::PauseRoutines(routines) => Outcome::Routines(
                routines
                    .iter()
                    .map(|identity| (identity.schedule, RoutinePause::NoAnswer))
                    .collect(),
            ),
            Step::Settle { .. } => Outcome::Settle(Err(unavailable())),
            Step::ChatRead { .. } => Outcome::ChatRead(Err(unavailable())),
            Step::Stop { .. } => Outcome::Stopped,
            Step::Probe { .. } => Outcome::Probe(Probed::OwnerAway),
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
            Step::PauseRoutines(routines) => {
                // The person who holds the home now is the reader of each
                // Schedule; without the owner's authority nothing is asked.
                let profile = checked
                    .as_ref()
                    .and_then(|checked| checked.workspace(self.workspace).ok());
                // One silent coordinator is not asked again this round for
                // the Routines after it: each call may wait its whole bound.
                let mut silent = false;
                let mut answers = Vec::with_capacity(routines.len());
                for identity in &routines {
                    let answer = match profile {
                        Some(profile)
                            if !silent
                                && identity.owner.user_scope == self.owner.user_scope
                                && identity.owner.device == self.owner.device
                                && identity.home == self.workspace =>
                        {
                            pause_routine(
                                self.authority.as_ref(),
                                self.delegate.as_ref(),
                                self.owner,
                                profile,
                                identity,
                            )
                        }
                        _ => RoutinePause::NoAnswer,
                    };
                    silent |= answer == RoutinePause::NoAnswer;
                    answers.push((identity.schedule, answer));
                }
                Outcome::Routines(answers)
            }
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
                    // Every held native effect of the Bot, page after page
                    // to the end, so the conversation's own are found however
                    // many its siblings hold; the writer keeps only its own.
                    let mut effects = HashSet::new();
                    let mut after = None;
                    let mut effects_cut = true;
                    for _ in 0..EFFECT_PAGE_STOP {
                        let BotReply::NativePendingEffects { page: found } = self
                            .delegate
                            .execute(DelegateCommand::PendingNativeEffects {
                                owner,
                                bot: Some(bot),
                                after: after.clone(),
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
                            None => {
                                effects_cut = false;
                                break;
                            }
                            // A cursor that does not advance would loop.
                            Some(next) if after.as_ref() == Some(&next) => break,
                            Some(next) => after = Some(next),
                        }
                    }
                    Ok(ChatWork {
                        admissions: work.admissions.into_iter().map(|a| a.admission).collect(),
                        more: work.more,
                        effects,
                        effects_cut,
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
            Step::Probe { bot } => {
                let owner = delegate_owner(self.owner);
                // Without the owner's authority over its workspace nothing is
                // asked: that is not the execution owner being away.
                if checked.is_none() {
                    return Outcome::Probe(Probed::NotAsked);
                }
                Outcome::Probe(
                    match self.delegate.execute(DelegateCommand::UnsettledWork {
                        owner,
                        bot,
                        limit: 1,
                    }) {
                        Ok(BotReply::UnsettledWork { .. }) => Probed::Answered,
                        _ => Probed::OwnerAway,
                    },
                )
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

/// The first wait of a round that made no progress: 250 ms, doubling to
/// 32 s at the seventh.
const ROUND_UNIT: Duration = Duration::from_millis(250);

fn backoff(unit: Duration, round: u32) -> Duration {
    unit * (1u32 << round.min(7))
}

/// The Bot commands that only read, and so are answered for an archived Bot
/// as for an active one (EXPERIENCE §3: Archive "retains its history in an
/// Archived filter"). Every other command naming an archived Bot answers
/// `bot_archived`. A new read-only Bot command is added here.
pub(super) fn bot_read_command(request: &BotCommand) -> bool {
    matches!(
        request,
        BotCommand::Read { .. }
            | BotCommand::ReadRoutines { .. }
            | BotCommand::ReadRoutineDetail { .. }
            | BotCommand::ReadRoutineCreation { .. }
            | BotCommand::ReadLibrary { .. }
            | BotCommand::ReadMemory { .. }
            | BotCommand::ListMemory { .. }
            | BotCommand::ReadMemoryFact { .. }
            | BotCommand::ReadTaskAnswer { .. }
            | BotCommand::ListTaskBots { .. }
            | BotCommand::ReadOperation { .. }
            | BotCommand::ReadLifecycleOperation { .. }
            | BotCommand::List { .. }
            | BotCommand::ListLifecycle { .. }
            // P1.8 reads: an archived Bot's conversations stay listable;
            // activity is read for active Bots only by its own query.
            | BotCommand::ListBotChats { .. }
            | BotCommand::ListActivity { .. }
            // An owner's Trash, whatever each Bot's lifecycle.
            | BotCommand::ListTrashedBotChats { .. }
            // P1.8 creation reads: a completed creation stays readable
            // whatever the Bot's lifecycle is now.
            | BotCommand::ReadBotCreation { .. }
            | BotCommand::ListBotCreations { .. }
            // P1.8 B1: a Run of a conversation, read through its owner;
            // an archived Bot's history reads.
            | BotCommand::ReadRun { .. }
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
            | BotCommand::CreateRoutine { bot_id, .. }
            | BotCommand::RetryRoutineCreation { bot_id, .. }
            | BotCommand::SetKnowledge { bot_id, .. }
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
        self.with_open_operations(std::slice::from_mut(&mut bot))?;
        self.with_lifecycle_holders(prepared_owner(context)?, std::slice::from_mut(&mut bot))?;
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
            | BotCommand::CreateRoutine { bot_id, .. }
            | BotCommand::RetryRoutineCreation { bot_id, .. }
            | BotCommand::SetKnowledge { bot_id, .. }
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
        self.with_lifecycle_holders(owner, &mut bots)?;
        Ok(BotResponse::List { bots })
    }

    /// Fill in each read Bot's open lifecycle operation and the change of it
    /// still being delivered to its execution owner (`pending_change`), each
    /// one indexed read per Bot.
    pub(super) fn with_open_operations(&self, bots: &mut [Bot]) -> ApiResult<()> {
        for bot in bots {
            bot.open_operation = self
                .repository
                .bot_open_lifecycle(bot.id)
                .map_err(bot_failure)?;
            bot.pending_change = self.bot_pending_change(bot.id)?;
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
    /// One that parked and was then reversed answers the same repeat with its
    /// progress (`reversed`) and is not taken up. Anything else under that id
    /// is `request_conflict`.
    fn repeat_bot_lifecycle(
        &mut self,
        op: LifecycleOperation,
        (owner, bot): (BotOwner, BotId),
        kind: BotLifecycleKind,
        chat: Option<ChatId>,
        fingerprint: &str,
    ) -> ApiResult<LifecycleOperation> {
        // A refusal is kept only by a parked operation, and by one reversed
        // while parked.
        let by_id = op.kind == kind && op.refusal.is_some();
        if op.bot != bot
            || op.owner != owner
            || op.chat != chat
            || (op.fingerprint != fingerprint && !by_id)
        {
            return Err(bot_failure(BotError::Conflict));
        }
        if op.stage == BotLifecycleStage::Incomplete {
            self.resume_parked_bot_lifecycle((op.owner.user_scope, op.operation))
                .map_err(bot_failure)?;
        }
        self.repository
            .bot_lifecycle_operation(op.owner.user_scope, op.operation)
            .map_err(bot_failure)?
            .ok_or_else(not_found)
    }

    /// Take a parked operation up again from `captured`, which re-asks every
    /// owner idempotently, and drive it: a person's Continue, and an owner
    /// answering again after the operation parked for its silence, are this
    /// one path. A reversed operation is not taken up (`false`).
    fn resume_parked_bot_lifecycle(&mut self, key: OperationKey) -> Result<bool, BotError> {
        let resumed =
            self.repository
                .resume_bot_lifecycle(key.0, key.1, Timestamp::now().millis())?;
        if resumed {
            self.bot_lifecycle.driving.entry(key).or_default();
        }
        Ok(resumed)
    }

    /// The owner answered a call about this Bot. An operation of it parked
    /// because the owner did not answer takes up its work again, exactly as
    /// a person's Continue would, so its rounds stop whatever the owner ran
    /// meanwhile (a restarted owner starts the Runs it had queued before the
    /// AI side reaches it). One parked for another reason, or reversed,
    /// stays as it is; a person's Restore of it still wins.
    pub(in crate::worker) fn owner_answered_bot_lifecycle(&mut self, bot: BotId) {
        let Ok(Some(open)) = self.repository.bot_open_lifecycle(bot) else {
            return;
        };
        if open.stage != BotLifecycleStage::Incomplete
            || open.refusal != Some(BotLifecycleRefusal::OwnerUnavailable)
            || !matches!(
                open.kind,
                BotLifecycleKind::Archive | BotLifecycleKind::Trash | BotLifecycleKind::TrashChat
            )
        {
            return;
        }
        let Ok(Some(owner)) = self.repository.bot_owner(bot) else {
            return;
        };
        if let Ok(true) = self.resume_parked_bot_lifecycle((owner.user_scope, open.request_id)) {
            tracing::info!(target: "koru_ai::bots", operation = %open.request_id, "Bot lifecycle operation taken up again: its owner answered");
        }
    }

    /// Ask the owner of each operation parked for its silence whether it
    /// answers again: a pass asks up to [`PROBE_PAGE`] of them, **one probe
    /// outstanding at a time**, each answer sending the next. Run by the
    /// backed-off receipt recovery pass and once after a restart's resume
    /// pages, never on a timer of its own: an operation with nothing else owed
    /// to its owner (its pause already acknowledged) would otherwise never
    /// learn that the owner is back. A pass already under way carries on by
    /// itself, so asking again while it runs starts nothing.
    ///
    /// One at a time because a probe runs on the shared administration lane,
    /// whose queue holds [`PROVIDER_ADMIN_QUEUE_CAPACITY`] and which the
    /// other background passes share: a pass that put its whole page on it
    /// at once filled it while the owner was silent (each probe holding the
    /// thread until its owner call gave up), and the next background job was
    /// refused `ai_worker_saturated` (before preparation had a lane of its
    /// own, a person's Bot read or command too). The lane runs one job at a
    /// time anyway, so asking one at a time costs the walk nothing but a turn
    /// of the writer between probes.
    ///
    /// The walk keeps its place across passes ([`LifecycleDriver::probe_after`])
    /// and starts again from the first at the end, so every parked operation
    /// is asked in turn however many before it keep failing (the fourth
    /// lifecycle review's F2). A probe its owner did not answer arms the
    /// recovery pass again, and so does a pass that stops with more parked
    /// operations past it; one refused before asking (the owner's authority
    /// over the workspace lost) or a Bot that is gone arms nothing.
    pub(in crate::worker) fn probe_parked_bot_lifecycle(&mut self) {
        if self.bot_delegate.is_none() || self.provider_admin.is_none() {
            return;
        }
        if self.bot_lifecycle.probing.is_some() {
            return;
        }
        self.probe_next_parked_bot_lifecycle(PROBE_PAGE);
    }

    /// The probe walk's next step, with `left` probes this pass may still
    /// send: ask the next parked operation's owner, or end the pass.
    fn probe_next_parked_bot_lifecycle(&mut self, mut left: u16) {
        loop {
            let after = self.bot_lifecycle.probe_after.clone();
            let Ok(next) = self
                .repository
                .parked_for_owner_bot_lifecycle(after.as_ref(), 1)
            else {
                return self.bot_recovery_retry.owner_away(Instant::now());
            };
            let Some(parked) = next.into_iter().next() else {
                // The end of the list: the next pass starts from the first.
                self.bot_lifecycle.probe_after = None;
                return;
            };
            if left == 0 {
                // More are parked past where this pass stopped: there must
                // be a next pass, and it carries on from here.
                return self.bot_recovery_retry.owner_away(Instant::now());
            }
            left -= 1;
            let (Some(authority), Some(delegate), Some(pool)) = (
                self.bot_authority.clone(),
                self.bot_delegate.clone(),
                self.provider_admin.as_ref(),
            ) else {
                return;
            };
            // A Bot that is gone asks nothing and keeps nothing armed.
            let Ok(Some((subject, _))) = self.repository.bot_any_lifecycle(parked.bot) else {
                self.bot_lifecycle.probe_after = Some(parked.cursor);
                continue;
            };
            let key = (parked.scope, parked.operation);
            let job = LifecycleJob {
                key,
                owner: BotOwner {
                    user_scope: subject.user_scope,
                    device: subject.owner_device,
                },
                workspace: subject.home_workspace,
                authority,
                delegate,
                step: Step::Probe { bot: parked.bot },
            };
            if pool
                .try_submit(ProviderAdminJob::BotLifecycle(job))
                .is_err()
            {
                // The lane is full: the next pass asks this one first.
                return self.bot_recovery_retry.owner_away(Instant::now());
            }
            self.bot_lifecycle.probe_after = Some(parked.cursor);
            self.bot_lifecycle.probing = Some((key, left));
            return;
        }
    }

    /// A probe's answer: an owner that answered takes its parked operation
    /// up again; one that did not arms the recovery pass, which asks again.
    /// Then the pass sends its next probe.
    fn complete_bot_lifecycle_probe(&mut self, key: OperationKey, answer: Probed) {
        let left = match self.bot_lifecycle.probing {
            Some((probing, left)) if probing == key => {
                self.bot_lifecycle.probing = None;
                Some(left)
            }
            _ => None,
        };
        match answer {
            Probed::Answered => {
                self.bot_recovery_retry.answered(Instant::now());
                if let Ok(Some(op)) = self.repository.bot_lifecycle_operation(key.0, key.1) {
                    self.owner_answered_bot_lifecycle(op.bot);
                }
            }
            Probed::OwnerAway => self.bot_recovery_retry.owner_away(Instant::now()),
            Probed::NotAsked => {}
        }
        if let Some(left) = left {
            self.probe_next_parked_bot_lifecycle(left);
        }
    }

    /// One lifecycle operation is open per Bot (M1). A new one while another
    /// is moving or parked answers that one's hold (`bot_lifecycle_pending`),
    /// never `request_conflict`, which means a reused request id; the Bot's
    /// read names the open operation, to resume it or reverse it. Only the
    /// Restore of that very operation (`reverses`) may start beside it.
    fn refuse_competing(&self, bot: BotId, reverses: Option<BotRequestId>) -> ApiResult<()> {
        match self
            .repository
            .bot_open_lifecycle(bot)
            .map_err(bot_failure)?
        {
            Some(open) if Some(open.request_id) != reverses => Err(hold_failure(
                if open.stage == BotLifecycleStage::Incomplete {
                    BotHold::Parked
                } else {
                    BotHold::Pending
                },
            )),
            _ => Ok(()),
        }
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
                (_, BotLifecycle::Trashed) => return Err(hold_failure(BotHold::BotTrashed)),
                (BotLifecycleKind::Archive, BotLifecycle::Archived) => {
                    return Err(archived_failure());
                }
                _ => {}
            }
        }
        if self.bot_delegate.is_none() || self.provider_admin.is_none() {
            return Err(bot_failure(BotError::Unavailable));
        }
        if existing.is_none() {
            self.refuse_competing(*bot_id, None)?;
        }
        let key = (owner.user_scope, *request_id);
        let now = Timestamp::now().millis();
        let fresh = existing.is_none();
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
                // One member per Routine of the Bot, in the same transaction
                // (P4.3). For a Routine `hide` means paused: its Schedule
                // document is not moved to the Store's Trash (O41). One whose
                // creation never finished has no Schedule to pause and is
                // kept as it is.
                .chain(
                    self.repository
                        .lifecycle_bot_routines(*bot_id)
                        .map_err(bot_failure)?
                        .into_iter()
                        .map(|(schedule, workspace_id, bound)| LifecycleMemberDraft {
                            subject: BotLifecycleSubject::Routine {
                                workspace_id,
                                document_id: koru_ai_core::DocumentId::from_uuid(
                                    schedule.as_uuid(),
                                ),
                            },
                            reason: BotLifecycleReason::OwnedRoutine,
                            prior_state: BotLifecyclePriorState::Live,
                            intended: if bound {
                                BotLifecycleIntended::Hide
                            } else {
                                BotLifecycleIntended::Retain
                            },
                            retained: (!bound).then_some(BotLifecycleRetained::OwnershipUnproven),
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
        // What capture refused is stopped now, whether or not the owner
        // ever answers: each attempt not yet ended as the person's Stop.
        if fresh {
            let _ = self.refuse_unadmitted(&operation, &bot, None);
        }
        if operation.stage.moving() {
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
                return Err(hold_failure(BotHold::BotTrashed));
            }
            // A source already reversed is refused by the capture, as any
            // second reversal is (`request_conflict`).
            if source.stage.moving() {
                return Err(source_moving());
            }
            self.refuse_competing(*bot_id, Some(*source_request_id))?;
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
        // A parked operation's pause may never have reached the owner, or
        // its answer was lost. The Bot comes back paused, so that pause is
        // still owed: offered now, and by the backed-off recovery pass until
        // the owner acknowledges it. Until then a pause, resume or change of
        // the Bot answers `bot_operation_pending`; after it, each is accepted.
        if let Some(pause) = source.pause_request
            && let Ok(Some(active)) = self.repository.bot(*bot_id)
            && let Ok(Some(receipt)) = self.repository.bot_admission_receipt(owner, *bot_id, pause)
            && receipt.operation().status == BotOperationStatus::Pending
        {
            self.queue_bot_receipt(&active, receipt, None);
        }
        // An access edit left `prepared` while the Bot was in Trash is owed
        // its barrier install again.
        self.routine_controls.access_owed();
        let mut progress = operation.progress();
        (progress.routines_needing_access, progress.routines_still_on) =
            self.restored_routines(owner, *bot_id, &source);
        Ok(BotResponse::Lifecycle {
            operation: progress,
        })
    }

    /// What a Restore tells the person about the Bot's Routines, read from
    /// the journal and the permission records on the writer, with no owner
    /// call and nothing stored. The Restore re-enables nothing and pauses
    /// nothing: every Routine keeps the state it has.
    ///
    /// `still on`: each captured Routine the source operation did not confirm
    /// paused. A completed Archive or Trash has none (it completes only once
    /// every one is acknowledged or kept); a parked one may. It means "not
    /// confirmed paused".
    ///
    /// `needing access`: each captured Routine whose permission records no
    /// longer read: its creation row bound, its original grant's digest
    /// valid, and its edited grant, if any, valid. Such a Routine stays
    /// paused and Enable refuses it until its access is chosen again.
    fn restored_routines(
        &self,
        owner: BotOwner,
        bot: BotId,
        source: &LifecycleOperation,
    ) -> (
        Vec<koru_delegate_core::ScheduleId>,
        Vec<koru_delegate_core::ScheduleId>,
    ) {
        let mut needing_access = Vec::new();
        let mut still_on = Vec::new();
        for member in &source.members {
            let BotLifecycleSubject::Routine { document_id, .. } = &member.subject else {
                continue;
            };
            let schedule = koru_delegate_core::ScheduleId::from_uuid(document_id.as_uuid());
            // A Routine captured without a finished creation was never
            // asked to pause and has no access to choose.
            if member.intended != BotLifecycleIntended::Hide {
                continue;
            }
            if !matches!(
                member.stage,
                BotLifecycleMemberStage::Acknowledged | BotLifecycleMemberStage::Retained
            ) {
                still_on.push(schedule);
            }
            let readable = matches!(
                self.repository.bot_routine_creation(owner, bot, schedule),
                Ok(Some(creation))
                    if creation.state == BotRoutineCreationState::Bound
                        && creation.identity.validate_digests().is_ok()
            ) && self
                .repository
                .routine_current_grant(owner, bot, schedule)
                .is_ok();
            if !readable {
                needing_access.push(schedule);
            }
        }
        (needing_access, still_on)
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
                BotLifecycle::Trashed => return Err(hold_failure(BotHold::BotTrashed)),
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
        if existing.is_none() {
            self.refuse_competing(*bot_id, None)?;
        }
        let fresh = existing.is_none();
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
        if fresh {
            let _ = self.refuse_unadmitted(&operation, &bot, Some(*chat_id));
        }
        if operation.stage.moving() {
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
                BotLifecycle::Trashed => return Err(hold_failure(BotHold::BotTrashed)),
                BotLifecycle::Archived => return Err(archived_failure()),
                BotLifecycle::Active => {}
            }
            // A moving conversation Trash is reversed once it completes or parks.
            if source.stage.moving() {
                return Err(source_moving());
            }
            self.refuse_competing(*bot_id, Some(*source_request_id))?;
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
                // Every moving operation is driven again; those parked for
                // an owner that did not answer ask it once whether it does now.
                Ok(rows) if rows.is_empty() => {
                    self.bot_lifecycle.recovery = None;
                    self.probe_parked_bot_lifecycle();
                }
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
        let unit = self.bot_lifecycle.unit;
        let Some(drive) = self.bot_lifecycle.driving.get_mut(&key) else {
            return;
        };
        if drive.wait(Instant::now(), unit) {
            self.park_bot_lifecycle(key, refusal);
        }
    }

    /// Put off for a reason that is not the operation's (a full pool, a
    /// restart's recovery walk still paging). Past [`PARK_AFTER`] such rounds
    /// it parks as its owner not answering, which is what keeps that walk
    /// from paging: the person can then reverse it, and an owner that
    /// answers again takes it up.
    fn defer_bot_lifecycle(&mut self, key: OperationKey) {
        let unit = self.bot_lifecycle.unit;
        let Some(drive) = self.bot_lifecycle.driving.get_mut(&key) else {
            return;
        };
        if drive.defer(Instant::now(), unit) {
            self.park_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable);
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
                    // Its owner is asked again on the recovery pass, and the
                    // operation is taken up once it answers.
                    if refusal == BotLifecycleRefusal::OwnerUnavailable {
                        self.bot_recovery_retry.owner_away(Instant::now());
                    }
                }
            }
            Err(error) => {
                tracing::warn!(target: "koru_ai::bots", ?error, operation = %key.1, "a Bot lifecycle operation could not be parked; it is retried");
                let unit = self.bot_lifecycle.unit;
                if let Some(drive) = self.bot_lifecycle.driving.get_mut(&key) {
                    // Only its wait matters here: parking is what failed.
                    let _ = drive.defer(Instant::now(), unit);
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
            // longer, and parking only after as many rounds as a wait.
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
        ) || !op.stage.moving()
        {
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
        // Every Bot task attempt of the subject not yet ended, whatever its
        // receipt's stage (one admitted while it was still in its context
        // window has no live task for the delegate's report to find), is
        // stopped as the person's Stop. It is read from the journal of
        // attempts each time, so a restart re-checks it.
        match self.stop_unended_attempts(op.bot, chat) {
            Some(false) => return Some(false),
            Some(true) => unresolved = true,
            None => {}
        }
        unresolved.then_some(true)
    }

    /// Stop, as the person's Stop, each Bot task attempt of a Bot (or of one
    /// of its conversations) that has not ended. `None` when there is none,
    /// `Some(true)` when some were stopped, `Some(false)` when they could not
    /// be read.
    fn stop_unended_attempts(&mut self, bot: BotId, chat: Option<ChatId>) -> Option<bool> {
        let attempts = match self.repository.bot_unended_attempts(bot, chat) {
            Ok(attempts) => attempts,
            Err(_) => return Some(false),
        };
        if attempts.is_empty() {
            return None;
        }
        for attempt in attempts {
            let _ = self.stop_attempt(attempt);
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

    /// `settling`: refuse the work not admitted at capture, then ask the delegate
    /// what of the Bot is unsettled; the answer is applied in
    /// [`Self::complete_bot_lifecycle`].
    fn step_archive_settle(&mut self, key: OperationKey, op: &LifecycleOperation, bot: &Bot) {
        // A restart's conversation recovery is still loading interrupted
        // tasks. Not this operation's doing, so it defers, which parks only
        // after as many rounds as a wait does.
        if self.bot_chat_recovery.is_some() {
            return self.defer_bot_lifecycle(key);
        }
        // The Bot's Routines first, on every path: each one not yet settled
        // by the coordinator's answer is asked to pause, this round.
        match self.unsettled_lifecycle_routines(op) {
            Ok(routines) if routines.is_empty() => {}
            Ok(routines) => {
                return self.submit_bot_lifecycle(key, op, bot, Step::PauseRoutines(routines));
            }
            Err(_) => return self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable),
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

    /// The captured Routines of an Archive or Trash that the coordinator has
    /// not yet answered for: each `pending` one, and each `unavailable` one
    /// (asked again after a park and a resume). Their retained creation
    /// identities name the route to pause.
    fn unsettled_lifecycle_routines(
        &self,
        op: &LifecycleOperation,
    ) -> Result<Vec<BotRoutineCreationIdentity>, BotError> {
        let mut routines = Vec::new();
        for member in &op.members {
            let BotLifecycleSubject::Routine { document_id, .. } = &member.subject else {
                continue;
            };
            if !matches!(
                member.stage,
                BotLifecycleMemberStage::Pending | BotLifecycleMemberStage::Unavailable
            ) {
                continue;
            }
            let schedule = koru_delegate_core::ScheduleId::from_uuid(document_id.as_uuid());
            let creation = self
                .repository
                .bot_routine_creation(op.owner, op.bot, schedule)?
                .ok_or(BotError::Storage)?;
            routines.push(creation.identity);
        }
        Ok(routines)
    }

    /// The coordinator's answers, recorded. A member is acknowledged here and
    /// nowhere else. One the coordinator could not pause from this computer
    /// parks the operation at once; one with no answer waits and is asked
    /// again, and parks the operation when waiting stops helping.
    fn settle_lifecycle_routines(
        &mut self,
        key: OperationKey,
        answers: Vec<(koru_delegate_core::ScheduleId, RoutinePause)>,
    ) {
        let now = Timestamp::now().millis();
        let (mut elsewhere, mut unanswered) = (false, false);
        for (schedule, answer) in answers {
            let recorded = match answer {
                RoutinePause::Paused => LifecycleRoutineAnswer::Paused,
                RoutinePause::Gone => LifecycleRoutineAnswer::Gone,
                RoutinePause::Elsewhere => {
                    elsewhere = true;
                    LifecycleRoutineAnswer::Unavailable
                }
                RoutinePause::NoAnswer => {
                    unanswered = true;
                    LifecycleRoutineAnswer::Unavailable
                }
            };
            if self
                .repository
                .settle_bot_lifecycle_routine(
                    key.0,
                    key.1,
                    koru_ai_core::DocumentId::from_uuid(schedule.as_uuid()),
                    recorded,
                    now,
                )
                .is_err()
            {
                unanswered = true;
            }
        }
        if elsewhere {
            self.park_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable)
        } else if unanswered {
            self.wait_bot_lifecycle(key, BotLifecycleRefusal::OwnerUnavailable)
        } else {
            self.progressed_bot_lifecycle(key)
        }
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
        // Nor may any Bot task attempt of it still be unended.
        if self.stop_unended_attempts(op.bot, None).is_some() {
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
        self.request_bot_boot_refresh();
        let key = result.key;
        if let Outcome::Probe(answer) = result.outcome {
            return self.complete_bot_lifecycle_probe(key, answer);
        }
        let Some(drive) = self.bot_lifecycle.driving.get_mut(&key) else {
            return;
        };
        drive.busy = false;
        // A parked, completed or reversed operation ignores a late answer.
        let op = match self.repository.bot_lifecycle_operation(key.0, key.1) {
            Ok(Some(op)) if op.stage.moving() => op,
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
            Outcome::Routines(answers) => self.settle_lifecycle_routines(key, answers),
            Outcome::Settle(Ok(unsettled)) => self.settle_bot_archive(key, &op, unsettled),
            Outcome::ChatRead(Ok(work)) => self.settle_bot_chat_trash(key, &op, work),
            // The round was already counted when its read was applied; the
            // next read says what the Stops ended.
            Outcome::Stopped => {}
            Outcome::Probe(_) => {}
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
            + usize::from(work.effects_cut);
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
            | BotCommand::SetKnowledge { bot_id, .. }
            | BotCommand::Update { bot_id, .. }
            | BotCommand::UpdateAccess { bot_id, .. }
            | BotCommand::Pause { bot_id, .. }
            | BotCommand::Resume { bot_id, .. }
            | BotCommand::CreateRoutine { bot_id, .. }
            | BotCommand::RetryRoutineCreation { bot_id, .. } => (*bot_id, None, false),
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
        let Some(hold) = hold.filter(|hold| !trash_only || hold.hides()) else {
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
            || matches!(
                request,
                // A task's Stop only reduces work: a trashed Bot's is
                // answered too.
                BotCommand::ReadLifecycleOperation { .. } | BotCommand::StopTask { .. }
            ))
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
        BotHold::BotTrashed | BotHold::Trashed => {
            !matches!(command, WorkerCommand::CancelAttempt { .. })
        }
        BotHold::Pending | BotHold::Parked => matches!(
            command,
            WorkerCommand::SendTurn { .. }
                | WorkerCommand::RetryTurn { .. }
                | WorkerCommand::SteerAttempt { .. }
                | WorkerCommand::BranchChat { .. }
                | WorkerCommand::UpdateChatModel { .. }
                | WorkerCommand::UpdateChatAgentSettings { .. }
                // Held as the conversation's other settings are.
                | WorkerCommand::RenameChat { .. }
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
        BotHold::BotTrashed => failure(
            bot_lifecycle_codes::BOT_TRASHED,
            "This Bot is in Trash. Restore the Bot to use it or its conversations again.",
            false,
        ),
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

/// A Restore (or a conversation's Restore) naming an operation that is
/// still moving. A moving operation always ends, completed or parked (the
/// rounds park it at the latest), and the Restore is accepted then, so this
/// `bot_lifecycle_pending` is retryable, whatever moved it: its capture, a
/// person's Continue, or the engine taking a parked operation up again
/// because its owner answered (the fourth lifecycle review's F5). The journal
/// does not record why an operation moves, and the answer is true either
/// way. Every other `bot_lifecycle_pending` keeps its flag: a new operation
/// beside an open one, or a change of a held subject, is not accepted by
/// waiting alone.
fn source_moving() -> Failure {
    let mut moving = hold_failure(BotHold::Pending);
    moving.retryable = true;
    moving
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
