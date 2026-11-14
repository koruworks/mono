// SPDX-License-Identifier: MPL-2.0

//! Dedicated schedule evaluator and its narrow rendezvous with the Run actor.

use std::{
    net::{Shutdown, TcpStream},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use koru_delegate_api::wire::{
    CreateScheduleRequest, CreateScheduleResponse, EditScheduleRequest, EditScheduleResponse,
    Failure, ListSchedulesResponse, PauseScheduleRequest, PauseScheduleResponse,
    RenameScheduleRequest, RenameScheduleResponse, ResumeScheduleRequest, ResumeScheduleResponse,
    RetireScheduleRequest, RetireScheduleResponse, ScheduleChange, ScheduleFiringView,
    ScheduleNextFiring, ScheduleView,
};
use koru_delegate_core::RunId;
use koru_delegate_runtime::{
    Port, PortError, PortResult, ScheduleAdmission, ScheduleChangeWatch, ScheduleCoordinator,
    ScheduleEffects, ScheduleEvents, ScheduleListing, ScheduleNext, ScheduleWake,
    ScheduleWatchTargets, Started,
};
use koru_delegate_scope::ScopeIdentity;
use koru_delegate_sqlite::SlotClaim;

use crate::{actor::Command as ActorCommand, schedule_events::ScheduleEventAuthorityGate};

const REPAIR_BACKOFF: Duration = Duration::from_secs(1);
const SCHEDULER_RECONCILE_BATCH: usize = 1;
const EVENT_PUBLISHER_STOP_BOUND: Duration = Duration::from_secs(8);
mod routine;

#[derive(Default)]
struct LoopState {
    deadline_ms: Option<i64>,
    repair_at: Option<Instant>,
    #[cfg(test)]
    evaluations: u64,
}

impl LoopState {
    fn take_evaluation(&mut self, woke: bool, wall_now_ms: i64, now: Instant) -> bool {
        let due = evaluation_due(woke, self.deadline_ms, wall_now_ms, self.repair_at, now);
        #[cfg(test)]
        if due {
            self.evaluations = self.evaluations.saturating_add(1);
        }
        due
    }

    fn next_wait(&self, wall_now_ms: i64, now: Instant) -> Option<Duration> {
        next_wait(self.deadline_ms, wall_now_ms, self.repair_at, now)
    }
}

#[allow(clippy::large_enum_variant)] // one bounded channel owns requests; indirection buys nothing
enum Command {
    Wake,
    Create {
        request: CreateScheduleRequest,
        reply: mpsc::SyncSender<Result<CreateScheduleResponse, Failure>>,
    },
    Rename {
        request: RenameScheduleRequest,
        reply: mpsc::SyncSender<Result<RenameScheduleResponse, Failure>>,
    },
    Edit {
        request: EditScheduleRequest,
        reply: mpsc::SyncSender<Result<EditScheduleResponse, Failure>>,
    },
    Pause {
        request: PauseScheduleRequest,
        reply: mpsc::SyncSender<Result<PauseScheduleResponse, Failure>>,
    },
    Resume {
        request: ResumeScheduleRequest,
        reply: mpsc::SyncSender<Result<ResumeScheduleResponse, Failure>>,
    },
    Retire {
        request: RetireScheduleRequest,
        reply: mpsc::SyncSender<Result<RetireScheduleResponse, Failure>>,
    },
    List {
        workspace: koru_delegate_core::WorkspaceId,
        reply: mpsc::SyncSender<Result<ListSchedulesResponse, Failure>>,
    },
    Watch {
        workspace: koru_delegate_core::WorkspaceId,
        after: Option<ScheduleChange>,
        sink: mpsc::SyncSender<ScheduleWatchEvent>,
        reply: mpsc::SyncSender<Result<u64, Failure>>,
    },
    CancelWatch(u64),
    #[cfg(feature = "test-support")]
    FixtureWake(mpsc::SyncSender<Result<(), String>>),
    Stop,
}

enum PublisherCommand {
    Wake,
    Stop,
}

/// Journal delivery owns its own worker. Schedule API replies and person
/// commands therefore never sit behind a gateway send or a recovered outbox
/// backlog; the only scheduler-side repair is one bounded Store comparison.
struct EventPublisher {
    commands: mpsc::Sender<PublisherCommand>,
    wake: ScheduleWake,
    stopped: mpsc::Receiver<()>,
    stop_bound: Duration,
    thread: Option<JoinHandle<()>>,
}

impl EventPublisher {
    fn spawn(events: Box<dyn ScheduleEvents>) -> Result<Self, String> {
        Self::spawn_with_stop_bound(events, EVENT_PUBLISHER_STOP_BOUND)
    }

    fn spawn_with_stop_bound(
        events: Box<dyn ScheduleEvents>,
        stop_bound: Duration,
    ) -> Result<Self, String> {
        let (commands, inbox) = mpsc::channel();
        let (stopped, wait_stopped) = mpsc::sync_channel(1);
        let wake_commands = commands.clone();
        let wake = ScheduleWake::new(move || {
            let _ = wake_commands.send(PublisherCommand::Wake);
        });
        let thread_wake = wake.clone();
        let thread = std::thread::Builder::new()
            .name("delegate-schedule-events".to_owned())
            .spawn(move || {
                run_event_publisher(events, thread_wake, inbox);
                let _ = stopped.send(());
            })
            .map_err(|error| format!("could not start schedule event publisher: {error}"))?;
        Ok(Self {
            commands,
            wake,
            stopped: wait_stopped,
            stop_bound,
            thread: Some(thread),
        })
    }

    fn wake(&self) -> ScheduleWake {
        self.wake.clone()
    }

    fn stop_bound(&self) -> Duration {
        self.stop_bound
    }

    fn request_stop(&self) {
        let _ = self.commands.send(PublisherCommand::Stop);
    }

    fn stop(&mut self) {
        self.stop_by(Instant::now() + self.stop_bound);
    }

    fn stop_by(&mut self, deadline: Instant) {
        self.request_stop();
        if let Some(thread) = self.thread.take() {
            if self
                .stopped
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .is_ok()
            {
                let _ = thread.join();
            } else {
                // HTTP requests on the shipping adapter carry a two-second
                // deadline. This outer bound is the final defence if a
                // transport or injected implementation fails to honor it:
                // dropping the handle detaches the already-stopping worker so
                // the delegate process can exit before its supervisor kills it.
                tracing::warn!(
                    target: "koru::delegate",
                    "schedule event publisher exceeded its shutdown bound"
                );
            }
        }
    }
}

impl Drop for EventPublisher {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct Handle {
    stopped: Arc<AtomicBool>,
    commands: mpsc::Sender<Command>,
    wake: ScheduleWake,
    catalog: Arc<CatalogControl>,
    catalog_thread: Option<JoinHandle<()>>,
    authority_gate: Arc<ScheduleEventAuthorityGate>,
    publisher: Option<EventPublisher>,
    thread: Option<JoinHandle<()>>,
}

impl Handle {
    pub fn spawn(
        coordinator: ScheduleCoordinator,
        client: koru_api_client::Client,
        actor: mpsc::Sender<ActorCommand>,
        publisher_events: Box<dyn ScheduleEvents>,
        authority_gate: Arc<ScheduleEventAuthorityGate>,
    ) -> Result<Self, String> {
        let (commands, inbox) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let wake_commands = commands.clone();
        let wake = ScheduleWake::new(move || {
            let _ = wake_commands.send(Command::Wake);
        });
        let catalog = Arc::new(CatalogControl::new());
        let catalog_watch = koru_store_client::CatalogWatch::open(&client.store().discovery())
            .map_err(|error| format!("could not open Store catalog watch: {error}"))?;
        let catalog_abort = catalog_watch.abort_handle().ok_or_else(|| {
            "Store catalog watch has no interruptible TCP abort handle".to_owned()
        })?;
        let _ = catalog.install(Some(catalog_abort));
        let catalog_thread =
            spawn_catalog_watch(client, wake.clone(), Arc::clone(&catalog), catalog_watch)?;
        let mut publisher = match EventPublisher::spawn(publisher_events) {
            Ok(publisher) => publisher,
            Err(error) => {
                catalog.stop();
                let _ = catalog_thread.join();
                return Err(error);
            }
        };
        let publisher_wake = publisher.wake();
        let thread_wake = wake.clone();
        let thread_authority_gate = Arc::clone(&authority_gate);
        let thread_stopped = Arc::clone(&stopped);
        let thread = match std::thread::Builder::new()
            .name("delegate-scheduler".to_owned())
            .spawn(move || {
                run(
                    coordinator,
                    actor,
                    thread_authority_gate,
                    thread_wake,
                    publisher_wake,
                    inbox,
                    thread_stopped,
                )
            }) {
            Ok(thread) => thread,
            Err(error) => {
                catalog.stop();
                let _ = catalog_thread.join();
                publisher.stop();
                return Err(format!("could not start schedule evaluator: {error}"));
            }
        };
        Ok(Self {
            stopped,
            commands,
            wake,
            catalog,
            catalog_thread: Some(catalog_thread),
            authority_gate,
            publisher: Some(publisher),
            thread: Some(thread),
        })
    }

    pub fn control(&self) -> Control {
        Control {
            stopped: Arc::clone(&self.stopped),
            commands: self.commands.clone(),
            wake: self.wake.clone(),
        }
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stopped.store(true, Ordering::Release);
        let publisher_deadline = Instant::now()
            + self
                .publisher
                .as_ref()
                .map_or(EVENT_PUBLISHER_STOP_BOUND, EventPublisher::stop_bound);
        let _ = self.commands.send(Command::Stop);
        // A publisher may be inside a bounded Store comparison while holding
        // the live-authority gate. Closing it wakes a scheduler waiting to
        // enter; that scheduler exits instead of making shutdown depend on the
        // publisher returning first.
        self.authority_gate.stop();
        if let Some(publisher) = &self.publisher {
            // Start the publisher's grace period with the host's, not after
            // the catalog and scheduler joins have already consumed time.
            publisher.request_stop();
        }
        self.catalog.stop();
        if let Some(thread) = self.catalog_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Some(mut publisher) = self.publisher.take() {
            publisher.stop_by(publisher_deadline);
        }
    }
}

#[derive(Clone)]
pub struct Control {
    stopped: Arc<AtomicBool>,
    commands: mpsc::Sender<Command>,
    wake: ScheduleWake,
}

pub(crate) enum ScheduleWatchEvent {
    Changed(ScheduleChange),
}

impl Control {
    pub fn wake(&self) {
        self.wake.signal();
    }

    pub fn create(
        &self,
        request: CreateScheduleRequest,
    ) -> Result<CreateScheduleResponse, Failure> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.commands
            .send(Command::Create { request, reply })
            .map_err(|_| Failure::unavailable())?;
        answer
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| Failure::unavailable())?
    }

    pub fn list(
        &self,
        workspace: koru_delegate_core::WorkspaceId,
    ) -> Result<ListSchedulesResponse, Failure> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.commands
            .send(Command::List { workspace, reply })
            .map_err(|_| Failure::unavailable())?;
        answer
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| Failure::unavailable())?
    }

    pub fn rename(
        &self,
        request: RenameScheduleRequest,
    ) -> Result<RenameScheduleResponse, Failure> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.commands
            .send(Command::Rename { request, reply })
            .map_err(|_| Failure::unavailable())?;
        answer
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| Failure::unavailable())?
    }

    pub fn edit(&self, request: EditScheduleRequest) -> Result<EditScheduleResponse, Failure> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.commands
            .send(Command::Edit { request, reply })
            .map_err(|_| Failure::unavailable())?;
        answer
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| Failure::unavailable())?
    }

    pub fn pause(&self, request: PauseScheduleRequest) -> Result<PauseScheduleResponse, Failure> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.commands
            .send(Command::Pause { request, reply })
            .map_err(|_| Failure::unavailable())?;
        answer
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| Failure::unavailable())?
    }

    pub fn resume(
        &self,
        request: ResumeScheduleRequest,
    ) -> Result<ResumeScheduleResponse, Failure> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.commands
            .send(Command::Resume { request, reply })
            .map_err(|_| Failure::unavailable())?;
        answer
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| Failure::unavailable())?
    }

    pub fn retire(
        &self,
        request: RetireScheduleRequest,
    ) -> Result<RetireScheduleResponse, Failure> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.commands
            .send(Command::Retire { request, reply })
            .map_err(|_| Failure::unavailable())?;
        answer
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| Failure::unavailable())?
    }

    pub fn watch(
        &self,
        workspace: koru_delegate_core::WorkspaceId,
        after: Option<ScheduleChange>,
        sink: mpsc::SyncSender<ScheduleWatchEvent>,
    ) -> Result<u64, Failure> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.commands
            .send(Command::Watch {
                workspace,
                after,
                sink,
                reply,
            })
            .map_err(|_| Failure::unavailable())?;
        answer
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| Failure::unavailable())?
    }

    pub fn cancel_watch(&self, watcher: u64) {
        let _ = self.commands.send(Command::CancelWatch(watcher));
    }

    #[cfg(feature = "test-support")]
    pub fn fixture_wake_and_wait(&self) -> Result<(), String> {
        let (completed, wait) = mpsc::sync_channel(1);
        self.commands
            .send(Command::FixtureWake(completed))
            .map_err(|_| "the schedule evaluator stopped".to_owned())?;
        wait.recv_timeout(Duration::from_secs(30))
            .map_err(|_| "the schedule evaluation did not complete".to_owned())?
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        let _ = self.commands.send(Command::Stop);
    }
}

#[cfg(test)]
pub(crate) fn test_control(actor: mpsc::Sender<ActorCommand>) -> Control {
    let (commands, inbox) = mpsc::channel();
    let wake_commands = commands.clone();
    let wake = ScheduleWake::new(move || {
        let _ = wake_commands.send(Command::Wake);
    });
    std::thread::spawn(move || {
        while let Ok(command) = inbox.recv() {
            if matches!(command, Command::Stop) {
                let _ = actor.send(ActorCommand::ScheduleStopped);
                return;
            }
        }
    });
    Control { commands, wake, stopped: Arc::new(AtomicBool::new(false)) }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct CatalogControl {
    stopped: AtomicBool,
    socket: Mutex<Option<TcpStream>>,
    retry: Condvar,
}

impl CatalogControl {
    fn new() -> Self {
        Self {
            stopped: AtomicBool::new(false),
            socket: Mutex::new(None),
            retry: Condvar::new(),
        }
    }

    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    fn install(&self, socket: Option<TcpStream>) -> bool {
        let mut current = self
            .socket
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.stopped() {
            if let Some(socket) = socket {
                let _ = socket.shutdown(Shutdown::Both);
            }
            false
        } else {
            *current = socket;
            true
        }
    }

    fn clear(&self) {
        self.socket
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }

    fn wait_retry(&self) -> bool {
        let guard = self
            .socket
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _ = self.retry.wait_timeout(guard, REPAIR_BACKOFF);
        self.stopped()
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(socket) = self
            .socket
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = socket.shutdown(Shutdown::Both);
        }
        self.retry.notify_all();
    }
}

fn spawn_catalog_watch(
    client: koru_api_client::Client,
    wake: ScheduleWake,
    control: Arc<CatalogControl>,
    initial: koru_store_client::CatalogWatch,
) -> Result<JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name("delegate-schedule-catalog".to_owned())
        .spawn(move || {
            let mut initial = Some(initial);
            loop {
                if control.stopped() {
                    return;
                }
                let mut watch = match initial.take() {
                    Some(watch) => watch,
                    None => {
                        match koru_store_client::CatalogWatch::open(&client.store().discovery()) {
                            Ok(watch) => watch,
                            Err(_) => {
                                if control.wait_retry() {
                                    return;
                                }
                                continue;
                            }
                        }
                    }
                };
                let Some(abort) = watch.abort_handle() else {
                    return;
                };
                if !control.install(Some(abort)) {
                    return;
                }
                while watch.next_generation().is_ok() {
                    wake.signal();
                }
                control.clear();
                if control.stopped() {
                    return;
                }
                wake.signal();
                if control.wait_retry() {
                    return;
                }
            }
        })
        .map_err(|error| format!("could not start Store catalog watch: {error}"))
}

fn run_event_publisher(
    mut events: Box<dyn ScheduleEvents>,
    wake: ScheduleWake,
    inbox: mpsc::Receiver<PublisherCommand>,
) {
    let mut retry_at = None;
    let mut unhealthy = false;
    wake.signal();
    loop {
        let now = Instant::now();
        if wake.take() || retry_at.is_some_and(|at| at <= now) {
            match events.maintain() {
                Ok(report) => {
                    retry_at = report.retry_needed.then(|| Instant::now() + REPAIR_BACKOFF);
                    if unhealthy {
                        tracing::info!(target: "koru::delegate", "schedule event publication recovered");
                    }
                    unhealthy = false;
                }
                Err(error) => {
                    if !unhealthy {
                        tracing::warn!(
                            target: "koru::delegate",
                            port = error.port.name(),
                            code = error.code,
                            "schedule event publication needs repair"
                        );
                    }
                    unhealthy = true;
                    retry_at = Some(Instant::now() + REPAIR_BACKOFF);
                }
            }
        }

        let received = match retry_at {
            Some(at) => inbox.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => inbox
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        match received {
            Ok(PublisherCommand::Wake) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(PublisherCommand::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn run(
    mut coordinator: ScheduleCoordinator,
    actor: mpsc::Sender<ActorCommand>,
    authority_gate: Arc<ScheduleEventAuthorityGate>,
    wake: ScheduleWake,
    publisher_wake: ScheduleWake,
    inbox: mpsc::Receiver<Command>,
    stopped: Arc<AtomicBool>,
) {
    let stopped_actor = actor.clone();
    let mut effects = ActorEffects::new(actor, stopped);
    let mut exact_watch: Option<Box<dyn ScheduleChangeWatch>> = None;
    let mut exact_targets = None;
    let mut state = LoopState::default();
    let mut evaluation_unhealthy = false;
    let mut event_unhealthy = false;
    let mut watch_unhealthy = false;
    let mut generation = ScheduleChange {
        instance: uuid::Uuid::new_v4(),
        generation: 0,
    };
    let mut watchers: Vec<ScheduleWatcher> = Vec::new();
    let mut next_watcher = 1_u64;
    #[cfg(feature = "test-support")]
    let mut fixture_completions: Vec<mpsc::SyncSender<Result<(), String>>> = Vec::new();
    wake.signal();
    loop {
        let now = Instant::now();
        let wall_now = wall_now_ms();
        #[cfg(feature = "test-support")]
        let fixture_woken = !fixture_completions.is_empty();
        #[cfg(not(feature = "test-support"))]
        let fixture_woken = false;
        let woke = wake.take() || fixture_woken;
        if state.take_evaluation(woke, wall_now, now) {
            let Some(_authority) = authority_gate.enter() else {
                break;
            };
            reconcile_schedule_events(&mut coordinator, &mut event_unhealthy);
            #[cfg(feature = "test-support")]
            let mut fixture_result = Ok(());
            let zone = koru_time_zone::SystemZone::detect();
            match coordinator.evaluate(&mut effects, wall_now, &zone) {
                Ok(report) => {
                    state.deadline_ms = report.next_deadline_ms;
                    state.repair_at = None;
                    if report.failures != 0 && !evaluation_unhealthy {
                        tracing::warn!(
                            target: "koru::delegate",
                            failures = report.failures,
                            "schedule evaluation needs repair"
                        );
                    } else if report.failures == 0 && evaluation_unhealthy {
                        tracing::info!(target: "koru::delegate", "schedule evaluation recovered");
                    }
                    evaluation_unhealthy = report.failures != 0;
                    #[cfg(feature = "test-support")]
                    if !fixture_completions.is_empty() && report.failures != 0 {
                        fixture_result = Err(format!(
                            "schedule evaluation reported {} failures",
                            report.failures
                        ));
                    }
                }
                Err(error) => {
                    #[cfg(feature = "test-support")]
                    if !fixture_completions.is_empty() {
                        fixture_result = Err(format!("schedule evaluation failed: {error}"));
                    }
                    if !evaluation_unhealthy {
                        tracing::warn!(
                            target: "koru::delegate",
                            port = error.port.name(),
                            code = error.code,
                            "schedule evaluation needs repair"
                        );
                    }
                    evaluation_unhealthy = true;
                    state.deadline_ms = None;
                    state.repair_at = Some(Instant::now() + REPAIR_BACKOFF);
                }
            }
            let mut watch_failed = false;
            match coordinator.schedules_mut().discover_active() {
                Ok(catalog) => {
                    let targets = ScheduleWatchTargets::from_catalog(&catalog);
                    if exact_targets.as_ref() != Some(&targets) {
                        match coordinator
                            .schedules_mut()
                            .watch_changes(&targets, wake.clone())
                        {
                            Ok(replacement) => {
                                if let Some(previous) = exact_watch.replace(replacement) {
                                    previous.close();
                                }
                                exact_targets = Some(targets);
                                // Subscribe, then authoritatively re-read.
                                wake.signal();
                            }
                            Err(error) => {
                                #[cfg(feature = "test-support")]
                                if !fixture_completions.is_empty() && fixture_result.is_ok() {
                                    fixture_result =
                                        Err(format!("schedule watch refresh failed: {error}"));
                                }
                                if !watch_unhealthy {
                                    tracing::warn!(target: "koru::delegate", port = error.port.name(), code = error.code, "schedule watch needs repair");
                                }
                                watch_failed = true;
                                state.repair_at = Some(Instant::now() + REPAIR_BACKOFF);
                            }
                        }
                    }
                }
                Err(error) => {
                    #[cfg(feature = "test-support")]
                    if !fixture_completions.is_empty() && fixture_result.is_ok() {
                        fixture_result = Err(format!("schedule discovery failed: {error}"));
                    }
                    if !watch_unhealthy {
                        tracing::warn!(target: "koru::delegate", port = error.port.name(), code = error.code, "schedule watch needs repair");
                    }
                    watch_failed = true;
                    state.repair_at = Some(Instant::now() + REPAIR_BACKOFF);
                }
            }
            if !watch_failed && watch_unhealthy {
                tracing::info!(target: "koru::delegate", "schedule watch recovered");
            }
            watch_unhealthy = watch_failed;
            generation.generation = generation.generation.saturating_add(1);
            notify_watchers(&mut watchers, &generation);
            #[cfg(feature = "test-support")]
            for completed in fixture_completions.drain(..) {
                let _ = completed.send(fixture_result.clone());
            }
            publisher_wake.signal();
        }

        let wait = state.next_wait(wall_now_ms(), Instant::now());
        let received = match wait {
            Some(wait) => inbox.recv_timeout(wait),
            None => inbox
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        match received {
            Ok(Command::Wake) => {}
            Ok(Command::Create { request, reply }) => {
                let zone = koru_time_zone::SystemZone::detect();
                let result = {
                    let Some(_authority) = authority_gate.enter() else {
                        break;
                    };
                    coordinator
                        .reconcile_created_event(request.schedule)
                        .and_then(|()| {
                            coordinator.create_named_schedule(
                                request.workspace,
                                request.schedule,
                                request.name,
                                request.template,
                                request.recurrence,
                                wall_now_ms(),
                                &zone,
                            )
                        })
                        .map(|listing| CreateScheduleResponse {
                            schedule: schedule_view(listing),
                        })
                        .map_err(schedule_failure)
                };
                let created = answer_then_publish(reply, result, &publisher_wake);
                if created {
                    wake.signal();
                }
            }
            Ok(Command::Rename { request, reply }) => {
                let zone = koru_time_zone::SystemZone::detect();
                let result = {
                    let Some(_authority) = authority_gate.enter() else {
                        break;
                    };
                    coordinator
                        .rename_schedule(
                            request.workspace,
                            request.schedule,
                            &request.expected_name,
                            &request.name,
                            wall_now_ms(),
                            &zone,
                        )
                        .map(|listing| RenameScheduleResponse {
                            schedule: schedule_view(listing),
                        })
                        .map_err(schedule_failure)
                };
                let changed = result.is_ok();
                let _ = reply.send(result);
                if changed {
                    generation.generation = generation.generation.saturating_add(1);
                    notify_watchers(&mut watchers, &generation);
                }
            }
            Ok(Command::Edit { request, reply }) => {
                let zone = koru_time_zone::SystemZone::detect();
                let result = {
                    let Some(_authority) = authority_gate.enter() else {
                        break;
                    };
                    coordinator
                        .edit_schedule(
                            request.workspace,
                            request.schedule,
                            request.expected_revision,
                            &request.expected_name,
                            &request.name,
                            request.template,
                            request.recurrence,
                            wall_now_ms(),
                            &zone,
                        )
                        .map(|listing| EditScheduleResponse {
                            schedule: schedule_view(listing),
                        })
                        .map_err(schedule_failure)
                };
                let changed = answer_then_publish(reply, result, &publisher_wake);
                if changed {
                    generation.generation = generation.generation.saturating_add(1);
                    notify_watchers(&mut watchers, &generation);
                    wake.signal();
                }
            }
            Ok(Command::Pause { request, reply }) => {
                let zone = koru_time_zone::SystemZone::detect();
                let result = {
                    let Some(_authority) = authority_gate.enter() else {
                        break;
                    };
                    coordinator
                        .pause_schedule(
                            request.workspace,
                            request.schedule,
                            request.expected_revision,
                            wall_now_ms(),
                            &zone,
                        )
                        .map(|listing| PauseScheduleResponse {
                            schedule: schedule_view(listing),
                        })
                        .map_err(schedule_failure)
                };
                let changed = answer_then_publish(reply, result, &publisher_wake);
                if changed {
                    generation.generation = generation.generation.saturating_add(1);
                    notify_watchers(&mut watchers, &generation);
                    wake.signal();
                }
            }
            Ok(Command::Resume { request, reply }) => {
                let zone = koru_time_zone::SystemZone::detect();
                let result = {
                    let Some(_authority) = authority_gate.enter() else {
                        break;
                    };
                    coordinator
                        .resume_schedule(
                            request.workspace,
                            request.schedule,
                            request.expected_revision,
                            wall_now_ms(),
                            &zone,
                        )
                        .map(|listing| ResumeScheduleResponse {
                            schedule: schedule_view(listing),
                        })
                        .map_err(schedule_failure)
                };
                let changed = answer_then_publish(reply, result, &publisher_wake);
                if changed {
                    generation.generation = generation.generation.saturating_add(1);
                    notify_watchers(&mut watchers, &generation);
                    wake.signal();
                }
            }
            Ok(Command::Retire { request, reply }) => {
                let zone = koru_time_zone::SystemZone::detect();
                let result = {
                    let Some(_authority) = authority_gate.enter() else {
                        break;
                    };
                    coordinator
                        .retire_schedule(request.workspace, request.schedule, wall_now_ms(), &zone)
                        .map(|listing| RetireScheduleResponse {
                            schedule: schedule_view(listing),
                        })
                        .map_err(schedule_failure)
                };
                let changed = result.is_ok();
                let _ = reply.send(result);
                if changed {
                    generation.generation = generation.generation.saturating_add(1);
                    notify_watchers(&mut watchers, &generation);
                    wake.signal();
                }
            }
            Ok(Command::List { workspace, reply }) => {
                let zone = koru_time_zone::SystemZone::detect();
                let result = coordinator
                    .list_schedules(workspace, wall_now_ms(), &zone)
                    .map(|listed| ListSchedulesResponse {
                        schedules: listed.schedules.into_iter().map(schedule_view).collect(),
                        incomplete: listed.incomplete,
                    })
                    .map_err(schedule_failure);
                let _ = reply.send(result);
            }
            Ok(Command::Watch {
                workspace,
                after,
                sink,
                reply,
            }) => {
                let zone = koru_time_zone::SystemZone::detect();
                if let Err(error) = coordinator.list_schedules(workspace, wall_now_ms(), &zone) {
                    let _ = reply.send(Err(schedule_failure(error)));
                    continue;
                }
                let current_is_new = after.as_ref() != Some(&generation);
                let admitted = !current_is_new
                    || sink
                        .try_send(ScheduleWatchEvent::Changed(generation.clone()))
                        .is_ok();
                if admitted {
                    // Store schedule wakes deliberately carry no room or
                    // document identity. One global generation therefore
                    // invalidates each capability-admitted workspace stream.
                    let watcher = next_watcher;
                    next_watcher = next_watcher.saturating_add(1);
                    watchers.push(ScheduleWatcher { watcher, sink });
                    let _ = reply.send(Ok(watcher));
                } else {
                    let _ = reply.send(Err(Failure::unavailable()));
                }
            }
            Ok(Command::CancelWatch(watcher)) => {
                cancel_watcher(&mut watchers, watcher);
            }
            #[cfg(feature = "test-support")]
            Ok(Command::FixtureWake(completed)) => fixture_completions.push(completed),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if let Some(watch) = exact_watch {
        watch.close();
    }
    let _ = stopped_actor.send(ActorCommand::ScheduleStopped);
}

struct ScheduleWatcher {
    watcher: u64,
    sink: mpsc::SyncSender<ScheduleWatchEvent>,
}

fn notify_watchers(watchers: &mut Vec<ScheduleWatcher>, generation: &ScheduleChange) {
    watchers.retain(|watcher| {
        watcher
            .sink
            .try_send(ScheduleWatchEvent::Changed(generation.clone()))
            .is_ok()
    });
}

fn cancel_watcher(watchers: &mut Vec<ScheduleWatcher>, watcher: u64) {
    watchers.retain(|candidate| candidate.watcher != watcher);
}

fn schedule_view(listing: ScheduleListing) -> ScheduleView {
    let next_firing = match listing.next {
        ScheduleNext::None => None,
        ScheduleNext::Exact(at_ms) => Some(ScheduleNextFiring::Exact { at_ms }),
        ScheduleNext::OwnerLocal => Some(ScheduleNextFiring::OwnerLocal),
    };
    ScheduleView {
        workspace: listing.workspace,
        document: listing.document,
        name: Some(listing.name),
        header: listing.header,
        state: listing.state,
        next_firing,
        firings: listing
            .firings
            .into_iter()
            .map(ScheduleFiringView::from)
            .collect(),
    }
}

fn schedule_failure(error: PortError) -> Failure {
    let retryable = !matches!(
        error.code.as_str(),
        "unknown_workspace"
            | "schedule_document"
            | "schedule_conflict"
            | "schedule_not_found"
            | "schedule_wrong_workspace"
            | "schedule_not_owner"
            | "schedule_retired"
            | "schedule_read_only"
            | "schedule_name_conflict"
            | "schedule_revision_conflict"
            | "schedule_handoff_pending"
            | "schedule_state_conflict"
    );
    let code = match error.code.as_str() {
        "unknown_workspace" => "invalid_request",
        "schedule_document" | "recurrence" => "invalid_schedule",
        "schedule_conflict" => "schedule_conflict",
        "schedule_not_found" => "schedule_not_found",
        "schedule_wrong_workspace" => "schedule_wrong_workspace",
        "schedule_not_owner" => "schedule_not_owner",
        "schedule_retired" => "schedule_retired",
        "schedule_read_only" => "schedule_read_only",
        "schedule_name_conflict" => "schedule_name_conflict",
        "schedule_revision_conflict" => "schedule_revision_conflict",
        "schedule_handoff_pending" => "schedule_handoff_pending",
        "schedule_state_conflict" => "schedule_state_conflict",
        _ => "schedule_unavailable",
    };
    Failure::new(
        code,
        "The schedule operation could not be completed.",
        retryable,
    )
    .with_detail("port", error.port.name())
}

fn answer_then_publish<T, E>(
    reply: mpsc::SyncSender<Result<T, E>>,
    result: Result<T, E>,
    publisher_wake: &ScheduleWake,
) -> bool {
    let succeeded = result.is_ok();
    let _ = reply.send(result);
    // The row is already durable and ready. Only after the API answer is in
    // its rendezvous may the independent worker enter gateway delivery.
    publisher_wake.signal();
    succeeded
}

struct ActorEffects {
    actor: mpsc::Sender<ActorCommand>,
    stopped: Arc<AtomicBool>,
    routine: Option<routine::PendingRoutine>,
}

impl ActorEffects {
    fn disconnected() -> PortError {
        PortError::new(
            Port::Schedules,
            "actor_closed",
            "the delegate actor stopped",
        )
    }
}

impl ScheduleEffects for ActorEffects {
    fn begin_routine(
        &mut self,
        prepared: &koru_delegate_core::RoutinePrepared,
        admission: &koru_delegate_core::BotAdmissionBarrier,
        proof: &koru_delegate_core::FreshBotPolicyProof,
        scope: ScopeIdentity,
        authored_scope: koru_delegate_core::WorkspaceScope,
        deadline: Instant,
    ) -> PortResult<ScheduleAdmission> {
        self.begin_routine_wait(prepared, admission, proof, scope, authored_scope, deadline)
    }

    fn release_routine(&mut self, run: RunId, deadline: Instant) -> PortResult<()> {
        self.release_routine_wait(run, deadline)
    }

    fn begin(
        &mut self,
        run: RunId,
        scope: ScopeIdentity,
        authored_scope: koru_delegate_core::WorkspaceScope,
    ) -> PortResult<ScheduleAdmission> {
        self.no_pending_routine()?;
        let (reply, answer) = mpsc::sync_channel(1);
        self.actor
            .send(ActorCommand::ScheduleBegin {
                run,
                scope,
                authored_scope,
                reply,
            })
            .map_err(|_| Self::disconnected())?;
        answer.recv().map_err(|_| Self::disconnected())?
    }

    fn finish(
        &mut self,
        run: RunId,
        claim: SlotClaim,
        scope: koru_delegate_scope::ResolvedScope,
        authored_scope: koru_delegate_core::WorkspaceScope,
    ) -> PortResult<Option<Started>> {
        self.no_pending_routine()?;
        let (reply, answer) = mpsc::sync_channel(1);
        self.actor
            .send(ActorCommand::ScheduleFinish {
                run,
                claim,
                scope,
                authored_scope,
                reply,
            })
            .map_err(|_| Self::disconnected())?;
        answer.recv().map_err(|_| Self::disconnected())??;
        Ok(None)
    }

    fn abort(&mut self, run: RunId) {
        let _ = self.actor.send(ActorCommand::ScheduleAbort { run });
    }

    fn reconcile(
        &mut self,
        claim: SlotClaim,
        authored_scope: koru_delegate_core::WorkspaceScope,
    ) -> PortResult<Option<Started>> {
        self.no_pending_routine()?;
        let (reply, answer) = mpsc::sync_channel(1);
        self.actor
            .send(ActorCommand::ScheduleReconcile {
                claim,
                authored_scope,
                reply,
            })
            .map_err(|_| Self::disconnected())?;
        answer.recv().map_err(|_| Self::disconnected())??;
        Ok(None)
    }
}

fn wall_now_ms() -> i64 {
    crate::stubs::wall_now_ms()
}

fn evaluation_due(
    woken: bool,
    deadline_ms: Option<i64>,
    wall_now_ms: i64,
    repair_at: Option<Instant>,
    monotonic_now: Instant,
) -> bool {
    woken
        || deadline_ms.is_some_and(|deadline| deadline <= wall_now_ms)
        || repair_at.is_some_and(|deadline| deadline <= monotonic_now)
}

fn next_wait(
    deadline_ms: Option<i64>,
    wall_now_ms: i64,
    repair_at: Option<Instant>,
    monotonic_now: Instant,
) -> Option<Duration> {
    let schedule_wait = deadline_ms.map(|deadline| {
        Duration::from_millis(u64::try_from(deadline.saturating_sub(wall_now_ms)).unwrap_or(0))
    });
    let repair_wait = repair_at.map(|deadline| deadline.saturating_duration_since(monotonic_now));
    match (schedule_wait, repair_wait) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn reconcile_schedule_events(coordinator: &mut ScheduleCoordinator, unhealthy: &mut bool) {
    match coordinator.reconcile_prepared_events(SCHEDULER_RECONCILE_BATCH) {
        Ok(_) => {
            if *unhealthy {
                tracing::info!(target: "koru::delegate", "schedule event reconciliation recovered");
            }
            *unhealthy = false;
        }
        Err(error) => {
            if !*unhealthy {
                tracing::warn!(
                    target: "koru::delegate",
                    port = error.port.name(),
                    code = error.code,
                    "schedule event publication needs repair"
                );
            }
            *unhealthy = true;
        }
    }
}

#[cfg(test)]
mod tests;
