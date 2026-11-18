// SPDX-License-Identifier: MPL-2.0

//! The Windows local launcher. The engine queues commands; a lifecycle thread
//! owns its Bot-keyed table. Blocking stops each have their own lifecycle task.
//! No launch, stop or settings read runs on the engine's dispatch thread.

mod registration;
use crate::{Error, bot_worker::Bootstrap};
use koru_agent_worker::machine::{RestartBudget, State};
use koru_agent_worker::supervisor::{self, LaunchPlan, Priority, WorkerHandle, WorkerSpec};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
    mpsc,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Clone, Copy)]
pub struct Policy {
    pub registration_timeout: Duration,
    pub idle_window: Duration,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            registration_timeout: Duration::from_secs(10),
            idle_window: Duration::from_millis(crate::capacity::IDLE_WINDOW_MS),
        }
    }
}

pub enum Event {
    CapacityChanged,
    Allocated {
        bot: Uuid,
        incarnation: Uuid,
    },
    Registered {
        bot: Uuid,
        incarnation: Uuid,
        at_ms: i64,
    },
    Ended {
        bot: Uuid,
        incarnation: Uuid,
    },
}
enum Command {
    RefreshCapacity(mpsc::SyncSender<crate::worker::Reply>),
    Ensure(Uuid),
    Stop(Uuid),
    Shutdown,
}
const PENDING: u8 = 0;
const CONNECTED: u8 = 1;
const DISCONNECTED: u8 = 2;
struct Gate {
    bootstrap: Bootstrap,
    registration: AtomicU8,
    stopping: AtomicBool,
    last_heard: AtomicU64,
    sessions: AtomicU32,
    deadline: Instant,
}
type Registry = Arc<Mutex<BTreeMap<Uuid, Arc<Gate>>>>;
struct Entry {
    gate: Arc<Gate>,
    handle: Arc<WorkerHandle<Arc<Gate>>>,
    idle_since: Option<Instant>,
    stop_started: bool,
    wake_after_stop: bool,
}

pub struct Launcher {
    sender: mpsc::SyncSender<Command>,
    capacity: Arc<AtomicU32>,
    registry: Registry,
    events: mpsc::Receiver<Event>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Launcher {
    pub fn start(
        root: PathBuf,
        segment: String,
        policy: Policy,
        requests: mpsc::SyncSender<crate::carrier::Request>,
    ) -> Result<Self, Error> {
        // Unix containment has not been qualified; never silently claim it.
        if !cfg!(windows) {
            return Err(Error::EngineUnavailable);
        }
        let listener = koru_ipc::Listener::bind(crate::bot_worker::endpoint(&root, &segment)?)
            .map_err(|_| Error::EngineUnavailable)?;
        let registry: Registry = Arc::new(Mutex::new(BTreeMap::new()));
        let capacity = Arc::new(AtomicU32::new(1));
        let capacity_view = capacity.clone();
        let (sender, receiver) = mpsc::sync_channel(64);
        let (event_tx, events) = mpsc::channel();
        registration::serve(listener, registry.clone(), event_tx.clone(), requests)?;
        let entries = registry.clone();
        let thread = std::thread::Builder::new()
            .name("koru-bots launcher".into())
            .spawn(move || {
                run(
                    root,
                    segment,
                    policy,
                    entries,
                    receiver,
                    event_tx,
                    capacity_view,
                )
            })
            .map_err(|_| Error::EngineUnavailable)?;
        Ok(Self {
            sender,
            capacity,
            registry,
            events,
            thread: Some(thread),
        })
    }
    pub fn ensure(&self, bot: Uuid) -> Result<(), Error> {
        if bot.is_nil() {
            return Err(Error::InvalidRequest);
        }
        self.sender
            .try_send(Command::Ensure(bot))
            .map_err(|_| Error::EngineUnavailable)
    }
    pub fn capacity(&self) -> u32 {
        self.capacity.load(Ordering::Acquire)
    }
    pub(crate) fn refresh_capacity(&self, reply: mpsc::SyncSender<crate::worker::Reply>) {
        if self
            .sender
            .try_send(Command::RefreshCapacity(reply.clone()))
            .is_err()
        {
            let _ = reply.try_send(crate::worker::Reply::Refused(Error::EngineUnavailable));
        }
    }
    pub fn incarnation(&self, bot: Uuid) -> Option<Uuid> {
        self.registry
            .lock()
            .ok()?
            .get(&bot)
            .map(|gate| gate.bootstrap.incarnation)
    }
    /// A cache for launcher idle exit, written only from the engine's one
    /// authoritative admission record. Worker heartbeats do not supply it.
    pub fn occupied(&self, bot: Uuid, worker: Uuid, count: u32) {
        if let Ok(registry) = self.registry.lock() {
            if let Some(gate) = registry
                .get(&bot)
                .filter(|gate| gate.bootstrap.incarnation == worker)
            {
                gate.sessions.store(count, Ordering::Release);
            }
        }
    }
    pub fn stop(&self, bot: Uuid) -> Result<(), Error> {
        self.sender
            .try_send(Command::Stop(bot))
            .map_err(|_| Error::EngineUnavailable)
    }
    pub fn events(&self) -> impl Iterator<Item = Event> + '_ {
        self.events.try_iter()
    }
    pub fn last_heard(&self, bot: Uuid, incarnation: Uuid) -> Option<i64> {
        self.registry
            .lock()
            .ok()?
            .get(&bot)
            .filter(|g| g.bootstrap.incarnation == incarnation)
            .map(|g| g.last_heard.load(Ordering::Acquire) as i64)
    }
}
impl Drop for Launcher {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    root: PathBuf,
    segment: String,
    policy: Policy,
    registry: Registry,
    commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<Event>,
    capacity: Arc<AtomicU32>,
) {
    capacity.store(crate::capacity::configured(&root), Ordering::Release);
    let mut entries: BTreeMap<Uuid, Entry> = BTreeMap::new();
    let (stopped, stop_results) = mpsc::channel();
    let mut wake_again = VecDeque::new();
    loop {
        let command = match wake_again.pop_front() {
            Some(bot) => Ok(Command::Ensure(bot)),
            None => commands.recv_timeout(Duration::from_millis(25)),
        };
        match command {
            Ok(Command::RefreshCapacity(reply)) => {
                let sessions = crate::capacity::configured(&root);
                capacity.store(sessions, Ordering::Release);
                let _ = events.send(Event::CapacityChanged);
                let _ = reply.try_send(crate::worker::Reply::Capacity { sessions });
            }
            Ok(Command::Ensure(bot)) if !entries.contains_key(&bot) => {
                capacity.store(crate::capacity::configured(&root), Ordering::Release);
                let gate = Arc::new(Gate {
                    bootstrap: Bootstrap {
                        bot,
                        incarnation: Uuid::new_v4(),
                        root: root.clone(),
                        segment: segment.clone(),
                        capability: format!(
                            "{}{}",
                            Uuid::new_v4().simple(),
                            Uuid::new_v4().simple()
                        ),
                    },
                    registration: AtomicU8::new(PENDING),
                    stopping: AtomicBool::new(false),
                    last_heard: AtomicU64::new(0),
                    sessions: AtomicU32::new(0),
                    deadline: Instant::now() + policy.registration_timeout,
                });
                registry
                    .lock()
                    .expect("worker registry")
                    .insert(bot, gate.clone());
                let _ = events.send(Event::Allocated {
                    bot,
                    incarnation: gate.bootstrap.incarnation,
                });
                let handle = launch(gate.clone(), policy);
                entries.insert(
                    bot,
                    Entry {
                        gate,
                        handle: Arc::new(handle),
                        idle_since: None,
                        stop_started: false,
                        wake_after_stop: false,
                    },
                );
            }
            Ok(Command::Ensure(bot)) => {
                if let Some(entry) = entries.get_mut(&bot) {
                    if entry.stop_started {
                        entry.wake_after_stop = true;
                    } else {
                        entry.idle_since = None;
                    }
                }
            }
            Ok(Command::Stop(bot)) => {
                if let Some(entry) = entries.get_mut(&bot) {
                    entry.wake_after_stop = false;
                    begin_stop(entry, &stopped);
                }
            }
            Ok(Command::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            _ => {}
        }
        // A timeout keeps ownership, but the supervisor may reap the child
        // later. Its terminal Stopped state is explicit confirmation of death,
        // unlike an empty current payload during startup or a failed probe.
        let completed: Vec<_> = entries
            .iter()
            .filter(|(_, entry)| {
                entry.stop_started && entry.handle.status().state == State::Stopped
            })
            .map(|(bot, entry)| (*bot, entry.gate.bootstrap.incarnation, true))
            .collect();
        for (bot, incarnation, gone) in stop_results.try_iter().chain(completed) {
            if gone
                && entries
                    .get(&bot)
                    .is_some_and(|e| e.gate.bootstrap.incarnation == incarnation)
            {
                // This is the only removal path. Timed-out stops retain ownership.
                if entries
                    .remove(&bot)
                    .is_some_and(|entry| entry.wake_after_stop)
                {
                    wake_again.push_back(bot);
                }
                registry.lock().expect("worker registry").remove(&bot);
                let _ = events.send(Event::Ended { bot, incarnation });
            }
        }
        for entry in entries.values_mut() {
            if entry.stop_started {
                continue;
            }
            let gate = &entry.gate;
            if matches!(
                entry.handle.status().state,
                State::Exhausted | State::Stopped
            ) || (gate.registration.load(Ordering::Acquire) == PENDING
                && Instant::now() >= gate.deadline)
                || gate.registration.load(Ordering::Acquire) == DISCONNECTED
            {
                begin_stop(entry, &stopped);
                continue;
            }
            if gate.registration.load(Ordering::Acquire) == CONNECTED
                && gate.sessions.load(Ordering::Acquire) == 0
            {
                let since = entry.idle_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= policy.idle_window {
                    begin_stop(entry, &stopped);
                }
            } else {
                entry.idle_since = None;
            }
        }
    }
    // Shutdown is already off dispatch. No process can survive the engine's Job.
    for entry in entries.values() {
        entry.gate.stopping.store(true, Ordering::Release);
    }
    for entry in entries.values() {
        let _ = entry.handle.stop();
    }
}

fn begin_stop(entry: &mut Entry, results: &mpsc::Sender<(Uuid, Uuid, bool)>) {
    if entry.stop_started {
        return;
    }
    entry.gate.stopping.store(true, Ordering::Release);
    let handle = entry.handle.clone();
    let results = results.clone();
    let bot = entry.gate.bootstrap.bot;
    let incarnation = entry.gate.bootstrap.incarnation;
    if std::thread::Builder::new()
        .name("koru-bot stop".into())
        .spawn(move || {
            let gone = handle.stop().child_is_gone();
            let _ = results.send((bot, incarnation, gone));
        })
        .is_ok()
    {
        entry.stop_started = true;
    }
}

fn launch(gate: Arc<Gate>, policy: Policy) -> WorkerHandle<Arc<Gate>> {
    let child = gate.clone();
    let mut spec = WorkerSpec::new(
        "bot",
        move |_| {
            Ok(LaunchPlan {
                args: vec!["--bot".into(), child.bootstrap.bot.to_string().into()],
                bootstrap: serde_json::to_vec(&child.bootstrap)
                    .map_err(|_| "invalid_request".to_owned())?,
                payload: child.clone(),
            })
        },
        |gate: &Arc<Gate>| {
            if gate.registration.load(Ordering::Acquire) == CONNECTED {
                Ok(())
            } else {
                Err("not_registered".into())
            }
        },
    );
    spec.ready_deadline = policy.registration_timeout;
    spec.stop_deadline = Duration::from_secs(2);
    spec.priority = Priority::Background;
    // Engine turn policy, not the singleton supervisor, decides when to retry.
    spec.restart_budget = RestartBudget {
        max_restarts: 0,
        window: Duration::from_secs(60),
    };
    spec.graceful_stop = Some(Box::new(|gate| {
        gate.stopping.store(true, Ordering::Release);
        Ok(())
    }));
    supervisor::start(spec)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
