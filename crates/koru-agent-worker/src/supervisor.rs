// SPDX-License-Identifier: MPL-2.0

//! The parent side: spawn a role of our own executable — or a sibling program
//! shipped beside it — prove it ready, watch it, and restart it inside a
//! budget, with every decision taken by the pure [`crate::machine`] and every
//! blocking wait on its own thread.
//!
//! Which program runs is [`Program`]; an optional ceiling on the child tree's
//! committed memory is [`WorkerSpec::memory_limit`]. Both are mechanism: the
//! crate still knows nothing about what the program does.
//!
//! Ownership map, because five threads meet here:
//!
//! - The **control thread** owns the machine and the current child's pipes
//!   and job handle. It never blocks on anything but its channel.
//! - A **waiter thread** per child owns the `Child` and blocks in `wait()`;
//!   its exit message is the only path back into the machine for a death,
//!   so a replacement can never spawn before the old child is reaped.
//! - A **prober thread** per child polls the role's readiness closure until
//!   the deadline; a **health thread** per ready child polls the role's
//!   health closure. Both check the current generation before speaking, so
//!   a slow probe for a dead child says nothing.
//! - **Backoff timers** are short-lived threads carrying an epoch; a stale
//!   timer is ignored.
//!
//! The generation fence is the public contract: [`WorkerHandle::issue`]
//! hands out the payload only for the generation it was asked about, so an
//! operation minted against child A is refused once child B has taken over
//! — visibly, never by routing A's order to B.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::machine::{Action, Backoff, Event, Machine, RestartBudget, State};

/// A child's ordinal under one supervisor: 1 for the first spawn, +1 each
/// respawn. The fence every lifecycle operation carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Generation(pub u64);

impl std::fmt::Display for Generation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One of the role's per-child questions — readiness, health, a graceful
/// stop — as the spec hands it over: a closure the crate calls without
/// understanding it, answering `Ok` or a reason.
///
/// A name rather than the type spelled out at each of the six places it
/// appears, because the six were the whole of `clippy::type_complexity`'s
/// complaint about this file.
pub type Probe<P> = Box<dyn Fn(&P) -> Result<(), String> + Send + Sync>;

/// The same closure once the control thread has shared it with the threads
/// that run it: the prober, the health poller, and — since C2 — the graceful
/// stop.
type SharedProbe<P> = Arc<dyn Fn(&P) -> Result<(), String> + Send + Sync>;

/// Which executable a supervised child is.
///
/// **Chosen once per supervision, not per spawn.** Every generation of one
/// supervision is the same program; what varies between them — arguments,
/// bootstrap, payload — is the [`LaunchPlan`]. It lives on [`WorkerSpec`]
/// rather than the plan for that reason, and because every role builds its
/// plan as a struct literal that a new field would break.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Program {
    /// This executable as `<current_exe> --worker <role> [plan args…]`: every
    /// self-hosted role, and the default.
    OwnRole,
    /// A separately shipped executable, named by its **file name** and
    /// resolved beside `current_exe()` — never through `PATH`, never a path a
    /// caller supplies — and run as `<dir>/<name> [plan args…]`, with no
    /// `--worker` in front: it is its own program, not a role of this one.
    ///
    /// The name is the platform's, suffix included (`koru_update::exe` is
    /// where shipped names are spelled). Anything that is not a single plain
    /// file name — a separator, `..`, a root — is refused at spawn.
    Sibling(OsString),
}

/// Whether [`WorkerSpec::memory_limit`] is enforced on this platform.
///
/// **Windows only.** There the child's Job Object carries the ceiling, and a
/// tree that reaches it is ended and reported as
/// [`WorkerEventKind::MemoryLimitReached`]. Elsewhere the crate's containment
/// seam is the process group, which has no memory ceiling, and none is
/// invented: the limit is accepted and **not applied** — the child runs
/// unbounded, the supervision logs that once when it starts, and no
/// `MemoryLimitReached` is ever reported. A caller that must not run an
/// unbounded child checks this before starting one.
pub const MEMORY_LIMIT_ENFORCED: bool = cfg!(windows);

/// What one spawn needs: the argv after `--worker <role>` (or, for a
/// [`Program::Sibling`], the sibling's whole argv), the one-line
/// bootstrap, and the role's opaque handles for this child (endpoint,
/// launch id, authorities — whatever the closures need to reach it).
pub struct LaunchPlan<P> {
    pub args: Vec<OsString>,
    pub bootstrap: Vec<u8>,
    pub payload: P,
}

/// Something the supervisor observed, for the role to log or react to.
/// Delivered on the control thread — keep the handler cheap.
#[derive(Clone, Debug)]
pub struct WorkerEvent {
    pub role: &'static str,
    pub generation: Generation,
    pub kind: WorkerEventKind,
}

/// What happened, for [`WorkerEvent`].
///
/// **`MemoryLimitReached`**: the child's tree reached
/// [`WorkerSpec::memory_limit`] and the supervisor ended it. It is reported
/// **before the `Exited` that follows**, so a role can tell an out-of-memory
/// end from an ordinary crash; the machine treats the exit as a crash all the
/// same (restart, budget). Never reported where [`MEMORY_LIMIT_ENFORCED`] is
/// false.
#[derive(Clone, Debug)]
pub enum WorkerEventKind {
    Ready,
    MemoryLimitReached { limit_bytes: u64 },
    Exited { code: Option<i32> },
    RestartScheduled { delay: Duration },
    SpawnFailed { error: String },
    ProbeFailed { error: String },
    HealthFailed { error: String },
    Exhausted,
    Held,
    Resumed,
    Stopped,
}

/// Everything a role supplies. Build with [`WorkerSpec::new`] and override
/// the defaults that matter; the closures are the role's knowledge and the
/// crate calls them without understanding them.
pub struct WorkerSpec<P> {
    /// The role's name: the `--worker` token for [`Program::OwnRole`], and
    /// the label in thread names, logs and events for either program.
    pub role: &'static str,
    /// Which executable each child is. [`Program::OwnRole`] by default.
    pub program: Program,
    /// A ceiling, in bytes, on the committed memory of each child's whole
    /// tree; `None` (the default) is no ceiling. See [`MEMORY_LIMIT_ENFORCED`]
    /// for where it applies. Zero is refused at spawn.
    pub memory_limit: Option<u64>,
    /// Mint one child's launch: argv, bootstrap, and the payload the other
    /// closures will be handed. Called once per spawn, with the generation
    /// the child will carry.
    pub launch: Box<dyn FnMut(Generation) -> Result<LaunchPlan<P>, String> + Send>,
    /// Polled until it succeeds or `ready_deadline` passes.
    pub readiness: Probe<P>,
    /// Polled every `health_interval` while ready; a failure is a crash.
    pub health: Option<Probe<P>>,
    /// Ask the child to stop on its own terms before the job is killed.
    pub graceful_stop: Option<Probe<P>>,
    pub on_event: Option<Box<dyn Fn(WorkerEvent) + Send + Sync>>,
    pub ready_deadline: Duration,
    pub ready_poll: Duration,
    pub health_interval: Duration,
    pub stop_deadline: Duration,
    pub restart_budget: RestartBudget,
    pub backoff: Backoff,
}

impl<P> WorkerSpec<P> {
    pub fn new(
        role: &'static str,
        launch: impl FnMut(Generation) -> Result<LaunchPlan<P>, String> + Send + 'static,
        readiness: impl Fn(&P) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            role,
            program: Program::OwnRole,
            memory_limit: None,
            launch: Box::new(launch),
            readiness: Box::new(readiness),
            health: None,
            graceful_stop: None,
            on_event: None,
            ready_deadline: Duration::from_secs(10),
            ready_poll: Duration::from_millis(25),
            health_interval: Duration::from_secs(15),
            stop_deadline: Duration::from_secs(5),
            restart_budget: RestartBudget {
                max_restarts: 5,
                window: Duration::from_secs(300),
            },
            backoff: Backoff {
                initial: Duration::from_millis(250),
                max: Duration::from_secs(30),
            },
        }
    }
}

/// The refusal a stale-generation operation gets, named so a log line can
/// say which side was old.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StaleGeneration {
    pub requested: Generation,
    pub current: Option<Generation>,
}

impl std::fmt::Display for StaleGeneration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.current {
            Some(current) => write!(
                f,
                "operation was minted against child generation {} but generation {current} is serving",
                self.requested
            ),
            None => write!(
                f,
                "operation was minted against child generation {} but no child is serving",
                self.requested
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitReport {
    pub generation: Generation,
    pub code: Option<i32>,
}

// ------------------------------------------------------------- stop outcomes

/// Which owner-initiated stop an outcome describes.
///
/// It is carried rather than inferred because the two mean different things to
/// an update: a serving stop leaves the supervision parked and revivable, and a
/// final stop ends it. A snapshot taken later cannot tell them apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopRequest {
    /// [`WorkerHandle::stop_serving`]: end the child, park the supervision.
    Serving,
    /// [`WorkerHandle::stop`]: end the child and the supervision both.
    Final,
}

impl std::fmt::Display for StopRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Serving => "serving stop",
            Self::Final => "final stop",
        })
    }
}

/// Why a stop had to be forced. **Neither of these is a graceful stop**, and a
/// caller deciding whether an image may be replaced must not read them as one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Forced {
    /// The child accepted the graceful request and had not exited when the
    /// stop deadline passed, so its job was killed.
    DeadlineElapsed,
    /// The role could not ask — it supplies no graceful stop, or the request
    /// was refused or failed — so the job was killed straight away.
    GracefulRefused(String),
}

impl std::fmt::Display for Forced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeadlineElapsed => f.write_str("it was killed on the stop deadline"),
            Self::GracefulRefused(reason) => {
                write!(f, "the graceful request did not land: {reason}")
            }
        }
    }
}

/// **What a stop actually did**, named so a later snapshot cannot stand in for
/// it.
///
/// Every variant carries the request it answers and the generation it was
/// about, because "the worker is stopped" is a fact about *now* and this is a
/// fact about *this request*. It is what `stop_serving` and `stop` return and
/// what [`StatusReport::last_stop`] records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopOutcome {
    /// The child was asked, agreed, and exited on its own with this code.
    Graceful {
        request: StopRequest,
        generation: Generation,
        code: Option<i32>,
    },
    /// The child was killed. See [`Forced`].
    Forced {
        request: StopRequest,
        generation: Generation,
        why: Forced,
    },
    /// There was no child to stop — it had already exited, or none was
    /// running. **This is not evidence of a clean shutdown**: a child that
    /// died on its own may have died mid-commit, which is why the exit it
    /// left behind is carried rather than dropped.
    NotRunning {
        request: StopRequest,
        generation: Option<Generation>,
        last_exit: Option<ExitReport>,
    },
    /// The request was accepted but the supervision did not report a
    /// completed stop before the caller's patience ran out. **The supervisor
    /// still owns whatever child is left**, so the image is still in use and
    /// a caller must fail closed: a timeout is not permission to swap.
    TimedOut {
        request: StopRequest,
        generation: Option<Generation>,
    },
}

impl StopOutcome {
    pub fn request(&self) -> StopRequest {
        match self {
            Self::Graceful { request, .. }
            | Self::Forced { request, .. }
            | Self::NotRunning { request, .. }
            | Self::TimedOut { request, .. } => *request,
        }
    }

    pub fn generation(&self) -> Option<Generation> {
        match self {
            Self::Graceful { generation, .. } | Self::Forced { generation, .. } => {
                Some(*generation)
            }
            Self::NotRunning { generation, .. } | Self::TimedOut { generation, .. } => *generation,
        }
    }

    /// The child was asked, agreed, and ended itself. The only outcome that
    /// says a worker shut down on its own terms.
    pub fn was_graceful(&self) -> bool {
        matches!(self, Self::Graceful { .. })
    }

    /// **Whether the supervision has provably let go of its child**, which is
    /// the question an update asks before replacing an image.
    ///
    /// True for a graceful exit, for a kill the supervision reaped, and for a
    /// child that was already gone. False for [`StopOutcome::TimedOut`],
    /// where a child may still be running and still holding the image: a
    /// timeout is not permission to swap. It is deliberately *not* the same
    /// question as [`StopOutcome::was_graceful`] — a forced stop released the
    /// image and still did not shut the Store down on its own terms.
    pub fn child_is_gone(&self) -> bool {
        !matches!(self, Self::TimedOut { .. })
    }
}

impl std::fmt::Display for StopOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Graceful {
                request,
                generation,
                code,
            } => write!(
                f,
                "the {request} of child {generation} was graceful; it exited {}",
                code.map_or_else(|| "with no code".to_owned(), |code| code.to_string())
            ),
            Self::Forced {
                request,
                generation,
                why,
            } => write!(f, "the {request} of child {generation} was forced: {why}"),
            Self::NotRunning {
                request,
                generation,
                last_exit,
            } => match (generation, last_exit) {
                (_, Some(exit)) => write!(
                    f,
                    "the {request} found no child; child {} had already exited {}",
                    exit.generation,
                    exit.code
                        .map_or_else(|| "with no code".to_owned(), |code| code.to_string())
                ),
                (Some(generation), None) => {
                    write!(
                        f,
                        "the {request} found no child for generation {generation}"
                    )
                }
                (None, None) => write!(f, "the {request} found no child"),
            },
            Self::TimedOut {
                request,
                generation,
            } => match generation {
                Some(generation) => write!(
                    f,
                    "the {request} of child {generation} did not complete in time; the supervisor still owns it"
                ),
                None => write!(f, "the {request} did not complete in time"),
            },
        }
    }
}

/// A point-in-time answer to "what is this worker doing" — the inspectable
/// state the principles require.
#[derive(Clone, Debug)]
pub struct StatusReport {
    pub role: &'static str,
    pub state: State,
    pub generation: Option<Generation>,
    pub respawns: u64,
    pub last_exit: Option<ExitReport>,
    pub last_error: Option<String>,
    /// What the last completed owner-initiated stop did. Recorded when the
    /// stop completes, so a status read after the fact still names the
    /// operation and the generation it described.
    pub last_stop: Option<StopOutcome>,
}

/// The current child, fenced: the payload is only handed out beside the
/// generation it belongs to.
pub struct CurrentChild<P> {
    pub generation: Generation,
    pub payload: Arc<P>,
    pub pid: u32,
}

impl<P> Clone for CurrentChild<P> {
    fn clone(&self) -> Self {
        Self {
            generation: self.generation,
            payload: Arc::clone(&self.payload),
            pid: self.pid,
        }
    }
}

pub struct WorkerHandle<P> {
    tx: Sender<Msg>,
    shared: Arc<Shared<P>>,
    /// Behind a mutex so a shared handle (an `Arc<WorkerHandle>` cloned
    /// into lifecycle worker threads) can still stop and join.
    control: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl<P: Send + Sync + 'static> WorkerHandle<P> {
    pub fn status(&self) -> StatusReport {
        self.shared.snapshot.lock().expect("worker status").report()
    }

    pub fn current(&self) -> Option<CurrentChild<P>> {
        self.shared
            .snapshot
            .lock()
            .expect("worker status")
            .current
            .clone()
    }

    /// The generation fence: hand back the payload only if `generation` is
    /// still the serving child.
    pub fn issue(&self, generation: Generation) -> Result<Arc<P>, StaleGeneration> {
        let snapshot = self.shared.snapshot.lock().expect("worker status");
        match &snapshot.current {
            Some(current) if current.generation == generation => Ok(Arc::clone(&current.payload)),
            current => Err(StaleGeneration {
                requested: generation,
                current: current.as_ref().map(|child| child.generation),
            }),
        }
    }

    /// Enter the update window: suppress restart, keep a live child.
    pub fn hold(&self) {
        let _ = self.tx.send(Msg::Hold);
    }

    /// Leave the update window: a surviving child returns to service, a
    /// dead one is replaced. Also the way out of `Exhausted`.
    pub fn resume(&self) {
        let _ = self.tx.send(Msg::Resume);
    }

    /// Stop the child and park the supervision, keeping it revivable:
    /// [`WorkerHandle::resume`] spawns a fresh child with a fresh budget.
    /// This is the stop an install takes before the swap — if the install
    /// aborts, the service must come back rather than stay suppressed.
    /// Blocks until the child is provably gone; call it from a lifecycle
    /// worker, never a thread that answers a person.
    ///
    /// **The answer names what happened**, so a caller can tell a graceful
    /// exit from a kill from a timeout instead of reading a later snapshot.
    pub fn stop_serving(&self) -> StopOutcome {
        self.ask_to_stop(StopRequest::Serving)
    }

    /// Stop the worker for good and wait until its child is provably gone
    /// and the control thread has exited. Blocks; call it from a lifecycle
    /// worker, never a thread that answers a person.
    ///
    /// **On a timeout it does not then join the control thread.** The control
    /// thread only exits after it has completed this stop, so joining it after
    /// a timed-out acknowledgement is the same wait a second time with no
    /// bound at all — which is what turned finding C1's wedge into a hang.
    /// A [`StopOutcome::TimedOut`] means the supervision still owns a child;
    /// the caller fails closed on it rather than blocking on it.
    pub fn stop(&self) -> StopOutcome {
        let outcome = self.ask_to_stop(StopRequest::Final);
        if outcome.child_is_gone()
            && let Some(control) = self.control.lock().expect("worker control").take()
        {
            let _ = control.join();
        }
        outcome
    }

    fn ask_to_stop(&self, request: StopRequest) -> StopOutcome {
        let (ack, acked) = sync_channel::<StopOutcome>(1);
        let msg = match request {
            StopRequest::Serving => Msg::StopServing { ack },
            StopRequest::Final => Msg::Stop { ack },
        };
        // A closed channel is a control thread that has already exited, which
        // is a stop that already happened rather than one that timed out.
        if self.tx.send(msg).is_err() {
            return StopOutcome::NotRunning {
                request,
                generation: None,
                last_exit: self.status().last_exit,
            };
        }
        acked
            .recv_timeout(STOP_ACKNOWLEDGEMENT)
            .unwrap_or_else(|_| StopOutcome::TimedOut {
                request,
                generation: self.status().generation,
            })
    }
}

impl<P> Drop for WorkerHandle<P> {
    fn drop(&mut self) {
        let (ack, acked) = sync_channel::<StopOutcome>(1);
        let mut gone = true;
        if self.tx.send(Msg::Stop { ack }).is_ok() {
            gone = acked.recv_timeout(STOP_ACKNOWLEDGEMENT).is_ok();
        }
        // The same rule as [`WorkerHandle::stop`]: a stop that did not
        // complete leaves a control thread that has not finished, and joining
        // it here would hang a destructor.
        if gone && let Some(control) = self.control.lock().expect("worker control").take() {
            let _ = control.join();
        }
    }
}

/// How long a caller waits for a stop to complete before calling it timed out.
/// Comfortably past any role's own `stop_deadline`, because the deadline is
/// what bounds the child and this only bounds the *report*.
const STOP_ACKNOWLEDGEMENT: Duration = Duration::from_secs(30);

/// How long a waiter holds a child's exit report back for its memory watch to
/// say whether the job reached its limit first. The watch answers as soon as
/// it reads the release, which is queued behind anything the job reported, so
/// this only bounds a watch that has gone wrong.
#[cfg(windows)]
const MEMORY_WATCH_FLUSH: Duration = Duration::from_secs(2);

/// The executable and argv one spawn runs. Pure, so the one rule that matters
/// — **a sibling is a plain file name beside this executable, and gets no
/// `--worker`** — is tested without a process.
fn command_line(
    program: &Program,
    role: &str,
    own_exe: &Path,
    args: &[OsString],
) -> Result<(PathBuf, Vec<OsString>), String> {
    match program {
        Program::OwnRole => {
            let mut argv: Vec<OsString> = vec!["--worker".into(), role.into()];
            argv.extend(args.iter().cloned());
            Ok((own_exe.to_path_buf(), argv))
        }
        Program::Sibling(name) => {
            let mut components = Path::new(name).components();
            let plain = matches!(
                (components.next(), components.next()),
                (Some(Component::Normal(only)), None) if only == name.as_os_str()
            );
            if !plain {
                return Err(format!(
                    "a sibling program is named by a plain file name, not {}",
                    Path::new(name).display()
                ));
            }
            let directory = own_exe.parent().ok_or_else(|| {
                format!(
                    "{} has no directory to find a sibling program in",
                    own_exe.display()
                )
            })?;
            Ok((directory.join(name), args.to_vec()))
        }
    }
}

/// A ceiling of zero bytes could never start a child, so it is a mistake in
/// the spec rather than a limit; refused on every platform alike.
fn check_memory_limit(limit: Option<u64>) -> Result<(), String> {
    match limit {
        Some(0) => Err("a worker memory limit of zero bytes can never be met".into()),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------- internals

enum Msg {
    Ready {
        generation: u64,
    },
    ProbeFailed {
        generation: u64,
        error: String,
    },
    HealthFailed {
        generation: u64,
        error: String,
    },
    Exited {
        generation: u64,
        code: Option<i32>,
    },
    /// The child's job reported its memory limit. Sent by the memory watch
    /// **before** it lets the waiter report the exit, so it always arrives
    /// ahead of that child's `Exited`. Only a Job Object sends it.
    #[cfg_attr(not(windows), allow(dead_code))]
    MemoryLimit {
        generation: u64,
    },
    BackoffElapsed {
        epoch: u64,
    },
    StopDeadline {
        generation: u64,
    },
    /// The role's graceful-stop callback finished, on its own thread. Carries
    /// the generation it was started for, so a completion that arrives after
    /// its child was replaced is ignored rather than acted on.
    GracefulStopped {
        generation: u64,
        refused: Option<String>,
    },
    Hold,
    Resume,
    StopServing {
        ack: SyncSender<StopOutcome>,
    },
    Stop {
        ack: SyncSender<StopOutcome>,
    },
}

struct Shared<P> {
    snapshot: Mutex<Snapshot<P>>,
    current_generation: AtomicU64,
    shutdown: AtomicBool,
}

struct Snapshot<P> {
    role: &'static str,
    state: State,
    current: Option<CurrentChild<P>>,
    respawns: u64,
    last_exit: Option<ExitReport>,
    last_error: Option<String>,
    last_stop: Option<StopOutcome>,
}

impl<P> Snapshot<P> {
    fn report(&self) -> StatusReport {
        StatusReport {
            role: self.role,
            state: self.state,
            generation: self.current.as_ref().map(|child| child.generation),
            respawns: self.respawns,
            last_exit: self.last_exit,
            last_error: self.last_error.clone(),
            last_stop: self.last_stop.clone(),
        }
    }
}

/// The living child as the control thread holds it: pipes and job beside
/// the public payload. Dropping this closes the bootstrap pipe (the
/// cross-platform parent-death signal) and kills anything still alive in the
/// tree — through the job handle on Windows, and through the worker's own
/// process group elsewhere, which is the nearest thing unix has (M24).
struct LiveChild {
    // Never read on Windows: held so dropping it closes the bootstrap
    // pipe, which is the role contract's parent-death signal.
    #[cfg_attr(windows, allow(dead_code))]
    stdin: Option<std::process::ChildStdin>,
    #[cfg(windows)]
    job: crate::windows_job::Job,
    // The kill target where there is no job to kill.
    #[cfg_attr(windows, allow(dead_code))]
    pid: u32,
}

impl LiveChild {
    fn kill(&mut self) {
        #[cfg(windows)]
        self.job.kill();
        #[cfg(not(windows))]
        {
            // No job objects here: close the parent pipe (the role contract
            // is to exit on EOF) and kill the worker's whole process group,
            // which it leads from spawn — so a grandchild dies with it, as the
            // Job Object makes it on Windows.
            self.stdin.take();
            crate::kill_group(self.pid);
        }
    }
}

/// The unix half of what closing the job handle does on Windows: whatever a
/// worker left behind in its group dies when the supervisor lets go of it,
/// whether the worker exited on its own or was killed.
#[cfg(not(windows))]
impl Drop for LiveChild {
    fn drop(&mut self) {
        self.stdin.take();
        crate::kill_group(self.pid);
    }
}

/// Start supervising one role. Returns immediately; the first spawn happens
/// on the control thread.
pub fn start<P: Send + Sync + 'static>(spec: WorkerSpec<P>) -> WorkerHandle<P> {
    if let Some(limit_bytes) = spec.memory_limit
        && !MEMORY_LIMIT_ENFORCED
    {
        // News, once per supervision rather than once per spawn: the caller
        // asked for a ceiling this platform does not give.
        tracing::warn!(
            target: "koru::worker",
            role = spec.role,
            limit_bytes,
            "this platform enforces no worker memory limit; the child runs unbounded"
        );
    }
    let (tx, rx) = channel::<Msg>();
    let shared = Arc::new(Shared {
        snapshot: Mutex::new(Snapshot {
            role: spec.role,
            state: State::Starting,
            current: None,
            respawns: 0,
            last_exit: None,
            last_error: None,
            last_stop: None,
        }),
        current_generation: AtomicU64::new(0),
        shutdown: AtomicBool::new(false),
    });
    let control = {
        let tx = tx.clone();
        let shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name(format!("koru-worker {}", spec.role))
            .spawn(move || Control::new(spec, tx, rx, shared).run())
            .expect("spawn worker control thread")
    };
    WorkerHandle {
        tx,
        shared,
        control: Mutex::new(Some(control)),
    }
}

/// What the control thread remembers about the stop it is currently running,
/// so that the outcome it reports describes *this* request rather than
/// whatever the supervision happens to look like when it completes.
struct StopInFlight {
    request: StopRequest,
    ack: SyncSender<StopOutcome>,
    /// The child that was serving when the request arrived, and whether there
    /// was one at all. `None` is C1's case: nothing to stop.
    generation: Option<Generation>,
    /// Set when the child had to be killed, and why.
    forced: Option<Forced>,
}

struct Control<P> {
    spec: WorkerSpec<P>,
    /// The role's probes, shared with the prober and health threads.
    readiness: SharedProbe<P>,
    health: Option<SharedProbe<P>>,
    /// **Shared, because it no longer runs here.** C2: the graceful request
    /// runs on its own thread so the control loop can service its own
    /// deadline and every other message while a role's callback is stalled.
    graceful_stop: Option<SharedProbe<P>>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    shared: Arc<Shared<P>>,
    machine: Machine,
    generation: u64,
    backoff_epoch: u64,
    live: Option<LiveChild>,
    /// At most one owner stop is in flight; a second `Stop` while one is
    /// running replaces the acknowledgement it will answer.
    stopping: Option<StopInFlight>,
    /// Whether the in-flight stop was the final one, which is what ends this
    /// thread. Kept beside `stopping` because the answer is needed after it
    /// has been taken.
    final_stop: bool,
}

impl<P: Send + Sync + 'static> Control<P> {
    fn new(
        mut spec: WorkerSpec<P>,
        tx: Sender<Msg>,
        rx: Receiver<Msg>,
        shared: Arc<Shared<P>>,
    ) -> Self {
        let machine = Machine::starting(spec.restart_budget, spec.backoff);
        let readiness: SharedProbe<P> = Arc::from(std::mem::replace(
            &mut spec.readiness,
            Box::new(|_| Err("readiness moved to the control thread".into())),
        ));
        let health = spec
            .health
            .take()
            .map(|health| -> SharedProbe<P> { Arc::from(health) });
        let graceful_stop = spec
            .graceful_stop
            .take()
            .map(|stop| -> SharedProbe<P> { Arc::from(stop) });
        Self {
            spec,
            readiness,
            health,
            graceful_stop,
            tx,
            rx,
            shared,
            machine,
            generation: 0,
            backoff_epoch: 0,
            live: None,
            stopping: None,
            final_stop: false,
        }
    }

    fn run(mut self) {
        self.spawn_child();
        loop {
            let Ok(msg) = self.rx.recv() else { break };
            let action = match msg {
                Msg::Ready { generation } if generation == self.generation => {
                    self.notify(WorkerEventKind::Ready);
                    tracing::info!(
                        target: "koru::worker",
                        role = self.spec.role,
                        generation = self.generation,
                        "worker is ready"
                    );
                    self.machine.on(Event::Ready, Instant::now())
                }
                Msg::ProbeFailed { generation, error } if generation == self.generation => {
                    self.record_error(&error);
                    self.notify(WorkerEventKind::ProbeFailed { error });
                    self.machine.on(Event::Unhealthy, Instant::now())
                }
                Msg::HealthFailed { generation, error } if generation == self.generation => {
                    self.record_error(&error);
                    self.notify(WorkerEventKind::HealthFailed { error });
                    self.machine.on(Event::Unhealthy, Instant::now())
                }
                Msg::Exited { generation, code } if generation == self.generation => {
                    self.reap(code);
                    self.machine.on(Event::Exited, Instant::now())
                }
                Msg::MemoryLimit { generation } if generation == self.generation => {
                    // Windows refused the commit that would have crossed the
                    // ceiling; it did not end the process. End the tree here,
                    // so a child that survives its refused allocation is not
                    // left running at its limit. The exit that follows the
                    // kill is an ordinary crash to the machine — the event
                    // is what tells it apart.
                    let limit_bytes = self.spec.memory_limit.unwrap_or(0);
                    self.record_error(&format!(
                        "child {generation} reached its memory limit of {limit_bytes} bytes and was ended"
                    ));
                    self.notify(WorkerEventKind::MemoryLimitReached { limit_bytes });
                    tracing::warn!(
                        target: "koru::worker",
                        role = self.spec.role,
                        generation,
                        limit_bytes,
                        "the worker reached its memory limit and was ended"
                    );
                    if let Some(live) = self.live.as_mut() {
                        live.kill();
                    }
                    Action::None
                }
                Msg::BackoffElapsed { epoch } if epoch == self.backoff_epoch => {
                    self.machine.on(Event::BackoffElapsed, Instant::now())
                }
                Msg::StopDeadline { generation } if generation == self.generation => {
                    // The graceful stop ran out of patience; the exit event
                    // that follows the kill finishes the stop. The deadline is
                    // armed *before* the role is asked (C2), so a callback
                    // that never returns cannot postpone it.
                    if let Some(live) = self.live.as_mut() {
                        live.kill();
                        if let Some(stopping) = self.stopping.as_mut() {
                            stopping.forced.get_or_insert(Forced::DeadlineElapsed);
                        }
                        tracing::warn!(
                            target: "koru::worker",
                            role = self.spec.role,
                            generation = self.generation,
                            "the worker did not stop before its deadline and was killed"
                        );
                    }
                    Action::None
                }
                Msg::GracefulStopped {
                    generation,
                    refused,
                } if generation == self.generation => {
                    // The role's callback finished for the child that is still
                    // current. A refusal is killed now rather than waited out;
                    // an accepted request leaves the deadline standing.
                    match refused {
                        Some(reason) => {
                            self.record_error(&reason);
                            if let Some(stopping) = self.stopping.as_mut() {
                                stopping
                                    .forced
                                    .get_or_insert(Forced::GracefulRefused(reason));
                            }
                            if let Some(live) = self.live.as_mut() {
                                live.kill();
                            }
                        }
                        // Routine, once per stop: the transcript, not news.
                        None => tracing::debug!(
                            target: "koru::worker",
                            role = self.spec.role,
                            generation,
                            "the graceful stop was accepted"
                        ),
                    }
                    Action::None
                }
                Msg::Hold => {
                    self.notify(WorkerEventKind::Held);
                    self.machine.on(Event::Hold, Instant::now())
                }
                Msg::Resume => {
                    self.notify(WorkerEventKind::Resumed);
                    self.machine.on(Event::Resume, Instant::now())
                }
                Msg::StopServing { ack } => self.begin_stop(StopRequest::Serving, ack),
                Msg::Stop { ack } => self.begin_stop(StopRequest::Final, ack),
                // A message from a child that is no longer current, or a
                // stale timer. Nothing to do.
                _ => Action::None,
            };
            self.perform(action);
            self.publish_state();
            if self.machine.state() == State::Stopped
                && let Some(stopping) = self.stopping.take()
            {
                self.live = None;
                let outcome = self.finished(&stopping);
                tracing::info!(
                    target: "koru::worker",
                    role = self.spec.role,
                    outcome = %outcome,
                    "worker stopped"
                );
                {
                    let mut snapshot = self.shared.snapshot.lock().expect("worker status");
                    snapshot.last_stop = Some(outcome.clone());
                }
                self.notify(WorkerEventKind::Stopped);
                let _ = stopping.ack.send(outcome);
                if self.final_stop {
                    // A final stop ends the supervision with it. A serving
                    // stop leaves this loop running, parked and revivable.
                    self.shared.shutdown.store(true, Ordering::Release);
                    break;
                }
            }
        }
    }

    /// Record a stop request and feed it to the machine.
    ///
    /// The generation is captured **now**, at the moment the request arrives,
    /// because that is the child the answer will be about. `None` means there
    /// was nothing to stop — C1's case, and the one where the outcome must
    /// say `NotRunning` rather than imply a clean shutdown.
    fn begin_stop(&mut self, request: StopRequest, ack: SyncSender<StopOutcome>) -> Action {
        let generation = self
            .machine
            .child_alive()
            .then_some(Generation(self.generation));
        if let Some(previous) = self.stopping.replace(StopInFlight {
            request,
            ack,
            generation,
            forced: None,
        }) {
            // A second stop while one is running: the first caller is told
            // what is true rather than left waiting on an acknowledgement
            // that will now be sent to somebody else.
            let _ = previous.ack.send(StopOutcome::TimedOut {
                request: previous.request,
                generation: previous.generation,
            });
        }
        self.final_stop |= request == StopRequest::Final;
        self.machine.on(Event::Stop, Instant::now())
    }

    /// What this stop actually did — the fact about *this request*, built from
    /// what the control thread observed while it ran.
    fn finished(&self, stopping: &StopInFlight) -> StopOutcome {
        let last_exit = self
            .shared
            .snapshot
            .lock()
            .expect("worker status")
            .last_exit;
        let Some(generation) = stopping.generation else {
            // **Absence of a child is never evidence of a clean Store join.**
            return StopOutcome::NotRunning {
                request: stopping.request,
                generation: None,
                last_exit,
            };
        };
        match &stopping.forced {
            Some(why) => StopOutcome::Forced {
                request: stopping.request,
                generation,
                why: why.clone(),
            },
            None => StopOutcome::Graceful {
                request: stopping.request,
                generation,
                code: last_exit
                    .filter(|exit| exit.generation == generation)
                    .and_then(|exit| exit.code),
            },
        }
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::Spawn => self.spawn_child(),
            Action::Kill => {
                if let Some(live) = self.live.as_mut() {
                    live.kill();
                }
            }
            Action::StopChild => self.stop_child(),
            Action::ScheduleBackoff(delay) => {
                self.backoff_epoch += 1;
                self.notify(WorkerEventKind::RestartScheduled { delay });
                tracing::warn!(
                    target: "koru::worker",
                    role = self.spec.role,
                    delay_ms = delay.as_millis() as u64,
                    "worker exited; restart scheduled"
                );
                let tx = self.tx.clone();
                let epoch = self.backoff_epoch;
                std::thread::Builder::new()
                    .name("koru-worker backoff".into())
                    .spawn(move || {
                        std::thread::sleep(delay);
                        let _ = tx.send(Msg::BackoffElapsed { epoch });
                    })
                    .expect("spawn backoff timer");
            }
        }
        if self.machine.state() == State::Exhausted {
            self.notify(WorkerEventKind::Exhausted);
            tracing::error!(
                target: "koru::worker",
                role = self.spec.role,
                "worker restart budget is exhausted; not restarting"
            );
        }
    }

    fn spawn_child(&mut self) {
        self.generation += 1;
        let generation = Generation(self.generation);
        self.shared
            .current_generation
            .store(self.generation, Ordering::Release);
        match self.try_spawn(generation) {
            Ok(()) => {}
            Err(error) => {
                self.record_error(&error);
                self.notify(WorkerEventKind::SpawnFailed { error });
                // The spawn itself failed: no child, no waiter. Feed the
                // exit directly so the machine backs off or exhausts.
                let _ = self.tx.send(Msg::Exited {
                    generation: self.generation,
                    code: None,
                });
            }
        }
    }

    fn try_spawn(&mut self, generation: Generation) -> Result<(), String> {
        check_memory_limit(self.spec.memory_limit)?;
        let plan = (self.spec.launch)(generation)?;
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let (program, argv) = command_line(&self.spec.program, self.spec.role, &exe, &plan.args)?;
        if matches!(self.spec.program, Program::Sibling(_)) && !program.is_file() {
            // Said plainly rather than left to the OS's "file not found",
            // which would not say where it looked.
            return Err(format!(
                "the sibling program {} is not beside {}",
                program.display(),
                exe.display()
            ));
        }
        let mut command = Command::new(program);
        command.args(argv).stdin(Stdio::piped());
        // The worker leads a process group of its own, so its tree can be
        // killed as one — and it is told it may do the same on parent loss.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0).env(crate::OWNS_GROUP_ENV, "1");
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("could not spawn worker: {error}"))?;
        let pid = child.id();
        // The child is in its job — ceiling included — before the bootstrap is
        // written, and a worker does nothing before it has read that line.
        #[cfg(windows)]
        let job = {
            let job = crate::windows_job::Job::new(self.spec.memory_limit)?;
            job.assign(&child)?;
            job
        };
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "the worker has no bootstrap pipe".to_owned())?;
        stdin
            .write_all(&plan.bootstrap)
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush())
            .map_err(|error| {
                let _ = child.kill();
                let _ = child.wait();
                format!("could not deliver worker bootstrap: {error}")
            })?;

        let payload = Arc::new(plan.payload);
        // Taken before the job moves into the live child.
        #[cfg(windows)]
        let memory_watch = job.memory_watch();
        self.live = Some(LiveChild {
            stdin: Some(stdin),
            #[cfg(windows)]
            job,
            pid,
        });
        {
            let mut snapshot = self.shared.snapshot.lock().expect("worker status");
            if self.generation > 1 {
                snapshot.respawns += 1;
            }
            snapshot.current = Some(CurrentChild {
                generation,
                payload: Arc::clone(&payload),
                pid,
            });
        }

        let raw_generation = self.generation;

        // **The memory watch, when the job has a ceiling.** It blocks on the
        // job's completion port; a limit reported there becomes a message to
        // this thread, which ends the tree. It is released by the waiter once
        // the child has exited, and only *then* acknowledges — so a limit the
        // job queued before the exit is always reported before the exit.
        #[cfg(windows)]
        let flush = memory_watch.map(|watch| {
            let (flushed, on_flushed) = sync_channel::<()>(1);
            let releaser = watch.clone();
            let tx = self.tx.clone();
            std::thread::Builder::new()
                .name("koru-worker memory".into())
                .stack_size(64 * 1024)
                .spawn(move || {
                    if watch.wait() {
                        let _ = tx.send(Msg::MemoryLimit {
                            generation: raw_generation,
                        });
                        // Further reports for the same child say nothing new.
                        while watch.wait() {}
                    }
                    let _ = flushed.send(());
                })
                .expect("spawn worker memory watch");
            (releaser, on_flushed)
        });

        // The waiter owns the Child; its message is the only death report.
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("koru-worker waiter".into())
            .spawn(move || {
                let code = child.wait().ok().and_then(|status| status.code());
                #[cfg(windows)]
                if let Some((releaser, on_flushed)) = flush {
                    releaser.release();
                    // Bounded: a watch that cannot answer must not hold the
                    // death report back for ever.
                    let _ = on_flushed.recv_timeout(MEMORY_WATCH_FLUSH);
                }
                let _ = tx.send(Msg::Exited {
                    generation: raw_generation,
                    code,
                });
            })
            .expect("spawn worker waiter");

        // The prober speaks only for its own generation.
        let tx = self.tx.clone();
        let shared_generation = Arc::clone(&self.shared);
        let readiness_payload = Arc::clone(&payload);
        let deadline = Instant::now() + self.spec.ready_deadline;
        let poll = self.spec.ready_poll;
        let probe = Arc::clone(&self.readiness);
        std::thread::Builder::new()
            .name("koru-worker prober".into())
            .spawn(move || {
                loop {
                    if shared_generation.current_generation.load(Ordering::Acquire)
                        != raw_generation
                    {
                        return;
                    }
                    let error = match probe(&readiness_payload) {
                        Ok(()) => {
                            let _ = tx.send(Msg::Ready {
                                generation: raw_generation,
                            });
                            return;
                        }
                        Err(error) => error,
                    };
                    if Instant::now() >= deadline {
                        let _ = tx.send(Msg::ProbeFailed {
                            generation: raw_generation,
                            error: format!("worker was not ready in time: {error}"),
                        });
                        return;
                    }
                    std::thread::sleep(poll);
                }
            })
            .expect("spawn worker prober");

        // Health, if the role wants it, on the same speak-only-for-your-
        // generation terms.
        if let Some(health) = self.health.as_ref().map(Arc::clone) {
            let tx = self.tx.clone();
            let shared = Arc::clone(&self.shared);
            let payload = Arc::clone(&payload);
            let interval = self.spec.health_interval;
            std::thread::Builder::new()
                .name("koru-worker health".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(interval);
                        if shared.shutdown.load(Ordering::Acquire)
                            || shared.current_generation.load(Ordering::Acquire) != raw_generation
                        {
                            return;
                        }
                        if let Err(error) = health(&payload) {
                            let _ = tx.send(Msg::HealthFailed {
                                generation: raw_generation,
                                error,
                            });
                            return;
                        }
                    }
                })
                .expect("spawn worker health");
        }
        Ok(())
    }

    /// **C2: the deadline covers the graceful request, and the guard runs off
    /// the thread it guards.**
    ///
    /// The order here is the correction. The role's callback used to be called
    /// inline, on this control thread, with the deadline armed only after it
    /// returned — so a stalled callback postponed its own alarm *and* every
    /// other control message: a health failure, an exit, a resume, the next
    /// stop. That is the guard running on the thread it guards, which
    /// PRINCIPLES names explicitly.
    ///
    /// Now: arm the deadline first, then run the request on its own thread and
    /// let its completion come back as a message. Three consequences worth
    /// saying out loud:
    ///
    /// - The control loop keeps answering while a role's callback is stuck,
    ///   and the deadline still fires and still kills the child.
    /// - **A late completion is fenced by generation.** The callback may now
    ///   outlive the child it was started for, so its answer is only acted on
    ///   while that child is still current.
    /// - Because it can outlive its child, the callback itself must be bound
    ///   to that child where it acts. That is the adapter's job — for the
    ///   gateway, `gateway_host::ask_the_checked_child_to_stop` — and this
    ///   correction is what makes that rule necessary rather than theoretical.
    fn stop_child(&mut self) {
        let payload = self
            .shared
            .snapshot
            .lock()
            .expect("worker status")
            .current
            .as_ref()
            .map(|child| Arc::clone(&child.payload));
        let (Some(stop), Some(payload)) = (self.graceful_stop.as_ref().map(Arc::clone), payload)
        else {
            // Nothing to ask, or nobody to ask: the kill is the whole stop,
            // and it is recorded as forced rather than passed off as graceful.
            if let Some(stopping) = self.stopping.as_mut() {
                stopping
                    .forced
                    .get_or_insert(Forced::GracefulRefused(match self.graceful_stop {
                        Some(_) => "the child was already gone".into(),
                        None => "the role supplies no graceful stop".into(),
                    }));
            }
            if let Some(live) = self.live.as_mut() {
                live.kill();
            }
            return;
        };

        // **Armed before the ask**, so a callback that never returns cannot
        // delay it.
        let generation = self.generation;
        let tx = self.tx.clone();
        let deadline = self.spec.stop_deadline;
        std::thread::Builder::new()
            .name("koru-worker stop deadline".into())
            .spawn(move || {
                std::thread::sleep(deadline);
                let _ = tx.send(Msg::StopDeadline { generation });
            })
            .expect("spawn stop deadline");

        // The ask, off this thread. It may outlive its child; the completion
        // says which generation it was about and is ignored if that child has
        // been replaced.
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("koru-worker graceful stop".into())
            .spawn(move || {
                let refused = stop(&payload).err();
                let _ = tx.send(Msg::GracefulStopped {
                    generation,
                    refused,
                });
            })
            .expect("spawn graceful stop");
    }

    fn reap(&mut self, code: Option<i32>) {
        self.live = None;
        let generation = Generation(self.generation);
        {
            let mut snapshot = self.shared.snapshot.lock().expect("worker status");
            snapshot.current = None;
            snapshot.last_exit = Some(ExitReport { generation, code });
        }
        self.shared.current_generation.store(0, Ordering::Release);
        self.notify(WorkerEventKind::Exited { code });
    }

    fn record_error(&self, error: &str) {
        let mut snapshot = self.shared.snapshot.lock().expect("worker status");
        snapshot.last_error = Some(error.to_owned());
    }

    fn publish_state(&self) {
        let mut snapshot = self.shared.snapshot.lock().expect("worker status");
        snapshot.state = self.machine.state();
    }

    fn notify(&self, kind: WorkerEventKind) {
        if let Some(on_event) = self.spec.on_event.as_ref() {
            on_event(WorkerEvent {
                role: self.spec.role,
                generation: Generation(self.generation),
                kind,
            });
        }
    }
}

#[cfg(test)]
mod tests;
