// SPDX-License-Identifier: MPL-2.0

//! The actor that owns the orchestrator.
//!
//! One thread holds the `Orchestrator` and everything it drives — the
//! run-side ledger port, the open Run documents, the session controllers.
//! Every API call is a [`Command`] sent to it with a reply channel, every
//! run's session stream is pumped into it by a thread of that run's own,
//! and every stream a caller opened is a watcher it feeds after each
//! change. Run lifecycle and session transitions stay on exactly this one
//! background thread, never on a socket connection thread. The separate
//! schedule thread owns discovery/preflight and its claim/history writes.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use koru_delegate_api::ipc::Response;
use koru_delegate_api::wire::{
    CancelRunResponse, CreateRunRequest, CreateRunResponse, Failure, ListRunsResponse,
    PurgeRunResponse, RateRunRequest, RateRunResponse, RestoreRunResponse, ResumeRunResponse,
    RunExecution, RunStreamItem, RunView, TimelineItem, TimelineQuery, TimelineWindow,
    TrashRunResponse, TrashedRun,
};
use koru_delegate_core::{
    ArtifactDisposition, DelegationRequest, DeviceId, RunId, RunStatus, WorkspaceId,
};
use koru_delegate_runtime::{
    CreateError, CreateIntentStatus, HandleOutcome, Orchestrator, PortError, PortResult, RunError,
    ScheduleAdmission, ScheduleEffects, ScheduledScopeReservation, ScopeReservationResolution,
    SessionUpdate, StartOrigin, Started, WindowItem,
};
use koru_delegate_scope::ScopeIdentity;
use koru_delegate_sqlite::SlotClaim;

/// How often the actor retries publications the bus did not take.
const TICK: Duration = Duration::from_secs(5);
/// How soon a manual `Created` run recovered at restart retries a transient
/// pre-attempt start failure. This deadline exists only while that unhealthy
/// work is pending; healthy idle still blocks indefinitely.
const DEFERRED_START_RETRY: Duration = Duration::from_secs(1);
/// How long a pump waits on its stream before checking whether it should
/// stop.
const PUMP_POLL: Duration = Duration::from_millis(250);
/// How often the actor takes in the Store's acknowledgements while a commit
/// is in flight. Cheap — a non-blocking read per open run — and what turns
/// a queued fact into a delivered one.
const SETTLE_POLL: Duration = Duration::from_millis(250);
/// The most history entries one notification pushes to one watcher.
const PAGE: usize = 200;
/// How often a run this device is only *reading* is asked what the Store
/// has sent it. A run this device drives is written here and needs none of
/// this; a run watched from a second device has no other way to move
/// (D15, UC5). Only runs with a watcher are asked, so an idle worker does
/// no work at all.
const WATCH_POLL: Duration = Duration::from_secs(1);

pub enum Command {
    BotDrain,
    /// Wake only; the bounded worker completion channel owns the value.
    PublicationWake,
    Bot {
        command: koru_delegate_api::bots::BotCommand,
        reply: mpsc::SyncSender<Response>,
    },
    Stop {
        why: &'static str,
    },
    ScheduleBegin {
        run: RunId,
        scope: ScopeIdentity,
        authored_scope: koru_delegate_core::WorkspaceScope,
        reply: mpsc::SyncSender<PortResult<ScheduleAdmission>>,
    },
    RoutineBegin {
        prepared: Box<koru_delegate_core::RoutinePrepared>,
        admission: koru_delegate_core::BotAdmissionBarrier,
        proof: koru_delegate_core::FreshBotPolicyProof,
        scope: ScopeIdentity,
        authored_scope: koru_delegate_core::WorkspaceScope,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
        reply: mpsc::SyncSender<PortResult<ScheduleAdmission>>,
    },
    RoutineRelease {
        run: RunId,
        reply: mpsc::SyncSender<PortResult<()>>,
    },
    ScheduleFinish {
        run: RunId,
        claim: SlotClaim,
        scope: koru_delegate_scope::ResolvedScope,
        authored_scope: koru_delegate_core::WorkspaceScope,
        reply: mpsc::SyncSender<PortResult<()>>,
    },
    ScheduleAbort {
        run: RunId,
    },
    ScheduleReconcile {
        claim: SlotClaim,
        authored_scope: koru_delegate_core::WorkspaceScope,
        reply: mpsc::SyncSender<PortResult<()>>,
    },
    ScheduleStopped,
    Create {
        request: CreateRunRequest,
        reply: mpsc::SyncSender<Response>,
    },
    List {
        workspace: WorkspaceId,
        reply: mpsc::SyncSender<Response>,
    },
    Run {
        run: RunId,
        window: TimelineQuery,
        reply: mpsc::SyncSender<Response>,
    },
    Cancel {
        run: RunId,
        reply: mpsc::SyncSender<Response>,
    },
    Resume {
        run: RunId,
        /// §7.6's last row: a person's explicit request for a run whose
        /// prior attempt is unknown.
        confirm: bool,
        reply: mpsc::SyncSender<Response>,
    },
    Rate {
        run: RunId,
        request: RateRunRequest,
        reply: mpsc::SyncSender<Response>,
    },
    /// The worker's half of putting a run in the trash: refuse one that is
    /// still working, and remember what to do with its results.
    Trash {
        run: RunId,
        artifacts: ArtifactDisposition,
        reply: mpsc::SyncSender<Response>,
    },
    Restore {
        run: RunId,
        reply: mpsc::SyncSender<Response>,
    },
    /// The trash was emptied: everything device-local the run left behind,
    /// and its working folder when Koru made it.
    Purge {
        run: RunId,
        reply: mpsc::SyncSender<Response>,
    },
    Stream {
        run: RunId,
        /// See [`Watcher::workspace`].
        workspace: Option<WorkspaceId>,
        after: Option<u64>,
        sink: mpsc::SyncSender<RunStreamItem>,
    },
    /// One update from a run's session, pumped in.
    Update {
        run: RunId,
        epoch: uuid::Uuid,
        update: SessionUpdate,
    },
    /// A run's pump ended.
    PumpEnded {
        run: RunId,
        epoch: uuid::Uuid,
    },
}

mod bots;
mod child_resume;
mod decision_resume;
mod publication;
mod routine;
#[cfg(test)]
pub(crate) mod tests;

struct Watcher {
    next_index: u64,
    last_status: RunStatus,
    /// The workspace the caller named, for a run this device did not
    /// create — the only thing such a run can be found by here.
    workspace: Option<WorkspaceId>,
    sink: mpsc::SyncSender<RunStreamItem>,
}

struct Pump {
    stop: Arc<AtomicBool>,
    epoch: uuid::Uuid,
    /// Present only for a decision producer: its single delivery permit.
    permit: Option<mpsc::SyncSender<()>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeferredStartDisposition {
    Complete,
    Reserved,
    Retryable,
}

fn failed_created_start_disposition(
    retry_transient: bool,
    durable_status: Option<&RunStatus>,
) -> DeferredStartDisposition {
    let may_still_be_created = match durable_status {
        None | Some(RunStatus::Created) => true,
        Some(_) => false,
    };
    if retry_transient && may_still_be_created {
        DeferredStartDisposition::Retryable
    } else {
        DeferredStartDisposition::Complete
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScheduleLease {
    Ordinary(RunId),
    Routine(RunId),
}

pub struct Actor {
    orchestrator: Orchestrator,
    schedule_admission: Option<ScheduleLease>,
    claimed_schedule_retries: Vec<ScheduledScopeReservation>,
    deferred_restart_starts: VecDeque<RunId>,
    deferred_restart_retry_at: Option<Instant>,
    deferred_restart_retry_unhealthy: bool,
    scheduler: Option<crate::scheduler::Control>,
    deferred_starts: VecDeque<Command>,
    commands: mpsc::Sender<Command>,
    device: DeviceId,
    bot_queue_cursor: u64,
    pub(crate) legacy_bots: bool,
    bot_drain_pending: bool,
    bot_drain_again: bool,
    pumps: HashMap<RunId, Pump>,
    watchers: HashMap<RunId, Vec<Watcher>>,
    active: Arc<AtomicU32>,
    publication_worker: Option<crate::publication::PublicationWorker>,
    publication_ack: Option<koru_delegate_runtime::AppliedRunProjection>,
    /// The last Run a physical publication workflow was started for.
    publication_cursor: Option<RunId>,
    /// Runs whose last publication workflow failed, and the instant (one
    /// ordinary TICK later) before which they are not started again. Memory
    /// only; bounded by the publication candidates.
    publication_retry_at: HashMap<RunId, Instant>,
    /// The last Run offered to the decision resume driver (O123 rotation).
    decision_resume_cursor: Option<RunId>,
    /// Whether the last resume turn found an open decision wait (or none has
    /// run yet, or a Bot command may have opened one): keeps the maintenance
    /// tick alive so the next Run is offered.
    decision_resume_pending: bool,
    /// Every resume turn, as tests observe it.
    #[cfg(test)]
    resume_log: Vec<decision_resume::ResumeTurnRecord>,
    /// The last child wait offered to the child driver (M6 rotation).
    child_resume_cursor: Option<koru_delegate_sqlite::ChildWaitCursor>,
    /// Whether the last child turn found an open child wait (or none has run
    /// yet, or a Bot command may have opened one).
    child_resume_pending: bool,
    /// An ask was just admitted (`RequestChild`): the loop takes its next
    /// maintenance turn now instead of at the next tick, so the wait's
    /// driver begins (the enrollment, then Koru's stop of the asking
    /// session) without up to five seconds of delay. It only brings the
    /// turn forward; the driver takes the same steps under the same checks.
    child_wait_opened: bool,
    /// When the child clocks are next sampled. `None` while no child wait is
    /// open and no child Stop is owed.
    child_clock_at: Option<Instant>,
    /// Every child driver turn and clock tick, as tests observe them.
    #[cfg(test)]
    child_log: Vec<child_resume::ChildTurn>,
    #[cfg(test)]
    child_ticks: Vec<koru_delegate_runtime::ChildClockTick>,
}

impl Actor {
    pub fn new(
        orchestrator: Orchestrator,
        commands: mpsc::Sender<Command>,
        device: String,
    ) -> Self {
        let device =
            DeviceId::parse(&device).unwrap_or_else(|| DeviceId::from_uuid(uuid::Uuid::nil()));
        Self {
            orchestrator,
            schedule_admission: None,
            claimed_schedule_retries: Vec::new(),
            deferred_restart_starts: VecDeque::new(),
            deferred_restart_retry_at: None,
            deferred_restart_retry_unhealthy: false,
            scheduler: None,
            deferred_starts: VecDeque::new(),
            commands,
            device,
            bot_queue_cursor: 0,
            legacy_bots: !koru_bots_engine::ENABLED,
            bot_drain_pending: false,
            bot_drain_again: false,
            pumps: HashMap::new(),
            watchers: HashMap::new(),
            active: Arc::new(AtomicU32::new(0)),
            publication_worker: None,
            publication_ack: None,
            publication_cursor: None,
            publication_retry_at: HashMap::new(),
            decision_resume_cursor: None,
            // A restart may find waits a previous process left open.
            decision_resume_pending: true,
            #[cfg(test)]
            resume_log: Vec::new(),
            child_resume_cursor: None,
            // A restart may find child waits a previous process left open,
            // and their clocks are adopted on the first tick.
            child_resume_pending: true,
            child_wait_opened: false,
            child_clock_at: Some(Instant::now()),
            #[cfg(test)]
            child_log: Vec::new(),
            #[cfg(test)]
            child_ticks: Vec::new(),
        }
    }

    /// The count of runs with a live attempt, for the hello.
    pub fn active_runs(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.active)
    }

    pub fn attach_scheduler(&mut self, scheduler: crate::scheduler::Control) {
        self.scheduler = Some(scheduler);
    }

    fn may_finish_schedule(&self, run: RunId) -> bool {
        self.schedule_admission == Some(ScheduleLease::Ordinary(run))
            || (self.schedule_admission.is_none()
                && self
                    .claimed_schedule_retries
                    .iter()
                    .any(|reserved| reserved.run == run))
    }

    fn record_schedule_finish(
        &mut self,
        run: RunId,
        authored_scope: koru_delegate_core::WorkspaceScope,
        scope: ScopeIdentity,
        succeeded: bool,
    ) {
        self.schedule_admission = None;
        if succeeded {
            self.claimed_schedule_retries
                .retain(|reserved| reserved.run != run);
        } else if let Some(reserved) = self
            .claimed_schedule_retries
            .iter_mut()
            .find(|reserved| reserved.run == run)
        {
            reserved.scope = authored_scope;
            reserved.identity = Some(scope);
            reserved.resolution = ScopeReservationResolution::Exact;
        } else {
            self.claimed_schedule_retries
                .push(ScheduledScopeReservation {
                    run,
                    scope: authored_scope,
                    identity: Some(scope),
                    resolution: ScopeReservationResolution::Exact,
                });
        }
    }

    fn reservation_identity(&self, reserved: &ScheduledScopeReservation) -> Option<ScopeIdentity> {
        match reserved.resolution {
            ScopeReservationResolution::Exact => reserved.identity.clone(),
            ScopeReservationResolution::Refresh => self
                .orchestrator
                .plan_scope_identity(reserved.run, &reserved.scope)
                .ok(),
        }
    }

    #[cfg(test)]
    fn scope_is_reserved(&self, run: RunId, scope: &koru_delegate_core::WorkspaceScope) -> bool {
        if self.claimed_schedule_retries.is_empty() {
            return false;
        }
        let candidate_identity = self.orchestrator.plan_scope_identity(run, scope).ok();
        self.reservation_holder_for(run, scope, candidate_identity.as_ref())
            .is_some()
    }

    fn reservation_holder_for(
        &self,
        run: RunId,
        scope: &koru_delegate_core::WorkspaceScope,
        candidate_identity: Option<&ScopeIdentity>,
    ) -> Option<RunId> {
        self.claimed_schedule_retries.iter().find_map(|reserved| {
            (reserved.run == run
                || reservation_overlaps(
                    scope,
                    candidate_identity,
                    self.reservation_identity(reserved).as_ref(),
                    reserved,
                ))
            .then_some(reserved.run)
        })
    }

    fn prepare_person_scope(
        &self,
        run: RunId,
        scope: &koru_delegate_core::WorkspaceScope,
    ) -> Result<koru_delegate_scope::ResolvedScope, Failure> {
        let resolved = self.orchestrator.plan_scope(run, scope);
        if self
            .reservation_holder_for(
                run,
                scope,
                resolved.as_ref().ok().map(|resolved| &resolved.identity),
            )
            .is_some()
        {
            return Err(Failure::new(
                "schedule_scope_reserved",
                "A claimed scheduled run is waiting to start in this scope.",
                true,
            ));
        }
        resolved.map_err(|_| {
            Failure::new(
                "scope_unavailable",
                "The working scope is not available on this device.",
                true,
            )
        })
    }

    /// §7.6 on startup: complete, classify, reattach, start.
    pub fn restart(&mut self) {
        match self.orchestrator.restart() {
            Ok(mut report) => {
                tracing::info!(
                    target: "koru::delegate",
                    completed_forward = report.completed_forward.len(),
                    started = report.started.len(),
                    reattached = report.reattached.len(),
                    reconciled = report.reconciled.len(),
                    interrupted = report.interrupted.len(),
                    unresolved = report.unresolved.len(),
                    claims_kept = report.claims.kept.len(),
                    scheduled_scope_reservations = report.pending_scheduled_scopes.len(),
                    deferred_manual_starts = report.deferred_manual_starts.len(),
                    "restart classified the ledger"
                );
                // The one place that holds all six verdicts, so the
                // counters are read off the report rather than counted
                // again inside the classification (P1.17).
                koru_delegate_runtime::counters::restart(&report);
                self.reserve_restart_claims(&mut report.pending_scheduled_scopes);
                self.deferred_restart_starts
                    .extend(report.deferred_manual_starts.drain(..));
                for started in report.started.into_iter().chain(report.reattached) {
                    self.pump(started);
                }
                self.drain_deferred_restart_starts();
            }
            Err(error) => {
                tracing::error!(target: "koru::delegate", %error, "restart could not classify the ledger");
            }
        }
        if self.legacy_bots
            && let Err(error) = self
                .orchestrator
                .recover_unused_bot_reservations(self.device)
        {
            tracing::warn!(target: "koru::delegate", %error, "unused Bot reservations remain held");
        }
        self.count_active();
        self.wake_bots();
    }

    fn reserve_restart_claims(&mut self, claims: &mut Vec<ScheduledScopeReservation>) {
        self.claimed_schedule_retries.append(claims);
    }

    fn record_schedule_reconcile(
        &mut self,
        run: RunId,
        scope: koru_delegate_core::WorkspaceScope,
        identity: Option<ScopeIdentity>,
        succeeded: bool,
    ) {
        if succeeded {
            self.claimed_schedule_retries
                .retain(|reserved| reserved.run != run);
        } else if let Some(reserved) = self
            .claimed_schedule_retries
            .iter_mut()
            .find(|reserved| reserved.run == run)
        {
            reserved.scope = scope;
            reserved.identity = identity;
        } else {
            self.claimed_schedule_retries
                .push(ScheduledScopeReservation {
                    run,
                    scope,
                    identity,
                    resolution: ScopeReservationResolution::Refresh,
                });
        }
    }

    /// The loop, until a stop arrives.
    pub fn run(&mut self, inbox: mpsc::Receiver<Command>, stop: mpsc::Receiver<&'static str>) {
        let stop_commands = self.commands.clone();
        let _ = std::thread::Builder::new()
            .name("koru-delegate stop".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                if let Ok(why) = stop.recv() {
                    let _ = stop_commands.send(Command::Stop { why });
                }
            });
        let mut publication_at = self.publications_pending().then(Instant::now);
        let mut watched_at = (!self.watchers.is_empty()).then(|| Instant::now() + WATCH_POLL);
        let mut settle_at = self
            .orchestrator
            .has_unsettled()
            .then(|| Instant::now() + SETTLE_POLL);
        let mut bot_budget_at: Option<Instant> = None;
        let mut stopping = false;
        let mut stop_requested = false;
        let mut scheduler_stopped = false;
        while !stopping {
            let now = Instant::now();
            if self
                .deferred_restart_retry_at
                .is_some_and(|deadline| deadline <= now)
            {
                self.deferred_restart_retry_at = None;
                self.drain_deferred_restart_starts();
            }
            if watched_at.is_some_and(|deadline| deadline <= now) {
                self.absorb_watched();
                watched_at = (!self.watchers.is_empty()).then(|| Instant::now() + WATCH_POLL);
            }
            if settle_at.is_some_and(|deadline| deadline <= now) {
                let durable = self.orchestrator.absorb_acks();
                if durable && publication_at.is_none() && self.publications_pending() {
                    // The facts that waited for durability go now, not on
                    // the next slow tick.
                    publication_at = Some(Instant::now());
                }
                settle_at = (!durable).then(|| Instant::now() + SETTLE_POLL);
            }
            if publication_at.is_some_and(|deadline| deadline <= now) {
                // A step taken offers the next turn now rather than a tick later.
                let stepped = self.maintenance_tick();
                if stepped {
                    // A step may have handed a Run to the cleanup owner (a
                    // decision that expired, O157). Its cleanup is due now,
                    // not at whatever budget deadline was computed before.
                    bot_budget_at = None;
                }
                publication_at = if stepped {
                    Some(Instant::now())
                } else {
                    self.publications_pending().then(|| Instant::now() + TICK)
                };
            }

            if self.child_clock_at.is_some_and(|deadline| deadline <= now) {
                // The child clocks (M6): count, fold, time out, reach the
                // children. A request that timed out may free a slot or make
                // a parent resumable, so the other deadlines look again.
                self.child_clock_at = self.child_clock_tick();
                if publication_at.is_none() && self.publications_pending() {
                    publication_at = Some(Instant::now());
                }
                bot_budget_at = None;
            }
            if bot_budget_at.is_some_and(|deadline| deadline <= now) {
                let mut released = false;
                for run in self.orchestrator.bot_budget_runs() {
                    released |= self.orchestrator.bot_budget_tick(run).unwrap_or(false);
                }
                bot_budget_at = None;
                if released {
                    self.wake_bots();
                }
            }
            if bot_budget_at.is_none() {
                bot_budget_at = self
                    .orchestrator
                    .bot_budget_delay_ms()
                    .unwrap_or(Some(1000))
                    .map(|delay| Instant::now() + Duration::from_millis(delay));
            }
            let wait = [
                bot_budget_at,
                self.child_clock_at,
                publication_at,
                watched_at,
                settle_at,
                self.deferred_restart_retry_at,
            ]
            .into_iter()
            .flatten()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .min();
            let received = match wait {
                Some(wait) => inbox.recv_timeout(wait),
                None => inbox
                    .recv()
                    .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            };
            match received {
                Ok(Command::Stop { why }) => {
                    tracing::info!(target: "koru::delegate", why, "stopping");
                    stop_requested = true;
                    if let Some(scheduler) = &self.scheduler {
                        scheduler.stop();
                    } else if self.schedule_admission.is_none() {
                        stopping = true;
                    }
                }
                Ok(Command::ScheduleStopped) => {
                    scheduler_stopped = true;
                    // The scheduler is the only producer that can consume a
                    // live pre/post-claim admission. Its stopped ack proves no
                    // later Finish can arrive; a durable claim is recovered on
                    // the next process, so retaining the in-memory lease here
                    // would only deadlock shutdown.
                    self.schedule_admission = None;
                    if stop_requested && self.schedule_admission.is_none() {
                        stopping = true;
                    }
                }
                Ok(command) => {
                    self.handle(command);
                    bot_budget_at = None;
                    if self.child_clock_at.is_none() && self.child_resume_pending {
                        // A command may have opened a child wait.
                        self.child_clock_at = Some(Instant::now());
                    }
                    if !self.watchers.is_empty() && watched_at.is_none() {
                        watched_at = Some(Instant::now() + WATCH_POLL);
                    }
                    if publication_at.is_none() && self.publications_pending() {
                        publication_at = Some(Instant::now());
                    }
                    if self.take_child_wait_opened() {
                        // An ask was admitted: its driver's first step is
                        // due now, not at a tick already scheduled.
                        publication_at = Some(Instant::now());
                    }
                    if settle_at.is_none() && self.orchestrator.has_unsettled() {
                        settle_at = Some(Instant::now() + SETTLE_POLL);
                    }
                    if stop_requested && scheduler_stopped && self.schedule_admission.is_none() {
                        stopping = true;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        for pump in self.pumps.values() {
            pump.stop.store(true, Ordering::Release);
        }
        // A clean stop folds every running child clock first (M6).
        self.fold_child_clocks_for_stop();
        self.shutdown_publication();
        // What was sent is acknowledged before this process is gone.
        self.orchestrator.settle_all();
    }

    /// Whether an ask was admitted since the last look; reading it clears it.
    fn take_child_wait_opened(&mut self) -> bool {
        std::mem::take(&mut self.child_wait_opened)
    }

    /// One maintenance tick: the retained acknowledgment, one publication
    /// step, bus retries, then one Run's decision wait (O123 rotation).
    /// Returns whether that Run took a step.
    fn maintenance_tick(&mut self) -> bool {
        self.retry_publication_ack();
        self.drive_publication();
        if let Err(error) = self.orchestrator.retry_publications(64) {
            tracing::warn!(target: "koru::delegate", %error, "publications could not be retried");
        }
        let mut stepped = matches!(
            self.advance_decision_resume(),
            decision_resume::ResumeTurn::Stepped(..)
        );
        // One Run's child wait per turn, rotating (M6).
        stepped |= matches!(
            self.advance_child_resume(),
            child_resume::ChildTurn::Stepped(..)
        );
        self.sync_decision_authorities();
        self.sync_child_authorities();
        stepped
    }

    /// Republish every open decision wait this process tracks, from the
    /// ledger (O147), so a settled or closed wait lifts its fence and a
    /// restart seeds it. A failure keeps the previous, fenced value.
    pub(super) fn sync_decision_authorities(&mut self) {
        if let Err(error) = self.orchestrator.sync_decision_authorities() {
            tracing::warn!(target: "koru::delegate", %error, "decision wait classification not refreshed");
        }
    }

    /// The answer to a person's request, once the run's commits are
    /// durable — the one wait the actor still does, and it is the same wait
    /// a window's `StoreSession` does before it calls an edit saved.
    fn settled(&mut self, run: RunId, response: Response) -> Response {
        match self.orchestrator.settle(run) {
            Ok(()) => response,
            Err(error) => Response::Failure(failure_of(&error)),
        }
    }

    fn handle(&mut self, command: Command) {
        if self.refuse_ordinary_bot_route(&command) {
            return;
        }
        match command {
            Command::BotDrain => self.drain_one_bot(),
            Command::PublicationWake => self.drain_publication_completions(),
            Command::Bot { command, reply } => {
                let queue_changed = matches!(
                    command,
                    koru_delegate_api::bots::BotCommand::Submit { .. }
                        | koru_delegate_api::bots::BotCommand::Barrier { .. }
                        // A team value may have opened every queued task.
                        | koru_delegate_api::bots::BotCommand::TeamBarrier { .. }
                        | koru_delegate_api::bots::BotCommand::Stop { .. }
                        | koru_delegate_api::bots::BotCommand::Interrupt { .. }
                        // An admitted ask queued up to three child Runs.
                        | koru_delegate_api::bots::BotCommand::RequestChild { .. }
                );
                let response = match self.bot_command(command) {
                    Ok(reply) => Response::Bot(reply),
                    Err(failure) => Response::Failure(failure),
                };
                let _ = reply.send(response);
                // A decision command may have opened, answered or closed a
                // wait; its classification follows the ledger.
                self.sync_decision_authorities();
                self.decision_resume_pending = true;
                // The same for a child wait (M6).
                self.sync_child_authorities();
                self.child_resume_pending = true;
                if queue_changed {
                    self.wake_bots();
                }
            }
            Command::Stop { .. } | Command::ScheduleStopped => {}
            Command::RoutineBegin {
                prepared,
                admission,
                proof,
                scope,
                authored_scope,
                deadline,
                cancelled,
                reply,
            } => {
                self.begin_routine(
                    *prepared,
                    admission,
                    proof,
                    scope,
                    authored_scope,
                    deadline,
                    cancelled,
                    reply,
                );
            }
            Command::RoutineRelease { run, reply } => {
                let result = self.release_routine(run);
                let _ = reply.send(result);
            }
            Command::ScheduleBegin {
                run,
                scope,
                authored_scope,
                reply,
            } => {
                let result = if self.schedule_admission.is_some() {
                    Err(PortError::new(
                        koru_delegate_runtime::Port::Schedules,
                        "admission_live",
                        "another schedule admission is live",
                    ))
                } else if let Some(holder) =
                    self.reservation_holder_for(run, &authored_scope, Some(&scope))
                {
                    Ok(ScheduleAdmission::Busy(holder))
                } else if let Some(holder) = self
                    .orchestrator
                    .claims()
                    .holders_of(&scope)
                    .first()
                    .map(|holder| holder.run)
                {
                    Ok(ScheduleAdmission::Busy(holder))
                } else {
                    self.schedule_admission = Some(ScheduleLease::Ordinary(run));
                    Ok(ScheduleAdmission::Granted)
                };
                let _ = reply.send(result);
            }
            Command::ScheduleFinish {
                run,
                claim,
                scope,
                authored_scope,
                reply,
            } => {
                if matches!(self.schedule_admission, Some(ScheduleLease::Routine(_))) {
                    let _ = reply.send(Err(routine::refused("admission_phase")));
                    return;
                }
                let result = if self.may_finish_schedule(run) {
                    let identity = scope.identity.clone();
                    let result = ScheduleEffects::finish(
                        &mut self.orchestrator,
                        run,
                        claim,
                        scope,
                        authored_scope.clone(),
                    );
                    self.record_schedule_finish(run, authored_scope, identity, result.is_ok());
                    result
                } else {
                    Err(PortError::new(
                        koru_delegate_runtime::Port::Schedules,
                        "admission_stale",
                        "the schedule admission lease is not live",
                    ))
                };
                let result = match result {
                    Ok(Some(started)) => {
                        self.pump(started);
                        Ok(())
                    }
                    Ok(None) => Ok(()),
                    Err(error) => Err(error),
                };
                let _ = reply.send(result);
                self.drain_deferred_starts();
                self.drain_deferred_restart_starts();
                self.wake_bots();
            }
            Command::ScheduleAbort { run } => {
                if self.schedule_admission == Some(ScheduleLease::Ordinary(run)) {
                    self.schedule_admission = None;
                    self.drain_deferred_starts();
                    self.wake_bots();
                }
            }
            Command::ScheduleReconcile {
                claim,
                authored_scope,
                reply,
            } => {
                if matches!(self.schedule_admission, Some(ScheduleLease::Routine(_))) {
                    let _ = reply.send(Err(routine::refused("admission_phase")));
                    return;
                }
                let run = claim.run;
                let identity = self
                    .orchestrator
                    .plan_scope_identity(run, &authored_scope)
                    .ok();
                let result = ScheduleEffects::reconcile(
                    &mut self.orchestrator,
                    claim,
                    authored_scope.clone(),
                );
                self.record_schedule_reconcile(run, authored_scope, identity, result.is_ok());
                let result = match result {
                    Ok(Some(started)) => {
                        self.pump(started);
                        Ok(())
                    }
                    Ok(None) => Ok(()),
                    Err(error) => Err(error),
                };
                let _ = reply.send(result);
                self.drain_deferred_restart_starts();
                self.wake_bots();
            }
            Command::Create { request, reply } => {
                if self.schedule_admission.is_some() {
                    self.deferred_starts
                        .push_back(Command::Create { request, reply });
                    return;
                }
                let retry = DelegationRequest {
                    run: request.run,
                    requested_by: request.requested_by,
                    objective: request.objective.clone(),
                    agent: request.agent.clone(),
                    requested_model: request.requested_model.clone(),
                    scope: request.scope.clone(),
                    context: request.context.clone(),
                    grants: request.grants.clone(),
                    executing_device: self.device,
                    origin: koru_delegate_core::RunOrigin::Manual,
                };
                let preflight = match self.orchestrator.existing_create(&retry, request.workspace) {
                    Ok(preflight) => preflight,
                    Err(CreateError::Capture(failure)) => {
                        let _ = reply.send(Response::Failure(
                            Failure::new(
                                "context_capture",
                                "A context item could not be captured; the request was abandoned.",
                                false,
                            )
                            .with_detail("item", failure.item as u64)
                            .with_detail("stage", format!("{:?}", failure.stage).to_lowercase()),
                        ));
                        return;
                    }
                    Err(CreateError::Port(port)) => {
                        let _ = reply.send(Response::Failure(Failure::new(
                            port.code.clone(),
                            port.to_string(),
                            true,
                        )));
                        return;
                    }
                };
                let was_pending = match preflight {
                    CreateIntentStatus::Satisfied(created) => {
                        let status = self
                            .orchestrator
                            .header(created.run)
                            .map(|header| header.status)
                            .unwrap_or(RunStatus::Created);
                        let _ = reply.send(Response::Created(CreateRunResponse {
                            run: created.run,
                            document: created.document,
                            status: status.clone(),
                        }));
                        if matches!(status, RunStatus::Created) {
                            self.start_or_defer_created(created.run, request.scope);
                        }
                        return;
                    }
                    CreateIntentStatus::Absent => false,
                    CreateIntentStatus::Pending => true,
                };
                let resolved = if was_pending {
                    None
                } else {
                    match self.prepare_person_scope(request.run, &request.scope) {
                        Ok(resolved) => Some(resolved),
                        Err(failure) => {
                            let _ = reply.send(Response::Failure(failure));
                            return;
                        }
                    }
                };
                let run = request.run;
                // Scope/schedule refusal above stays cheap. Before an actual
                // create, also reject an imported Bot Run with no local ledger.
                match self.orchestrator.is_bot_run(run, Some(request.workspace)) {
                    Ok(true) => {
                        let _ = reply.send(Response::Failure(Failure::new(
                            "bot_private_route_required",
                            "Bot records require the private owner route.",
                            false,
                        )));
                        return;
                    }
                    Ok(false) | Err(RunError::UnknownRun) => {}
                    Err(error) => {
                        let _ = reply.send(Response::Failure(failure_of(&error)));
                        return;
                    }
                }
                let scope = request.scope.clone();
                let response = match self.create(request) {
                    Ok(created) => Response::Created(created),
                    Err(failure) => Response::Failure(failure),
                };
                let response = self.settled(run, response);
                let created = matches!(response, Response::Created(_));
                let _ = reply.send(response);
                if created {
                    // 202 went out on the durable request; the start follows.
                    if let Some(resolved) = resolved {
                        match self
                            .orchestrator
                            .start_resolved(run, resolved, StartOrigin::Manual)
                        {
                            Ok(started) => self.pump(started),
                            Err(error) => {
                                tracing::warn!(target: "koru::delegate", run = ?run, %error, "the run could not start");
                            }
                        }
                        self.notify(run);
                    } else {
                        self.start_or_defer_created(run, scope);
                    }
                }
            }
            Command::List { workspace, reply } => {
                let response = match self.orchestrator.list(workspace) {
                    Ok(listed) => Response::Runs(self.listing(listed)),
                    Err(error) => Response::Failure(failure_of(&error)),
                };
                let _ = reply.send(response);
            }
            Command::Run { run, window, reply } => {
                let response =
                    match self.view(run, window.workspace, window.from, window.bounded_len()) {
                        Ok(view) => Response::Run(view),
                        Err(failure) => Response::Failure(failure),
                    };
                let _ = reply.send(response);
            }
            Command::Cancel { run, reply } => {
                let response = match self.orchestrator.cancel(run) {
                    Ok(header) => Response::Cancelled(CancelRunResponse { header }),
                    Err(error) => Response::Failure(failure_of(&error)),
                };
                let response = self.settled(run, response);
                let _ = reply.send(response);
                self.notify(run);
            }
            Command::Resume {
                run,
                confirm,
                reply,
            } => {
                if self.schedule_admission.is_some() {
                    self.deferred_starts.push_back(Command::Resume {
                        run,
                        confirm,
                        reply,
                    });
                    return;
                }
                let scope = match self.orchestrator.resumable_scope(run, confirm) {
                    Ok(scope) => scope,
                    Err(error) => {
                        let _ = reply.send(Response::Failure(failure_of(&error)));
                        return;
                    }
                };
                let resolved = match self.prepare_person_scope(run, &scope) {
                    Ok(resolved) => resolved,
                    Err(failure) => {
                        let _ = reply.send(Response::Failure(failure));
                        return;
                    }
                };
                let response = match self.orchestrator.resume_resolved(run, confirm, resolved) {
                    Ok(started) => {
                        let header = self.orchestrator.header(run);
                        self.pump(started);
                        match header {
                            Ok(header) => Response::Resumed(ResumeRunResponse { header }),
                            Err(error) => Response::Failure(failure_of(&error)),
                        }
                    }
                    Err(error) => Response::Failure(failure_of(&error)),
                };
                let response = self.settled(run, response);
                let _ = reply.send(response);
                self.notify(run);
            }
            Command::Rate {
                run,
                request,
                reply,
            } => {
                let response = match self.orchestrator.rate(
                    run,
                    request.rating,
                    request.note,
                    request.rated_by,
                ) {
                    Ok(header) => Response::Rated(RateRunResponse { header }),
                    Err(error) => Response::Failure(failure_of(&error)),
                };
                let response = self.settled(run, response);
                let _ = reply.send(response);
                self.notify(run);
            }
            Command::Trash {
                run,
                artifacts,
                reply,
            } => {
                let response = match self.orchestrator.trash(run, artifacts) {
                    Ok(place) => Response::Trashed(TrashRunResponse {
                        header: place.header,
                        document: place.document,
                    }),
                    Err(error) => Response::Failure(failure_of(&error)),
                };
                let response = self.settled(run, response);
                let _ = reply.send(response);
            }
            Command::Restore { run, reply } => {
                let response = match self.orchestrator.restore(run) {
                    Ok(place) => Response::Restored(RestoreRunResponse {
                        header: place.header,
                        document: place.document,
                    }),
                    Err(error) => Response::Failure(failure_of(&error)),
                };
                let response = self.settled(run, response);
                let _ = reply.send(response);
            }
            Command::Purge { run, reply } => {
                // The link goes with the state; nothing may still be in flight.
                if let Err(error) = self.orchestrator.settle(run) {
                    let _ = reply.send(Response::Failure(failure_of(&error)));
                    return;
                }
                let response = match self.orchestrator.purge(run) {
                    Ok(purged) => Response::Purged(PurgeRunResponse {
                        run: purged.run,
                        document: purged.document,
                        folder: purged.folder,
                        artifacts_retired: purged.artifacts_retired,
                    }),
                    Err(error) => Response::Failure(failure_of(&error)),
                };
                let _ = reply.send(response);
                // Nothing is watching a run that no longer exists.
                self.watchers.remove(&run);
            }
            Command::Stream {
                run,
                workspace,
                after,
                sink,
            } => {
                if self.orchestrator.view(run, workspace, 0, 0).is_err() {
                    let _ = sink.try_send(RunStreamItem::Closed);
                    return;
                }
                self.watchers.entry(run).or_default().push(Watcher {
                    next_index: after.map_or(0, |after| after.saturating_add(1)),
                    last_status: RunStatus::Created,
                    workspace,
                    sink,
                });
                self.notify(run);
            }
            Command::Update { run, epoch, update } => {
                // Already queued old-pump updates cannot enter a replacement
                // decision producer or mutate its retained source frontier.
                if self.pumps.get(&run).is_none_or(|pump| pump.epoch != epoch) {
                    return;
                }
                if self
                    .pumps
                    .get(&run)
                    .is_some_and(|pump| pump.permit.is_some())
                {
                    let was_active = self.orchestrator.bot_budget_runs().contains(&run);
                    self.decision_update(run, epoch, update);
                    self.notify(run);
                    if was_active && !self.orchestrator.bot_budget_runs().contains(&run) {
                        self.wake_bots();
                    }
                    return;
                }
                let was_active = self.orchestrator.bot_budget_runs().contains(&run);
                match self.orchestrator.handle(run, update) {
                    Ok(HandleOutcome::Continue) => {}
                    Ok(HandleOutcome::Ended) => self.stop_pump(run),
                    Err(error) => {
                        // A Bot Run's interruption is absorbing: the person's
                        // later turns, gaps and handback on the borrowed session
                        // are expected refusals there, not news.
                        let absorbing = self.orchestrator.header(run).is_ok_and(|header| {
                            header.bot.is_some()
                                && matches!(header.status, RunStatus::Interrupted { .. })
                        });
                        if absorbing {
                            tracing::debug!(target: "koru::delegate", run = ?run, %error, "an update to an interrupted Bot Run was refused");
                        } else {
                            tracing::warn!(target: "koru::delegate", run = ?run, %error, "an update was refused");
                        }
                    }
                }
                self.notify(run);
                if was_active && !self.orchestrator.bot_budget_runs().contains(&run) {
                    self.wake_bots();
                }
            }
            Command::PumpEnded { run, epoch } => {
                if self.pumps.get(&run).is_none_or(|pump| pump.epoch != epoch) {
                    return;
                }
                self.pumps.remove(&run);
                self.count_active();
                self.wake_bots();
            }
        }
    }

    fn publications_pending(&self) -> bool {
        // Exclusive Runs keep the existing coalesced maintenance tick alive so
        // owed projections, observations and producer reconnects progress.
        if self.publication_ack.is_some()
            || self.decision_resume_pending
            || self.child_resume_pending
            || !self.orchestrator.publication_candidates().is_empty()
        {
            return true;
        }
        match self
            .orchestrator
            .ports()
            .store
            .pending_publication_run_anchor()
        {
            Ok(anchor) => anchor.is_some(),
            Err(error) => {
                tracing::warn!(target: "koru::delegate", %error, "publication debt could not be observed");
                true
            }
        }
    }

    fn drain_deferred_starts(&mut self) {
        let mut remaining = self.deferred_starts.len();
        while self.schedule_admission.is_none() && remaining != 0 {
            let Some(command) = self.deferred_starts.pop_front() else {
                break;
            };
            remaining -= 1;
            self.handle(command);
        }
    }

    fn defer_created_start(&mut self, run: RunId) {
        if !self.deferred_restart_starts.contains(&run) {
            self.deferred_restart_starts.push_back(run);
        }
    }

    fn mark_deferred_start_complete(&mut self, run: RunId) {
        self.deferred_restart_starts
            .retain(|deferred| *deferred != run);
        if self.deferred_restart_starts.is_empty() {
            self.deferred_restart_retry_at = None;
            if self.deferred_restart_retry_unhealthy {
                tracing::info!(target: "koru::delegate", "deferred restart starts recovered");
                self.deferred_restart_retry_unhealthy = false;
            }
        }
    }

    fn start_or_defer_created(&mut self, run: RunId, scope: koru_delegate_core::WorkspaceScope) {
        if self.attempt_created_start(run, scope, false) == DeferredStartDisposition::Reserved {
            self.defer_created_start(run);
        }
    }

    fn attempt_created_start(
        &mut self,
        run: RunId,
        scope: koru_delegate_core::WorkspaceScope,
        retry_transient: bool,
    ) -> DeferredStartDisposition {
        let resolved = match self.prepare_person_scope(run, &scope) {
            Ok(resolved) => resolved,
            Err(failure) if failure.code == "schedule_scope_reserved" => {
                return DeferredStartDisposition::Reserved;
            }
            Err(_) => {
                // This is an already-durable manual run, so a missing scope
                // must become the same visible ScopeAdmission failure as an
                // ordinary post-202 start. The deferred queue is only for a
                // claimed schedule reservation, not a general retry worker.
                let result = self.orchestrator.start(run, StartOrigin::Manual);
                match result {
                    Ok(started) => {
                        self.mark_deferred_start_complete(run);
                        self.pump(started);
                        self.notify(run);
                        return DeferredStartDisposition::Complete;
                    }
                    Err(error) => {
                        let status = self
                            .orchestrator
                            .header(run)
                            .ok()
                            .map(|header| header.status);
                        let disposition =
                            failed_created_start_disposition(retry_transient, status.as_ref());
                        if disposition == DeferredStartDisposition::Complete {
                            tracing::warn!(target: "koru::delegate", run = ?run, %error, "a durable Created run could not record its scope refusal");
                            self.notify(run);
                        }
                        return disposition;
                    }
                }
            }
        };
        match self
            .orchestrator
            .start_resolved(run, resolved, StartOrigin::Manual)
        {
            Ok(started) => {
                self.mark_deferred_start_complete(run);
                self.pump(started);
                self.notify(run);
                DeferredStartDisposition::Complete
            }
            Err(error) => {
                let status = self
                    .orchestrator
                    .header(run)
                    .ok()
                    .map(|header| header.status);
                let disposition =
                    failed_created_start_disposition(retry_transient, status.as_ref());
                if disposition == DeferredStartDisposition::Complete {
                    tracing::warn!(target: "koru::delegate", run = ?run, %error, "a Created run could not start");
                    self.notify(run);
                }
                disposition
            }
        }
    }

    fn attempt_deferred_restart_start(&mut self, run: RunId) -> DeferredStartDisposition {
        let scope = match self.orchestrator.header(run) {
            Ok(header) if matches!(header.status, RunStatus::Created) => header.scope,
            Ok(_) => return DeferredStartDisposition::Complete,
            Err(_) => return DeferredStartDisposition::Retryable,
        };
        self.attempt_created_start(run, scope, true)
    }

    /// Restart found these manual runs already durably Created, but a claimed
    /// schedule had to retain scope precedence. Retry each once whenever
    /// reservations change. A transient pre-attempt failure arms a bounded
    /// retry deadline; reservation-only waits remain event-driven.
    fn drain_deferred_restart_starts(&mut self) {
        self.drain_deferred_restart_starts_with(Self::attempt_deferred_restart_start);
    }

    fn drain_deferred_restart_starts_with(
        &mut self,
        mut attempt: impl FnMut(&mut Self, RunId) -> DeferredStartDisposition,
    ) {
        let mut remaining = self.deferred_restart_starts.len();
        let mut retryable = false;
        while remaining != 0 {
            let Some(run) = self.deferred_restart_starts.pop_front() else {
                break;
            };
            remaining -= 1;
            match attempt(self, run) {
                DeferredStartDisposition::Complete => {}
                DeferredStartDisposition::Reserved => self.defer_created_start(run),
                DeferredStartDisposition::Retryable => {
                    self.deferred_restart_starts.push_back(run);
                    retryable = true;
                }
            }
        }
        if self.deferred_restart_starts.is_empty() {
            self.deferred_restart_retry_at = None;
            if self.deferred_restart_retry_unhealthy {
                tracing::info!(target: "koru::delegate", "deferred restart starts recovered");
                self.deferred_restart_retry_unhealthy = false;
            }
        } else if retryable {
            if !self.deferred_restart_retry_unhealthy {
                tracing::warn!(target: "koru::delegate", "deferred restart starts will retry after a transient failure");
                self.deferred_restart_retry_unhealthy = true;
            }
            self.deferred_restart_retry_at = Some(Instant::now() + DEFERRED_START_RETRY);
        } else {
            self.deferred_restart_retry_at = None;
            if self.deferred_restart_retry_unhealthy {
                tracing::info!(target: "koru::delegate", "deferred restart start transport recovered");
                self.deferred_restart_retry_unhealthy = false;
            }
        }
    }

    fn create(&mut self, request: CreateRunRequest) -> Result<CreateRunResponse, Failure> {
        let workspace = request.workspace;
        let delegation = DelegationRequest {
            run: request.run,
            requested_by: request.requested_by,
            objective: request.objective,
            agent: request.agent,
            requested_model: request.requested_model,
            scope: request.scope,
            context: request.context,
            grants: request.grants,
            executing_device: self.device,
            origin: koru_delegate_core::RunOrigin::Manual,
        };
        let created =
            self.orchestrator
                .create(delegation, workspace)
                .map_err(|error| match error {
                    CreateError::Capture(failure) => Failure::new(
                        "context_capture",
                        "A context item could not be captured; the request was abandoned.",
                        false,
                    )
                    .with_detail("item", failure.item as u64)
                    .with_detail("stage", format!("{:?}", failure.stage).to_lowercase()),
                    CreateError::Port(port) => {
                        Failure::new(port.code.clone(), port.to_string(), true)
                    }
                })?;
        let status = self
            .orchestrator
            .header(created.run)
            .map(|header| header.status)
            .unwrap_or(RunStatus::Created);
        Ok(CreateRunResponse {
            run: created.run,
            document: created.document,
            status,
        })
    }

    fn view(
        &mut self,
        run: RunId,
        workspace: Option<WorkspaceId>,
        from: u64,
        len: usize,
    ) -> Result<RunView, Failure> {
        let view = self
            .orchestrator
            .view(run, workspace, from, len)
            .map_err(|error| failure_of(&error))?;
        Ok(RunView {
            document: view.document,
            header: view.header,
            execution: RunExecution {
                device: self.device,
                session: view.session,
                run_workspace: view
                    .run_workspace
                    .as_ref()
                    .map(|path| path.display().to_string()),
            },
            context: view.context,
            timeline: TimelineWindow {
                total: view.timeline.total,
                from: view.timeline.from,
                items: view.timeline.items.into_iter().map(wire_item).collect(),
            },
        })
    }

    /// Ask the Store what it has sent for every watched run this device is
    /// not driving, and push what arrived. Nothing to do when nobody is
    /// watching, which is the ordinary case.
    fn absorb_watched(&mut self) {
        let watched: Vec<RunId> = self.watchers.keys().copied().collect();
        for run in watched {
            match self.orchestrator.absorb(run) {
                Ok(true) => self.notify(run),
                Ok(false) => {}
                Err(error) => {
                    tracing::debug!(target: "koru::delegate", run = ?run, %error, "a watched run could not be refreshed");
                }
            }
        }
    }

    /// Pump a started attempt's stream into this actor on its own thread.
    fn pump(&mut self, started: Started) {
        let run = started.run;
        let Some(mut events) = started.events else {
            self.count_active();
            return;
        };
        self.stop_pump(run);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let epoch = uuid::Uuid::new_v4();
        let commands = self.commands.clone();
        let spawned = std::thread::Builder::new()
            .name("koru-delegate pump".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                while !flag.load(Ordering::Acquire) {
                    let Some(update) = events.next(PUMP_POLL) else {
                        continue;
                    };
                    let closed = matches!(update, SessionUpdate::Closed { .. });
                    if commands
                        .send(Command::Update { run, epoch, update })
                        .is_err()
                        || closed
                    {
                        break;
                    }
                }
                let _ = commands.send(Command::PumpEnded { run, epoch });
            });
        match spawned {
            Ok(_) => {
                self.pumps.insert(
                    run,
                    Pump {
                        stop,
                        epoch,
                        permit: None,
                    },
                );
            }
            Err(error) => {
                tracing::error!(target: "koru::delegate", run = ?run, %error, "the run's stream could not be pumped");
            }
        }
        self.count_active();
    }

    fn stop_pump(&mut self, run: RunId) {
        if let Some(pump) = self.pumps.remove(&run) {
            pump.stop.store(true, Ordering::Release);
        }
        self.count_active();
    }

    fn count_active(&self) {
        let active = self
            .orchestrator
            .open_runs()
            .filter(|run| self.orchestrator.is_active(*run))
            .count();
        self.active
            .store(u32::try_from(active).unwrap_or(u32::MAX), Ordering::Release);
    }

    /// **One listing, two halves.** The workspace's runs as the manifest
    /// holds them, split into the ones a person is looking at and the ones
    /// in the trash — a trashed run is not a run you are looking at any
    /// more, and the Agents navigator stops listing it the moment it goes.
    ///
    /// The session each live run is being driven in here is read off the
    /// attempts this worker already holds: no document is opened for it, and
    /// a run this device is not driving simply has no entry. The document
    /// each run is registered as travels too, because trashing a run is a
    /// manifest edit and the manifest belongs to the window.
    fn listing(&mut self, listed: Vec<koru_delegate_runtime::ListedRun>) -> ListRunsResponse {
        let mut runs = Vec::new();
        let mut trashed = Vec::new();
        let mut documents = std::collections::BTreeMap::new();
        let mut sessions = std::collections::BTreeMap::new();
        for entry in listed {
            if entry.header.bot.is_some() {
                continue;
            }
            documents.insert(entry.header.run, entry.document);
            match entry.trashed_at_ms {
                Some(trashed_at_ms) => trashed.push(TrashedRun {
                    artifacts: self
                        .orchestrator
                        .deletion_recorded(entry.header.run)
                        .unwrap_or_default(),
                    header: entry.header,
                    document: entry.document,
                    trashed_at_ms,
                }),
                None => {
                    if let Some(session) = self.orchestrator.driving(entry.header.run) {
                        sessions.insert(entry.header.run, session);
                    }
                    runs.push(entry.header);
                }
            }
        }
        // Newest deletion first: the Trash tab's own order, and the one a
        // person undoing something they just did needs.
        trashed.sort_by(|a, b| match (a.trashed_at_ms, b.trashed_at_ms) {
            (Some(x), Some(y)) => y.cmp(&x),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        ListRunsResponse {
            runs,
            sessions,
            documents,
            trashed,
        }
    }

    /// Push what changed to every watcher of `run`; drop the ones whose
    /// caller went away, and close the ones whose run ended.
    fn notify(&mut self, run: RunId) {
        let Some(mut watchers) = self.watchers.remove(&run) else {
            return;
        };
        let header = match self.orchestrator.header(run) {
            Ok(header) => header,
            Err(error) if error.publication_pending() => {
                // Temporary ownership transfer is not a closed Run. Keep each
                // watcher's exact delivery cursor for the returned handle.
                self.watchers.insert(run, watchers);
                return;
            }
            Err(_) => {
                for watcher in watchers {
                    let _ = watcher.sink.try_send(RunStreamItem::Closed);
                }
                return;
            }
        };
        let mut kept = Vec::with_capacity(watchers.len());
        for mut watcher in watchers.drain(..) {
            let mut alive = true;
            loop {
                let Ok(view) =
                    self.orchestrator
                        .view(run, watcher.workspace, watcher.next_index, PAGE)
                else {
                    alive = false;
                    break;
                };
                if view.timeline.items.is_empty() {
                    break;
                }
                let count = view.timeline.items.len();
                for (offset, item) in view.timeline.items.into_iter().enumerate() {
                    let index = view.timeline.from + offset as u64;
                    match watcher.sink.try_send(RunStreamItem::Entry {
                        index,
                        item: wire_item(item),
                    }) {
                        Ok(()) => watcher.next_index = index + 1,
                        Err(mpsc::TrySendError::Full(_)) => break,
                        Err(mpsc::TrySendError::Disconnected(_)) => {
                            alive = false;
                            break;
                        }
                    }
                }
                if !alive || count < PAGE || watcher.next_index < view.timeline.from + count as u64
                {
                    break;
                }
            }
            if alive && watcher.last_status != header.status {
                match watcher.sink.try_send(RunStreamItem::Status {
                    status: header.status.clone(),
                }) {
                    Ok(()) => watcher.last_status = header.status.clone(),
                    Err(mpsc::TrySendError::Full(_)) => {}
                    Err(mpsc::TrySendError::Disconnected(_)) => alive = false,
                }
            }
            if alive && header.status.is_terminal() && watcher.last_status == header.status {
                let _ = watcher.sink.try_send(RunStreamItem::Closed);
                alive = false;
            }
            if alive {
                kept.push(watcher);
            }
        }
        if !kept.is_empty() {
            self.watchers.insert(run, kept);
        }
    }
}

fn reservation_overlaps(
    candidate_scope: &koru_delegate_core::WorkspaceScope,
    candidate_identity: Option<&ScopeIdentity>,
    held_identity: Option<&ScopeIdentity>,
    reserved: &ScheduledScopeReservation,
) -> bool {
    match (candidate_identity, held_identity) {
        // Project ids are names, not directory identity: two projects may
        // resolve to one folder or nested folders.
        (Some(candidate), Some(held)) => candidate.overlap(held).is_overlap(),
        // If either side cannot resolve, conservatism stays bounded to the
        // same authored project rather than blocking unrelated scopes.
        _ => {
            candidate_scope.project().is_some()
                && candidate_scope.project() == reserved.scope.project()
        }
    }
}

fn wire_item(item: WindowItem) -> TimelineItem {
    match item {
        WindowItem::Step(entry) => TimelineItem::Step(entry),
        WindowItem::Usage(fact) => TimelineItem::Usage(fact),
        WindowItem::Unrecognized => TimelineItem::Unrecognized,
    }
}

pub fn failure_of(error: &RunError) -> Failure {
    if error.publication_pending() {
        return Failure::new(
            "publication_pending",
            "The Run publication is still in progress.",
            true,
        );
    }
    if let RunError::Port(port) = error {
        if matches!(
            port.code.as_str(),
            "bot_retained_state" | "bot_retention_inconsistent" | "bot_retention_unavailable"
        ) {
            return Failure::new(
                port.code.clone(),
                port.detail.clone(),
                port.code == "bot_retention_unavailable",
            );
        }
    }
    let retryable = matches!(error, RunError::Port(_));
    let message = match error {
        RunError::UnknownRun => "No run by that id exists on this device.".to_owned(),
        RunError::WrongStatus { status } => format!("The run is {status}."),
        RunError::NotDelegatable => {
            "The session host did not negotiate the delegation family.".to_owned()
        }
        RunError::NotInterrupted { status } => {
            format!("Only an interrupted run resumes; the run is {status}.")
        }
        RunError::AttemptLive => "The attempt's session is still live.".to_owned(),
        RunError::PriorAttemptUnknown => {
            "Nothing proved the earlier attempt stopped. Resume it deliberately with \
             `confirm`, knowing earlier work may already exist."
                .to_owned()
        }
        RunError::SourceRefused(refusal) => format!(
            "A source this run was given no longer resolves for anything new (item {}).",
            refusal.item
        ),
        // The kind, never the path: the folder is a person's own, and the
        // one place its spelling is allowed is the screen that shows the
        // run. A person sees "something is still using it"; a log sees the
        // code.
        RunError::FolderNotRemoved { kind } => format!(
            "The run's working folder could not be removed ({kind}). Nothing was deleted; the run \
             is still in the trash."
        ),
        RunError::Illegal(illegal) => illegal.to_string(),
        RunError::Port(port) => format!("The {} port refused: {}.", port.port.name(), port.code),
    };
    Failure::new(error.code(), message, retryable)
}
