// SPDX-License-Identifier: MPL-2.0
//! The owner's controls of one Bot Routine (P4.3): enable, pause, edit of
//! timing and name, Test, and the access edit.
//!
//! Who owns what. The delegate's schedule coordinator owns the Schedule
//! document write and the claim; this module never writes a Schedule and
//! never decides that a control was applied. The AI worker owns the Routine's
//! access (its grant and the receipt of an edit) and the readiness checks.
//! Each control is one finite job off the writer (authority, one Store read,
//! one delegate call), with every SQL read and write on the writer before and
//! after it. The job waits on the Schedule's owner, so it runs on the
//! interactive owner-call lane, beside a person's approvals and steers: a
//! slow owner holds those, and never the preparation every Bot command needs.
//! The caller's owner is the vouched one from preparation, never a request
//! field.
//!
//! Nothing durable is pending for enable, pause, edit or Test: a restart
//! forgets them and the truth is read again. An access edit is durable: its
//! receipt is `prepared` with the grant row, the barrier install follows, and
//! a restart repeats the install until the receipt is `completed`.
use super::*;
use koru_ai_sqlite::BotHold;
use koru_ai_wire::bot_routine_codes as codes;
use koru_delegate_api::bots::RoutineControlAction;
use koru_delegate_core::{
    ActorRef, RoutineDigest32, RoutinePolicyBarrier, RoutineSlot, RunId, ScheduleId,
};

#[cfg(test)]
pub(in crate::worker::bots) mod tests;

/// The longest one control waits on the Schedule's owner. The owner's own
/// reply bound is 30 s; past this the answer is `routine_control_unknown`
/// and a retry with the same revision is safe. Shorter under test.
#[cfg(not(test))]
const OWNER_WITHIN: Duration = Duration::from_secs(15);
#[cfg(test)]
const OWNER_WITHIN: Duration = Duration::from_secs(3);
/// The one absolute deadline a control carries through all its phases, inside
/// the caller's own wait so the worker's answer always reaches it.
#[cfg(not(test))]
const CONTROL_DEADLINE: Duration = Duration::from_secs(22);
#[cfg(test)]
const CONTROL_DEADLINE: Duration = Duration::from_secs(8);
const _: () = assert!(CONTROL_DEADLINE.as_millis() < RESPONSE_TIMEOUT.as_millis());
/// The most unsettled admissions one access edit asks the owner for. A Bot
/// holds at most 50 queued; a longer answer refuses the edit.
const UNSETTLED_PAGE: u16 = 64;

/// This computer's current time zone name, read on every call and never
/// cached. `None` when it cannot be told.
pub(in crate::worker) type ZoneSource = Arc<dyn Fn() -> Option<String> + Send + Sync>;
type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// One Routine's unresolved control. A courtesy to the person, never a safety
/// mechanism: safety is the coordinator's revision check.
struct Slot {
    /// The control in flight, or 0 when only a Test's minute is outstanding.
    serial: u64,
    /// The Test this worker expects a paused preparation for.
    test: Option<TestSlot>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct TestSlot {
    minute: i64,
    revision: u32,
}
impl TestSlot {
    fn ended(&self, now_ms: i64) -> bool {
        now_ms >= self.minute + koru_delegate_core::ROUTINE_TEST_MINUTE_MS
    }
}

#[derive(Default)]
struct AccessRecovery {
    /// A `prepared` access edit may be waiting for its barrier install.
    owed: bool,
    retry_at: Option<Instant>,
    failures: u32,
    active: Option<BotRequestId>,
}

pub(in crate::worker) struct RoutineControls {
    slots: HashMap<ScheduleId, Slot>,
    serial: u64,
    clock: Clock,
    zone: Option<ZoneSource>,
    recovery: AccessRecovery,
}
impl RoutineControls {
    pub(in crate::worker) fn new(zone: Option<ZoneSource>) -> Self {
        Self {
            slots: HashMap::new(),
            serial: 0,
            clock: Arc::new(|| Timestamp::now().millis()),
            zone,
            // A restart scans once for edits whose install is not confirmed.
            recovery: AccessRecovery {
                owed: true,
                ..Default::default()
            },
        }
    }
    #[cfg(test)]
    pub(in crate::worker) fn set_clock(&mut self, clock: Clock) {
        self.clock = clock;
    }
    fn now_ms(&self) -> i64 {
        (self.clock)()
    }
    pub(in crate::worker) fn zone(&self) -> Option<ZoneSource> {
        self.zone.clone()
    }
    /// Take the Routine's one slot. A Test that names the minute and revision
    /// of its own outstanding Test is that Test again (a lost reply), and is
    /// let through.
    fn acquire(&mut self, schedule: ScheduleId, test: Option<TestSlot>) -> ApiResult<u64> {
        let now = self.now_ms();
        if let Some(slot) = self.slots.get(&schedule) {
            let replay = slot.serial == 0 && test.is_some() && slot.test == test;
            let free = slot.serial == 0 && slot.test.is_none_or(|test| test.ended(now));
            if !replay && !free {
                return Err(refusal(codes::CONTROL_PENDING, true));
            }
        }
        let serial = self.serial.checked_add(1).ok_or_else(unavailable_failure)?;
        self.serial = serial;
        self.slots.insert(schedule, Slot { serial, test });
        Ok(serial)
    }
    /// The control ended. An accepted Test, or one whose answer is unknown,
    /// keeps its minute outstanding until that minute ends or its claim is
    /// observed.
    fn release(&mut self, schedule: ScheduleId, serial: u64, keep_test: bool) {
        let Some(slot) = self.slots.get_mut(&schedule) else {
            return;
        };
        if slot.serial != serial {
            return;
        }
        if keep_test && slot.test.is_some() {
            slot.serial = 0;
        } else {
            self.slots.remove(&schedule);
        }
    }
    fn retarget_test(&mut self, schedule: ScheduleId, serial: u64, minute: i64) {
        if let Some(slot) = self.slots.get_mut(&schedule)
            && slot.serial == serial
            && let Some(test) = slot.test.as_mut()
        {
            test.minute = minute;
        }
    }
    /// Whether this worker itself asked for a Test of exactly this Routine,
    /// minute and definition revision, and that minute has not ended. The
    /// only thing that admits a paused preparation. In memory only: a restart
    /// forgets it and the preparation is refused.
    pub(in crate::worker) fn expects_test(
        &self,
        schedule: ScheduleId,
        occurrence_ms: i64,
        revision: u32,
    ) -> bool {
        let now = self.now_ms();
        self.slots.get(&schedule).is_some_and(|slot| {
            slot.test.is_some_and(|test| {
                test.minute == occurrence_ms && test.revision == revision && !test.ended(now)
            })
        })
    }
    /// The claim of a Test's minute was observed: its slot is free again.
    pub(in crate::worker) fn test_claimed(&mut self, schedule: ScheduleId, occurrence_ms: i64) {
        if self.slots.get(&schedule).is_some_and(|slot| {
            slot.serial == 0 && slot.test.is_some_and(|test| test.minute == occurrence_ms)
        }) {
            self.slots.remove(&schedule);
        }
    }
    /// A `prepared` access edit may be owed its install: scan again soon.
    pub(in crate::worker) fn access_owed(&mut self) {
        self.recovery.owed = true;
    }
}

/// This computer as the owner of a floating rule, with the zone its system
/// reports now. `None` when no zone source is installed or it cannot tell.
pub(in crate::worker) fn this_computer(
    zone: Option<&ZoneSource>,
    device: koru_delegate_core::DeviceId,
) -> Option<RoutineThisComputer> {
    zone.and_then(|zone| zone())
        .and_then(|iana| RoutineThisComputer::detected(device, &iana))
}

fn refusal(code: &'static str, retryable: bool) -> Failure {
    failure(
        code,
        "The Routine control could not be completed.",
        retryable,
    )
}
fn unknown() -> Failure {
    failure(
        codes::CONTROL_UNKNOWN,
        "The Routine's owner did not answer in time. Read the Routine again.",
        true,
    )
}
fn not_ready(reasons: &[BotRoutineNotReadyReason]) -> Failure {
    let mut refused = failure(codes::NOT_READY, "The Routine cannot run now.", false);
    refused.details.insert(
        "reasons".into(),
        reasons
            .iter()
            .map(|reason| reason.token())
            .collect::<Vec<_>>()
            .into(),
    );
    refused
}

/// The coordinator's refusal, as the owner command answers it. A transport
/// failure of a call that may have written is unknown, never "not applied":
/// the transport cannot tell an unreachable owner from a lost reply.
fn owner_refusal(error: &koru_delegate_api::Failure, mutation: bool) -> Failure {
    match error.code.as_str() {
        "routine_bot_paused" => not_ready(&[BotRoutineNotReadyReason::BotPaused]),
        "schedule_revision_conflict" | "schedule_name_conflict" => {
            bot_failure(BotError::RevisionConflict)
        }
        "schedule_state_conflict" => refusal(codes::STATE_CONFLICT, false),
        "schedule_not_found" | "schedule_retired" => refusal(codes::NOT_LIVE, false),
        "schedule_not_owner" | "schedule_handoff_pending" => refusal(codes::OWNER_ELSEWHERE, false),
        "invalid_schedule" | "invalid_request" => bot_failure(BotError::Invalid),
        "routine_test_requires_paused" => refusal(codes::MUST_BE_PAUSED, false),
        "routine_test_minute_changed" => refusal(codes::TEST_MINUTE_ENDING, true),
        "routine_test_too_soon" => refusal(codes::TEST_TOO_SOON, true),
        "routine_test_minute_taken" => refusal(codes::TEST_MINUTE_TAKEN, false),
        "routine_test_busy" => refusal(codes::TEST_BUSY, true),
        "delegate_unavailable" if mutation => unknown(),
        _ => refusal(codes::UNAVAILABLE, error.retryable || !mutation),
    }
}

/// What the Bot and the Routine's creation row were when the control was
/// admitted. Read again before the answer: a change fails the response. It
/// does not undo the Schedule write, and it does not need to.
#[derive(Clone, PartialEq, Eq)]
struct Subject {
    owner: BotOwner,
    bot: BotId,
    home: WorkspaceId,
    revision: u64,
    policy_revision: u64,
    paused: bool,
    lifecycle: BotLifecycle,
    held: bool,
    creation: BotRoutineCreation,
}

#[derive(Clone)]
enum Kind {
    Lifecycle(BotRoutineLifecycleAction),
    Edit {
        expected_name: RoutineScheduleName,
        name: RoutineScheduleName,
        recurrence: RoutineRecurrenceRule,
    },
    Test {
        retried: bool,
    },
    Access {
        update: Box<BotRoutineAccessUpdate>,
        fingerprint: RoutineDigest32,
    },
    /// A restart's repeat of a `prepared` edit's barrier install. No caller.
    AccessRecovery {
        request: BotRequestId,
    },
}
impl Kind {
    fn mutation(&self) -> bool {
        matches!(
            self,
            Self::Lifecycle(_) | Self::Edit { .. } | Self::Test { .. }
        )
    }
}
enum Work {
    Control { expected: u32 },
    Test { expected: u32, minute: i64 },
    AccessCheck,
    AccessInstall { barrier: Box<RoutinePolicyBarrier> },
}
enum Evidence {
    Controlled(Box<BotRoutineControl>),
    Tested {
        minute: i64,
    },
    MinuteChanged,
    AccessChecked {
        paused: bool,
        page_forbidden: bool,
        unsettled: Vec<RunId>,
        more: bool,
    },
    AccessInstalled,
}

pub(in crate::worker) struct ControlJob {
    ticket: Option<listing::Ticket>,
    context: Option<CommandContext>,
    reply: Option<SyncSender<ApiResult<BotResponse>>>,
    serial: u64,
    kind: Kind,
    work: Work,
    subject: Subject,
    /// The Routine's requesting Person, who must still hold its home.
    profile: ProfileId,
    cancellation: Cancellation,
    deadline: Instant,
    observed_at_ms: i64,
    authority: Arc<dyn BotAuthority>,
    delegate: Arc<dyn BotDelegatePort>,
    zone: Option<ZoneSource>,
}
pub(in crate::worker) struct ControlResult {
    job: ControlJob,
    result: ApiResult<Evidence>,
}

impl ControlJob {
    pub(in crate::worker) fn background(&self) -> bool {
        matches!(self.kind, Kind::AccessRecovery { .. })
    }
    fn schedule(&self) -> ScheduleId {
        self.subject.creation.identity.schedule
    }
    fn live(&self) -> ApiResult<()> {
        if let Some(context) = &self.context {
            authorize(context, AiScope::ManageBots)?;
        }
        // A control cut short (its owner epoch changed, or its deadline
        // passed) may or may not have reached the coordinator: for one that
        // writes the Schedule that is unknown, never "not applied".
        if self.cancellation.is_cancelled()
            || self.ticket.as_ref().is_some_and(|t| !t.valid())
            || Instant::now() >= self.deadline
        {
            return Err(self.cut_short());
        }
        Ok(())
    }
    fn cut_short(&self) -> Failure {
        if self.kind.mutation() {
            unknown()
        } else {
            refusal(codes::UNAVAILABLE, true)
        }
    }
    /// Fresh owner and home authority, read before and after every owner
    /// call. `person` also requires the Routine's requesting Person to still
    /// hold the home.
    fn current(&self, pages: &[BotPage], person: bool) -> ApiResult<BotAuthoritySnapshot> {
        self.live()?;
        let proof = self
            .authority
            .inspect(&[self.subject.home], pages)
            .map_err(bot_failure)?;
        if proof.owner != self.subject.owner {
            return Err(bot_failure(BotError::Forbidden));
        }
        let profile = proof.workspace(self.subject.home).map_err(bot_failure)?;
        if person && profile != self.profile {
            return Err(bot_failure(BotError::Forbidden));
        }
        self.live()?;
        Ok(proof)
    }
    /// The live Schedule, read at its exact retained location. It must be
    /// live and owned by this computer with no handoff pending; whether it is
    /// on or paused, and at which revision, is the coordinator's to judge.
    fn observe(&self) -> ApiResult<(RoutineRecurrenceRule, bool)> {
        let observed = self
            .authority
            .read_routine_detail(
                self.subject.owner,
                self.profile,
                &self.subject.creation.identity,
                self.observed_at_ms,
                &self.cancellation,
            )
            .map_err(bot_failure)?;
        self.live()?;
        match observed {
            RoutineDetailObservation::NotLive => Err(refusal(codes::NOT_LIVE, false)),
            RoutineDetailObservation::Unavailable => Err(bot_failure(BotError::Unavailable)),
            RoutineDetailObservation::Current {
                recurrence, state, ..
            } => match state {
                RoutineScheduleState::Active { owner } if owner == self.subject.owner.device => {
                    Ok((recurrence, false))
                }
                RoutineScheduleState::Paused { owner, .. }
                    if owner == self.subject.owner.device =>
                {
                    Ok((recurrence, true))
                }
                RoutineScheduleState::Retired => Err(refusal(codes::NOT_LIVE, false)),
                _ => Err(refusal(codes::OWNER_ELSEWHERE, false)),
            },
        }
    }
    fn call(&self, command: DelegateCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        let remaining = self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(OWNER_WITHIN);
        if remaining.is_zero() || self.cancellation.is_cancelled() {
            return Err(koru_delegate_api::Failure::unavailable());
        }
        let started = Instant::now();
        let reply = self.delegate.execute_within(command, remaining)?;
        // A port that ignores its bound still cannot deliver a late answer.
        if started.elapsed() >= remaining {
            return Err(koru_delegate_api::Failure::unavailable());
        }
        Ok(reply)
    }
    pub(in crate::worker) fn run(self) -> ControlResult {
        let result = catch_unwind(AssertUnwindSafe(|| self.execute()))
            .unwrap_or_else(|_| Err(self.cut_short()));
        ControlResult { job: self, result }
    }
    fn execute(&self) -> ApiResult<Evidence> {
        self.live()?;
        let identity = &self.subject.creation.identity;
        let route = identity.route();
        match &self.work {
            Work::Control { expected } => {
                self.current(&[], true)?;
                let (observed, _) = self.observe()?;
                let (action, public, recurrence, wanted) = match &self.kind {
                    Kind::Lifecycle(BotRoutineLifecycleAction::Enable) => (
                        RoutineControlAction::Enable {},
                        BotRoutineControlAction::Enable,
                        observed,
                        Some(koru_delegate_core::RoutineControlLifecycle::Active),
                    ),
                    Kind::Lifecycle(BotRoutineLifecycleAction::Pause) => (
                        RoutineControlAction::Pause {},
                        BotRoutineControlAction::Pause,
                        observed,
                        Some(koru_delegate_core::RoutineControlLifecycle::Paused),
                    ),
                    Kind::Edit {
                        expected_name,
                        name,
                        recurrence,
                    } => (
                        RoutineControlAction::Edit {
                            expected_name: expected_name.clone(),
                            name: name.clone(),
                            recurrence: recurrence.clone(),
                        },
                        BotRoutineControlAction::Edit,
                        recurrence.clone(),
                        None,
                    ),
                    _ => return Err(bot_failure(BotError::Invalid)),
                };
                let reply = self
                    .call(DelegateCommand::ControlRoutine {
                        route: route.clone(),
                        expected_revision: *expected,
                        action,
                    })
                    .map_err(|error| owner_refusal(&error, true))?;
                // From here the Schedule may have been written. A changed
                // authority fails the response; nothing is undone.
                self.current(&[], true)?;
                let BotReply::RoutineControlled {
                    route: echoed,
                    outcome,
                    definition,
                    effective_at_ms,
                    lifecycle,
                    ..
                } = reply
                else {
                    return Err(unknown());
                };
                if echoed != route
                    || !definition.valid()
                    || wanted.is_some_and(|wanted| wanted != lifecycle)
                {
                    return Err(unknown());
                }
                let device = self.subject.owner.device;
                let state = routine_control_state(lifecycle, device);
                let this = this_computer(self.zone.as_ref(), device);
                let (_, next) = routine_zone_and_next(
                    &recurrence,
                    &state,
                    effective_at_ms,
                    self.observed_at_ms,
                    this.as_ref(),
                );
                Ok(Evidence::Controlled(Box::new(BotRoutineControl {
                    schedule_id: identity.schedule,
                    action: public,
                    outcome,
                    definition_revision: definition.revision,
                    state,
                    next,
                })))
            }
            Work::Test { expected, minute } => {
                self.current(&[], true)?;
                let (_, paused) = self.observe()?;
                if !paused {
                    return Err(refusal(codes::MUST_BE_PAUSED, false));
                }
                let slot = RoutineSlot {
                    owner: identity.owner,
                    bot: identity.bot,
                    schedule: identity.schedule,
                    occurrence_ms: *minute,
                };
                let reply = match self.call(DelegateCommand::TestRoutine {
                    route,
                    expected_revision: *expected,
                    occurrence_ms: *minute,
                }) {
                    Ok(reply) => reply,
                    Err(error) if error.code == "routine_test_minute_changed" => {
                        return Ok(Evidence::MinuteChanged);
                    }
                    Err(error) => return Err(owner_refusal(&error, true)),
                };
                self.current(&[], true)?;
                match reply {
                    BotReply::RoutineTestAccepted {
                        slot: accepted,
                        definition,
                    } if accepted == slot && definition.revision == *expected => {
                        Ok(Evidence::Tested { minute: *minute })
                    }
                    _ => Err(unknown()),
                }
            }
            Work::AccessCheck => {
                let Kind::Access { update, .. } = &self.kind else {
                    return Err(bot_failure(BotError::Invalid));
                };
                let proof = self.current(&update.selected_pages, true)?;
                let mut page_forbidden = false;
                for page in &update.selected_pages {
                    match proof.page(page) {
                        Ok(()) => {}
                        // The person may not give what they cannot edit.
                        Err(BotError::Forbidden) => page_forbidden = true,
                        Err(error) => return Err(bot_failure(error)),
                    }
                }
                let (_, paused) = self.observe()?;
                let owner = identity.owner;
                let work = match self
                    .call(DelegateCommand::UnsettledWork {
                        owner,
                        bot: identity.bot,
                        limit: UNSETTLED_PAGE,
                    })
                    .map_err(|error| owner_refusal(&error, false))?
                {
                    BotReply::UnsettledWork { work }
                        if work.admissions.iter().all(|a| {
                            a.admission.key.owner == owner && a.admission.key.bot == identity.bot
                        }) =>
                    {
                        work
                    }
                    _ => return Err(refusal(codes::UNAVAILABLE, true)),
                };
                self.current(&update.selected_pages, true)?;
                Ok(Evidence::AccessChecked {
                    paused,
                    page_forbidden,
                    unsettled: work.admissions.iter().map(|a| a.admission.run).collect(),
                    more: work.more,
                })
            }
            Work::AccessInstall { barrier } => {
                // A prepared edit always completes its install: only the
                // owner and its home are required, not the person's Profile.
                self.current(&[], false)?;
                match self
                    .call(DelegateCommand::SynchronizeRoutinePolicy {
                        barrier: (**barrier).clone(),
                    })
                    .map_err(|error| owner_refusal(&error, false))?
                {
                    BotReply::RoutinePolicyBarrier {
                        barrier: installed, ..
                    } if installed == **barrier => Ok(Evidence::AccessInstalled),
                    _ => Err(refusal(codes::UNAVAILABLE, true)),
                }
            }
        }
    }
}

/// What the writer does with a control after its start or one of its phases.
enum Next {
    Answer(BotResponse),
    Run(Work),
}

fn access_response(
    schedule: ScheduleId,
    record: &BotRoutineAccessRecord,
) -> ApiResult<BotResponse> {
    match (&record.receipt.outcome, record.receipt.summary()) {
        (_, Some(summary)) => Ok(BotResponse::RoutineAccess {
            access: BotRoutineAccess::of(schedule, &summary),
        }),
        (BotRoutineAccessOutcome::Refused { reason }, _) => Err(match reason {
            BotRoutineAccessRefusal::NotPaused => refusal(codes::MUST_BE_PAUSED, false),
            BotRoutineAccessRefusal::PageForbidden => bot_failure(BotError::Forbidden),
        }),
        _ => Err(bot_failure(BotError::Storage)),
    }
}

impl WorkerState {
    /// The Bot and the Routine's creation row, as a control's common
    /// authority. Read at admission and again before the answer.
    fn routine_control_subject(
        &self,
        owner: BotOwner,
        bot_id: BotId,
        schedule: ScheduleId,
    ) -> ApiResult<Subject> {
        let (bot, lifecycle) = self
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
        if lifecycle == BotLifecycle::Trashed {
            return Err(lifecycle::hold_failure(BotHold::BotTrashed));
        }
        let creation = self
            .repository
            .bot_routine_creation(owner, bot_id, schedule)
            .map_err(bot_failure)?
            .ok_or_else(not_found)?;
        if creation.identity.home != bot.home_workspace
            || creation.identity.owner.user_scope != owner.user_scope
            || creation.identity.owner.device != owner.device
        {
            return Err(bot_failure(BotError::Forbidden));
        }
        Ok(Subject {
            owner,
            bot: bot_id,
            home: bot.home_workspace,
            revision: bot.revision,
            policy_revision: bot.policy_revision,
            paused: bot.paused,
            lifecycle,
            held: self
                .repository
                .bot_hold(bot_id)
                .map_err(bot_failure)?
                .is_some(),
            creation,
        })
    }

    /// One owner control, after the ordinary Bot preparation proved the
    /// caller's owner and its authority over the Bot's home.
    pub(in crate::worker) fn start_routine_control(
        &mut self,
        context: CommandContext,
        request: BotCommand,
        reply: SyncSender<ApiResult<BotResponse>>,
    ) {
        match self.admit_routine_control(&context, &request) {
            Ok(Admitted::Answer(response)) => {
                let _ = reply.send(Ok(response));
            }
            Ok(Admitted::Job(mut job)) => {
                job.context = Some(context);
                job.reply = Some(reply);
                self.submit_routine_control(*job);
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    fn admit_routine_control(
        &mut self,
        context: &CommandContext,
        request: &BotCommand,
    ) -> ApiResult<Admitted> {
        authorize(context, AiScope::ManageBots)?;
        validate_routine_control_bytes(request).map_err(bot_failure)?;
        let (bot_id, schedule) = request
            .routine_control()
            .ok_or_else(|| bot_failure(BotError::Invalid))?;
        validate_routine_list_request(bot_id, Some(schedule), 1).map_err(bot_failure)?;
        let proof = context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        // The owner is the vouched one, never a request field.
        let owner = proof.authority.owner().map_err(bot_failure)?;
        let subject = self.routine_control_subject(owner, bot_id, schedule)?;
        proof
            .authority
            .workspace(subject.home)
            .map_err(bot_failure)?;
        let (kind, expected) = match request {
            BotCommand::SetRoutineLifecycle {
                expected_definition_revision,
                action,
                ..
            } => (Kind::Lifecycle(*action), *expected_definition_revision),
            BotCommand::TestRoutine {
                expected_definition_revision,
                ..
            } => (Kind::Test { retried: false }, *expected_definition_revision),
            BotCommand::EditRoutine {
                expected_definition_revision,
                expected_name,
                name,
                recurrence,
                ..
            } => (
                Kind::Edit {
                    expected_name: expected_name.clone(),
                    name: name.clone(),
                    recurrence: recurrence.clone(),
                },
                *expected_definition_revision,
            ),
            BotCommand::UpdateRoutineAccess {
                request_id,
                expected_grant_revision,
                access,
                selected_pages,
                ..
            } => {
                let mut pages = selected_pages.clone();
                normalize_pages(&mut pages, subject.home).map_err(bot_failure)?;
                let update = BotRoutineAccessUpdate {
                    request: *request_id,
                    bot: bot_id,
                    schedule,
                    expected_grant_revision: *expected_grant_revision,
                    access: *access,
                    selected_pages: pages.clone(),
                };
                update.validate().map_err(bot_failure)?;
                // The existing bare JSON hash convention, over the normalized
                // command, so the order pages were listed in is not identity.
                let fingerprint = koru_delegate_core::routine_digest_json(
                    b"",
                    &BotCommand::UpdateRoutineAccess {
                        request_id: *request_id,
                        bot_id,
                        schedule_id: schedule,
                        expected_grant_revision: *expected_grant_revision,
                        access: *access,
                        selected_pages: pages,
                    },
                    BOT_ROUTINE_CONTROL_BYTES,
                )
                .map_err(|_| bot_failure(BotError::Invalid))?;
                (
                    Kind::Access {
                        update: Box::new(update),
                        fingerprint,
                    },
                    1,
                )
            }
            _ => return Err(bot_failure(BotError::Invalid)),
        };
        if expected == 0 {
            return Err(bot_failure(BotError::Invalid));
        }
        let ActorRef::Person { profile } = subject
            .creation
            .identity
            .seed
            .header
            .definition
            .template
            .requested_by
        else {
            return Err(bot_failure(BotError::Forbidden));
        };

        // An exact retry of a retained access edit is answered from its
        // receipt before any current-state refusal, even after a later edit.
        let mut retained = None;
        if let Kind::Access {
            update,
            fingerprint,
        } = &kind
            && self
                .repository
                .routine_access_record(owner, bot_id, update.request)
                .map_err(bot_failure)?
                .is_some()
        {
            match self
                .repository
                .prepare_routine_access_update(
                    owner,
                    update,
                    *fingerprint,
                    self.routine_controls.now_ms(),
                )
                .map_err(bot_failure)?
            {
                BotRoutineAccessAdmission::Retained(record)
                | BotRoutineAccessAdmission::Prepared(record) => {
                    if record.stage != BotReceiptStage::Prepared {
                        return access_response(schedule, &record).map(Admitted::Answer);
                    }
                    retained = Some(record);
                }
                BotRoutineAccessAdmission::Pending { .. } => {
                    return Err(refusal(codes::ACCESS_PENDING, true));
                }
            }
        }

        let archived = subject.lifecycle == BotLifecycle::Archived;
        let pausing = matches!(kind, Kind::Lifecycle(BotRoutineLifecycleAction::Pause));
        let readiness = matches!(
            kind,
            Kind::Lifecycle(BotRoutineLifecycleAction::Enable) | Kind::Test { .. }
        );
        let bound = subject.creation.state == BotRoutineCreationState::Bound;
        let mut work = None;
        if let Some(record) = &retained {
            // Its install is all that is left; it completes whatever the Bot
            // is doing now, as long as the Bot is not in Trash.
            work = Some(self.routine_access_install(&subject, record)?);
        } else {
            // An Archive, Trash or Restore of the Bot holds every control a
            // person sends. The pause that operation sends is the driver's.
            if let Some(hold) = self.repository.bot_hold(bot_id).map_err(bot_failure)? {
                return Err(lifecycle::hold_failure(hold));
            }
            if archived && !pausing && !readiness {
                return Err(lifecycle::archived_failure());
            }
            // Pause only reduces work: a Bot change still being delivered
            // does not hold it.
            if !pausing && self.bot_pending_change(bot_id)?.is_some() {
                return Err(bot_failure(BotError::Settling));
            }
            let current = self
                .repository
                .routine_current_grant(owner, bot_id, schedule);
            let access_pending = self
                .repository
                .routine_access_pending(owner, bot_id, schedule)
                .map_err(bot_failure)?
                .is_some();
            if readiness {
                let reasons: Vec<_> = [
                    (subject.paused, BotRoutineNotReadyReason::BotPaused),
                    (archived, BotRoutineNotReadyReason::BotArchived),
                    (!bound, BotRoutineNotReadyReason::CreationNotBound),
                    (
                        current.is_err(),
                        BotRoutineNotReadyReason::AccessUnavailable,
                    ),
                    (access_pending, BotRoutineNotReadyReason::AccessPending),
                ]
                .into_iter()
                .filter_map(|(blocked, reason)| blocked.then_some(reason))
                .collect();
                if !reasons.is_empty() {
                    return Err(not_ready(&reasons));
                }
            } else if !bound {
                return Err(refusal(codes::NOT_BOUND, false));
            }
            if let Kind::Access { update, .. } = &kind {
                if access_pending {
                    return Err(refusal(codes::ACCESS_PENDING, true));
                }
                if current.map_err(bot_failure)?.grant.grant_revision
                    != update.expected_grant_revision
                {
                    return Err(bot_failure(BotError::RevisionConflict));
                }
            }
        }
        let (Some(authority), Some(delegate)) =
            (self.bot_authority.clone(), self.bot_delegate.clone())
        else {
            return Err(refusal(codes::UNAVAILABLE, true));
        };
        let now = self.routine_controls.now_ms();
        let (work, test) = match (work, &kind) {
            (Some(work), _) => (work, None),
            (None, Kind::Test { .. }) => {
                let minute = routine_test_minute(now)
                    .ok_or_else(|| refusal(codes::TEST_MINUTE_ENDING, true))?;
                (
                    Work::Test { expected, minute },
                    Some(TestSlot {
                        minute,
                        revision: expected,
                    }),
                )
            }
            (None, Kind::Access { .. }) => (Work::AccessCheck, None),
            (None, _) => (Work::Control { expected }, None),
        };
        let ticket = self.routine_listings.acquire()?;
        self.routine_listings.bind_owner(owner)?;
        let serial = self.routine_controls.acquire(schedule, test)?;
        Ok(Admitted::Job(Box::new(ControlJob {
            ticket: Some(ticket),
            context: None,
            reply: None,
            serial,
            kind,
            work,
            subject,
            profile,
            cancellation: Cancellation::default(),
            deadline: Instant::now() + CONTROL_DEADLINE,
            observed_at_ms: now,
            authority,
            delegate,
            zone: self.routine_controls.zone(),
        })))
    }

    /// The barrier of a `prepared` edit, built from the grant row it wrote.
    fn routine_access_install(
        &self,
        subject: &Subject,
        record: &BotRoutineAccessRecord,
    ) -> ApiResult<Work> {
        let identity = &subject.creation.identity;
        let current = self
            .repository
            .routine_current_grant(subject.owner, subject.bot, identity.schedule)
            .map_err(bot_failure)?;
        // Nothing can replace a grant row while its edit is prepared.
        if current.request != record.request || record.receipt.schedule != identity.schedule {
            return Err(bot_failure(BotError::Storage));
        }
        Ok(Work::AccessInstall {
            barrier: Box::new(policy::current_routine_barrier(identity, &current)),
        })
    }

    fn submit_routine_control(&mut self, job: ControlJob) {
        if let Err(error) = job.live() {
            return self.fail_routine_control(job, error);
        }
        let Some(pool) = &self.provider_admin else {
            return self.fail_routine_control(job, refusal(codes::UNAVAILABLE, true));
        };
        let job = if job.background() {
            ProviderAdminJob::BotRoutineAccessRecovery(Box::new(job))
        } else {
            ProviderAdminJob::BotRoutineControl(Box::new(job))
        };
        match pool.try_submit(job) {
            Ok(()) => {}
            Err(AdminSubmitError {
                job:
                    ProviderAdminJob::BotRoutineControl(job)
                    | ProviderAdminJob::BotRoutineAccessRecovery(job),
                failure,
            }) => self.fail_routine_control(*job, failure),
            Err(_) => unreachable!("control job returned another job"),
        }
    }

    /// The control ended refused. An unknown Test keeps its minute expected:
    /// the owner may have accepted it.
    fn fail_routine_control(&mut self, job: ControlJob, error: Failure) {
        let keep_test =
            matches!(job.kind, Kind::Test { .. }) && error.code == codes::CONTROL_UNKNOWN;
        self.routine_controls
            .release(job.schedule(), job.serial, keep_test);
        match &job.kind {
            Kind::AccessRecovery { .. } => self.defer_routine_access_recovery(),
            // A prepared edit whose install did not finish is still owed it.
            Kind::Access { .. } => self.routine_controls.access_owed(),
            _ => {}
        }
        if let Some(reply) = &job.reply {
            let _ = reply.send(Err(error));
        }
    }

    pub(in crate::worker) fn complete_routine_control(&mut self, result: ControlResult) {
        let ControlResult { mut job, result } = result;
        let next = (|| -> ApiResult<Next> {
            job.live()?;
            if job
                .ticket
                .as_ref()
                .is_some_and(|ticket| !self.routine_listings.owns(ticket))
            {
                return Err(job.cut_short());
            }
            let evidence = result?;
            let subject = &job.subject;
            let (owner, bot, schedule) = (subject.owner, subject.bot, job.schedule());
            match (&mut job.kind, evidence) {
                (Kind::Lifecycle(_) | Kind::Edit { .. }, Evidence::Controlled(control)) => {
                    self.recheck_routine_control(subject)?;
                    let response = BotResponse::RoutineControl { control: *control };
                    validate_routine_control_bytes(&response).map_err(bot_failure)?;
                    Ok(Next::Answer(response))
                }
                (Kind::Test { .. }, Evidence::Tested { minute }) => {
                    self.recheck_routine_control(subject)?;
                    Ok(Next::Answer(BotResponse::RoutineTest {
                        test: BotRoutineTest {
                            schedule_id: schedule,
                            occurrence_ms: minute,
                        },
                    }))
                }
                // The owner's clock is in the next minute. Once, and never
                // by itself into a later one after that.
                (Kind::Test { retried }, Evidence::MinuteChanged) => {
                    let Work::Test { expected, .. } = job.work else {
                        return Err(bot_failure(BotError::Invalid));
                    };
                    let minute = (!*retried)
                        .then(|| routine_test_minute(self.routine_controls.now_ms()))
                        .flatten()
                        .ok_or_else(|| refusal(codes::TEST_MINUTE_ENDING, true))?;
                    *retried = true;
                    self.routine_controls
                        .retarget_test(schedule, job.serial, minute);
                    Ok(Next::Run(Work::Test { expected, minute }))
                }
                (
                    Kind::Access {
                        update,
                        fingerprint,
                    },
                    Evidence::AccessChecked {
                        paused,
                        page_forbidden,
                        unsettled,
                        more,
                    },
                ) => {
                    self.recheck_routine_control(subject)?;
                    let now = self.routine_controls.now_ms();
                    // A refusal the same request would meet again is kept, so
                    // its retry gets the same answer. A busy Routine is not:
                    // its retry must be able to succeed.
                    let refused = if page_forbidden {
                        Some(BotRoutineAccessRefusal::PageForbidden)
                    } else if !paused {
                        Some(BotRoutineAccessRefusal::NotPaused)
                    } else {
                        None
                    };
                    if let Some(reason) = refused {
                        let record = self
                            .repository
                            .refuse_routine_access_update(owner, update, *fingerprint, reason, now)
                            .map_err(bot_failure)?;
                        return access_response(schedule, &record).map(Next::Answer);
                    }
                    // No Run of this Routine may be unsettled: after an edit
                    // its next effect would be refused, not run under the
                    // old access. The match key is the preparation's Run id.
                    if more {
                        return Err(refusal(codes::RUN_IN_PROGRESS, true));
                    }
                    for run in unsettled {
                        if self
                            .repository
                            .routine_occurrence_for_run(owner, bot, run)
                            .map_err(bot_failure)?
                            .is_some_and(|row| row.prepared.slot.schedule == schedule)
                        {
                            return Err(refusal(codes::RUN_IN_PROGRESS, true));
                        }
                    }
                    match self
                        .repository
                        .prepare_routine_access_update(owner, update, *fingerprint, now)
                        .map_err(bot_failure)?
                    {
                        BotRoutineAccessAdmission::Prepared(record)
                        | BotRoutineAccessAdmission::Retained(record) => {
                            if record.stage != BotReceiptStage::Prepared {
                                return access_response(schedule, &record).map(Next::Answer);
                            }
                            self.routine_access_install(subject, &record).map(Next::Run)
                        }
                        BotRoutineAccessAdmission::Pending { .. } => {
                            Err(refusal(codes::ACCESS_PENDING, true))
                        }
                    }
                }
                (Kind::Access { update, .. }, Evidence::AccessInstalled) => {
                    let record = self
                        .repository
                        .complete_routine_access_update(
                            owner,
                            bot,
                            update.request,
                            self.routine_controls.now_ms(),
                        )
                        .map_err(bot_failure)?;
                    access_response(schedule, &record).map(Next::Answer)
                }
                (Kind::AccessRecovery { request }, Evidence::AccessInstalled) => {
                    let record = self
                        .repository
                        .complete_routine_access_update(
                            owner,
                            bot,
                            *request,
                            self.routine_controls.now_ms(),
                        )
                        .map_err(bot_failure)?;
                    access_response(schedule, &record).map(Next::Answer)
                }
                _ => Err(unavailable_failure()),
            }
        })();
        match next {
            Ok(Next::Run(work)) => {
                job.work = work;
                self.submit_routine_control(job);
            }
            Ok(Next::Answer(response)) => {
                self.routine_controls.release(
                    job.schedule(),
                    job.serial,
                    matches!(job.kind, Kind::Test { .. }),
                );
                if matches!(job.kind, Kind::AccessRecovery { .. }) {
                    self.routine_controls.recovery.active = None;
                    self.routine_controls.recovery.failures = 0;
                    self.routine_controls.recovery.retry_at = None;
                }
                if let Some(reply) = &job.reply {
                    let _ = reply.send(Ok(response));
                }
            }
            Err(error) => self.fail_routine_control(job, error),
        }
    }

    /// Step 6 of the authority chain: the Bot's revision, lifecycle and hold,
    /// and the creation row, read again before the answer.
    fn recheck_routine_control(&self, subject: &Subject) -> ApiResult<()> {
        let current = self.routine_control_subject(
            subject.owner,
            subject.bot,
            subject.creation.identity.schedule,
        )?;
        if current != *subject {
            return Err(bot_failure(BotError::RevisionConflict));
        }
        Ok(())
    }

    fn defer_routine_access_recovery(&mut self) {
        let recovery = &mut self.routine_controls.recovery;
        recovery.active = None;
        recovery.owed = true;
        recovery.failures = recovery.failures.saturating_add(1);
        recovery.retry_at = Some(
            Instant::now()
                + Duration::from_secs(
                    (1u64 << recovery.failures.min(9).saturating_sub(1)).min(300),
                ),
        );
    }

    /// Repeat the barrier install of each access edit still `prepared`, one
    /// at a time on the background administration lane: after a restart, and
    /// after an install that did not finish. Cheap when nothing is owed.
    pub(in crate::worker) fn schedule_routine_access_recovery(&mut self) {
        let recovery = &self.routine_controls.recovery;
        if !recovery.owed
            || recovery.active.is_some()
            || recovery.retry_at.is_some_and(|at| at > Instant::now())
        {
            return;
        }
        let (Some(authority), Some(delegate), Some(_)) = (
            self.bot_authority.clone(),
            self.bot_delegate.clone(),
            self.provider_admin.as_ref(),
        ) else {
            return;
        };
        // Every prepared edit, a bounded page at a time, until one is
        // installable. The index holds only edits still `prepared`.
        let mut pending = Vec::new();
        let mut after = 0;
        loop {
            let page = match self.repository.pending_routine_access(after, 16) {
                Ok(page) => page,
                Err(_) => return self.defer_routine_access_recovery(),
            };
            let Some(through) = page.scanned_through else {
                break;
            };
            pending.extend(page.pending);
            if pending.len() >= 256 || through <= after {
                break;
            }
            after = through;
        }
        let mut waiting = false;
        for (_, bot_id, scope, request) in pending {
            let prepared = (|| -> ApiResult<Option<ControlJob>> {
                let Some((bot, lifecycle)) = self
                    .repository
                    .bot_any_lifecycle(bot_id)
                    .map_err(bot_failure)?
                else {
                    return Ok(None);
                };
                // A trashed Bot's edit is installed once the Bot is restored.
                if bot.user_scope != scope || lifecycle == BotLifecycle::Trashed {
                    return Ok(None);
                }
                let owner = BotOwner {
                    user_scope: bot.user_scope,
                    device: bot.owner_device,
                };
                let Some(record) = self
                    .repository
                    .routine_access_record(owner, bot_id, request)
                    .map_err(bot_failure)?
                else {
                    return Ok(None);
                };
                let schedule = record.receipt.schedule;
                let subject = self.routine_control_subject(owner, bot_id, schedule)?;
                let ActorRef::Person { profile } = subject
                    .creation
                    .identity
                    .seed
                    .header
                    .definition
                    .template
                    .requested_by
                else {
                    return Ok(None);
                };
                let work = self.routine_access_install(&subject, &record)?;
                // A person's own retry is installing it now.
                let Ok(serial) = self.routine_controls.acquire(schedule, None) else {
                    return Err(refusal(codes::CONTROL_PENDING, true));
                };
                Ok(Some(ControlJob {
                    ticket: None,
                    context: None,
                    reply: None,
                    serial,
                    kind: Kind::AccessRecovery { request },
                    work,
                    subject,
                    profile,
                    cancellation: Cancellation::default(),
                    deadline: Instant::now() + CONTROL_DEADLINE,
                    observed_at_ms: self.routine_controls.now_ms(),
                    authority: authority.clone(),
                    delegate: delegate.clone(),
                    zone: None,
                }))
            })();
            match prepared {
                Ok(Some(job)) => {
                    self.routine_controls.recovery.active = Some(request);
                    self.submit_routine_control(job);
                    return;
                }
                Ok(None) => {}
                Err(_) => waiting = true,
            }
        }
        if waiting {
            self.defer_routine_access_recovery();
        } else {
            // Nothing this worker can install is pending.
            self.routine_controls.recovery.owed = false;
        }
    }
}

enum Admitted {
    Answer(BotResponse),
    Job(Box<ControlJob>),
}
