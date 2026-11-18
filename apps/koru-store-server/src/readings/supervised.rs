// SPDX-License-Identifier: MPL-2.0

//! **The real launcher: `koru-read-worker`, supervised by this daemon**
//! (TECHNICAL D4, D15, §10 "Worker launch").
//!
//! The worker parses untrusted files, so it runs as its own program: resolved
//! beside this executable as `koru_update::READ_EXE` (never through `PATH`),
//! started as `koru-read-worker --root <store dir> --supervised` through
//! `koru-agent-worker`'s supervisor, in a kill-on-close Job Object with a
//! memory ceiling, and handed one bootstrap line on stdin whose pipe then
//! stays open as the parent-death signal.
//!
//! ## Three things this does that the supervisor does not
//!
//! - **It checks the executable is there before starting anything.** A
//!   missing file would make every spawn a `SpawnFailed` and exhaust the
//!   restart budget for nothing; instead the coordinator is told "no reader"
//!   ([`Unavailable::NotInstalled`]) and jobs wait (TECHNICAL §11).
//! - **It stops the supervision when the coordinator's queue empties**, and
//!   starts a fresh one on the next job. The supervisor treats *any* exit as a
//!   crash and restarts it against a budget of five in five minutes, and the
//!   worker exits by itself when idle (a minute by default,
//!   `KORU_READ_IDLE_EXIT`); left up, an idle worker would be "crashed" into
//!   an exhausted budget. The coordinator lingers [`LINGER`] — shorter than
//!   the worker's own idle exit — before it lets go.
//! - **It tells a hostile file from a crash.** Exit code 3 is the worker's own
//!   watchdog ending it on a file that wedged it ([`Interrupted::Hostile`]);
//!   a death after the Job Object reported its ceiling is a memory kill.
//!
//! ## Where the memory limit is not enforced
//!
//! `koru_agent_worker::supervisor::MEMORY_LIMIT_ENFORCED` is false off
//! Windows: the supervisor accepts the ceiling, logs once that it cannot
//! apply it, and the child runs unbounded. **This daemon runs the worker
//! anyway**, because a text-layer PDF and a plain-text file are promised on
//! Linux (TECHNICAL §12) and the worker's own per-page time budget still
//! applies. A file that makes it die there — the kernel's out-of-memory
//! killer included — is an ordinary crash to the coordinator: recorded
//! locally, one retry, then left. `WorkerReport::memory_limit_enforced` says
//! which applies on this host.
//!
//! ## Below the person's work
//!
//! The worker is launched with `Priority::Background`: below normal priority
//! from its first instruction, and background I/O once it has read its
//! bootstrap (`koru_agent_worker::follow_launch_priority`, OCR Plan 3.1).
//! Windows only for now (`koru_agent_worker::supervisor::PRIORITY_LOWERED`);
//! elsewhere it runs at the Store's priority.
//!
//! ## Provisional, from Plan 0.1's measurements
//!
//! [`MEMORY_LIMIT_BYTES`] is 1 GiB: a text-layer job peaked at 180 MiB over
//! 721 real PDFs, recognition (a later plan) at about 600 MiB. The worker's
//! per-page time budget is 30 s, so [`JOB_PATIENCE`] — how long a job may go
//! without saying anything before the worker is taken for hung — is well past
//! it.
//!
//! ## The worker's half of the same names
//!
//! The endpoint is `koru_ipc::READ` qualified by the store root, and the
//! bootstrap line is `koru_read::WorkerBootstrap` — the worker binds and reads
//! exactly those (`apps/koru-read/src/{server,bootstrap}.rs`), so neither side
//! spells either for itself.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use koru_agent_worker::machine::State;
use koru_agent_worker::supervisor::{
    self, Generation, LaunchPlan, MEMORY_LIMIT_ENFORCED, Priority, Program, WorkerEventKind,
    WorkerHandle, WorkerSpec,
};
use koru_read::{
    BOOTSTRAP_VERSION, Event, FRAME_VERSION, PROTOCOL_VERSION, Request, WorkerBootstrap,
    WorkerStatus,
};

use super::launcher::{Availability, Interrupted, Launcher, Unavailable, WorkerReport};

/// The ceiling on the worker's committed memory (Plan 0.1: 1 GiB).
pub const MEMORY_LIMIT_BYTES: u64 = 1 << 30;

/// How long a job may say nothing before the worker is taken for hung. The
/// worker's own per-page budget is 30 s (Plan 0.1); a page that runs out of
/// it ends the worker, which this sees as the connection closing long before.
///
/// **Kept by [`patiently`], not by the socket**: a Windows named pipe has no
/// read timeout, so this is the only thing that ends a job whose worker is
/// alive and silent.
pub const JOB_PATIENCE: Duration = Duration::from_secs(120);

/// How long the coordinator keeps an idle worker before stopping its
/// supervision. Shorter than the worker's own idle exit (60 s), so the
/// supervisor never sees an idle exit and counts it as a crash.
pub const LINGER: Duration = Duration::from_secs(30);

/// How long a freshly started worker has to answer its first `Status`.
const READY_DEADLINE: Duration = Duration::from_secs(10);

/// The deadline on a job's `Status` handshake (kept by [`patiently`]), and the
/// socket timeouts the readiness probe asks for — which apply off Windows
/// only; there a probe that never hears back is ended by [`READY_DEADLINE`].
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait, after a job's connection closed, for the supervisor to
/// report how the child ended — so a memory kill and a watchdog exit are told
/// apart from an ordinary crash.
const EXIT_REPORT_WAIT: Duration = Duration::from_secs(3);

/// The worker's exit code when its own watchdog ends it on a file that
/// wedged it (Plan 1.3).
const HOSTILE_EXIT: i32 = 3;

/// One child's identity, as the supervisor hands it to the probes.
pub struct ReadChild {
    launch_id: String,
    endpoint: koru_ipc::Endpoint,
}

/// What the supervisor said happened, kept for the job that is running.
#[derive(Default)]
struct Observed {
    memory_kills: AtomicU64,
    /// The generation whose child last reached its memory limit.
    memory_generation: AtomicU64,
    /// `(generation, exit code)` of the last exit.
    last_exit: Mutex<Option<(u64, Option<i32>)>>,
}

pub struct SupervisedReader {
    root: PathBuf,
    channel: String,
    handle: Option<WorkerHandle<ReadChild>>,
    observed: Arc<Observed>,
    respawns_before: u64,
    /// The installed model's directory, passed as `--model`, or `None`.
    model: Option<PathBuf>,
    /// The current worker's answer to its last `Status`.
    last_status: Option<WorkerStatus>,
}

impl SupervisedReader {
    pub fn new(root: &Path, channel: &str) -> Self {
        Self {
            root: root.to_path_buf(),
            channel: channel.to_owned(),
            handle: None,
            observed: Arc::new(Observed::default()),
            respawns_before: 0,
            model: None,
            last_status: None,
        }
    }

    /// The executable is beside this one. One `stat`, on the coordinator's
    /// thread.
    fn installed() -> bool {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join(koru_update::READ_EXE)))
            .is_some_and(|path| path.is_file())
    }

    fn endpoint(&self) -> Result<koru_ipc::Endpoint, String> {
        koru_ipc::Endpoint::for_service_root(koru_ipc::READ, &self.channel, &self.root)
            .map_err(|e| e.to_string())
    }

    fn start(&mut self) -> Result<&WorkerHandle<ReadChild>, Unavailable> {
        if self.handle.is_none() {
            let endpoint = self.endpoint().map_err(Unavailable::NotReady)?;
            self.handle = Some(supervisor::start(spec(
                self.root.clone(),
                self.channel.clone(),
                endpoint,
                Arc::clone(&self.observed),
                self.model.clone(),
            )));
        }
        Ok(self.handle.as_ref().expect("just started"))
    }

    /// Stop the supervision and let the child go.
    fn stop(&mut self) {
        self.last_status = None;
        if let Some(handle) = self.handle.take() {
            self.respawns_before += handle.status().respawns;
            let _ = handle.stop();
        }
    }

    /// Wait until the supervisor says the current child is ready, at most
    /// [`READY_DEADLINE`]. On the coordinator's thread, which answers nobody.
    fn ready_child(&mut self) -> Result<(Generation, String, koru_ipc::Endpoint), Unavailable> {
        let deadline = Instant::now() + READY_DEADLINE;
        let handle = self.start()?;
        let late = loop {
            let status = handle.status();
            match status.state {
                State::Ready => {
                    if let Some(child) = handle.current() {
                        return Ok((
                            child.generation,
                            child.payload.launch_id.clone(),
                            child.payload.endpoint.clone(),
                        ));
                    }
                }
                State::Exhausted => return Err(Unavailable::Exhausted),
                State::Stopped => {
                    return Err(Unavailable::NotReady("the supervision stopped".into()));
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                break status
                    .last_error
                    .unwrap_or_else(|| "the worker was not ready in time".into());
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        // **Let it go, so the next job starts a fresh one.** The supervisor's
        // readiness probe asks the worker's `Status` over a named pipe, and a
        // Windows named pipe has no read timeout: a worker that accepted and
        // never answered would hold the probe — and this supervision, in
        // `Starting` — for ever. Ending the supervision ends the child, and the
        // probe's read with it.
        self.stop();
        Err(Unavailable::NotReady(late))
    }

    /// The child of `generation` is gone: say how it went.
    fn ended(&mut self, generation: Generation, hung: bool) -> Interrupted {
        let deadline = Instant::now() + EXIT_REPORT_WAIT;
        loop {
            if self.observed.memory_generation.load(Ordering::Acquire) == generation.0 {
                return Interrupted::Crashed { memory_limit: true };
            }
            let exit = *self
                .observed
                .last_exit
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some((g, code)) = exit
                && g == generation.0
            {
                return if code == Some(HOSTILE_EXIT) {
                    Interrupted::Hostile
                } else {
                    Interrupted::Crashed {
                        memory_limit: false,
                    }
                };
            }
            if hung || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        // Hung, or never reported: end it ourselves, so the next job starts a
        // fresh worker rather than queueing behind a wedged one.
        self.stop();
        Interrupted::Crashed {
            memory_limit: false,
        }
    }
}

impl Launcher for SupervisedReader {
    fn availability(&mut self) -> Availability {
        if !Self::installed() {
            self.stop();
            return Availability::Unavailable(Unavailable::NotInstalled);
        }
        match self.handle.as_ref().map(|h| h.status().state) {
            Some(State::Exhausted) => Availability::Unavailable(Unavailable::Exhausted),
            _ => Availability::Ready,
        }
    }

    fn read(
        &mut self,
        request: &Request,
        events: &mut dyn FnMut(Event),
    ) -> Result<(), Interrupted> {
        if !Self::installed() {
            return Err(Interrupted::Unavailable(Unavailable::NotInstalled));
        }
        let (generation, launch_id, endpoint) =
            self.ready_child().map_err(Interrupted::Unavailable)?;
        // No socket timeouts: they do not exist on a Windows named pipe (see
        // `patiently`), so the patience is the conversation thread's, the same
        // on every platform.
        let stream = match koru_ipc::connect(&endpoint) {
            Ok(stream) => stream,
            Err(_) => return Err(self.ended(generation, false)),
        };
        let mut learned: Option<WorkerStatus> = None;
        let outcome = patiently_with(
            stream,
            launch_id,
            Some(request.clone()),
            HANDSHAKE_TIMEOUT,
            JOB_PATIENCE,
            &mut |status| learned = Some(status),
            events,
        );
        if learned.is_some() {
            // What this launch says it can do (OCR Plan 2.4).
            self.last_status = learned;
        }
        match outcome {
            Ok(()) => Ok(()),
            Err(Broke::Handshake(Refusal::Wrong(why))) => {
                // Not the process this supervision launched, or another
                // build's: never send it a file.
                self.stop();
                Err(Interrupted::Unavailable(Unavailable::NotReady(why)))
            }
            Err(
                Broke::Handshake(Refusal::Closed { hung }) | Broke::Job(Refusal::Closed { hung }),
            ) => Err(self.ended(generation, hung)),
            Err(Broke::Job(Refusal::Wrong(_))) => Err(self.ended(generation, false)),
        }
    }

    fn idle(&mut self) {
        self.stop();
    }

    fn set_model(&mut self, model: Option<PathBuf>) {
        if self.model != model {
            self.model = model;
            // The next job starts a worker with the new `--model`.
            self.stop();
        }
    }

    fn probe(&mut self) -> Result<WorkerStatus, Unavailable> {
        if !Self::installed() {
            return Err(Unavailable::NotInstalled);
        }
        let (generation, launch_id, endpoint) = self.ready_child()?;
        // The handshake alone, through the same patient conversation as a job
        // (`patiently_with`): a silent worker is given up on, never waited for.
        let stream =
            koru_ipc::connect(&endpoint).map_err(|e| Unavailable::NotReady(e.to_string()))?;
        let mut learned: Option<WorkerStatus> = None;
        let outcome = patiently_with(
            stream,
            launch_id,
            None,
            HANDSHAKE_TIMEOUT,
            JOB_PATIENCE,
            &mut |status| learned = Some(status),
            &mut |_| {},
        );
        match (outcome, learned) {
            (Ok(()), Some(status)) => {
                self.last_status = Some(status.clone());
                Ok(status)
            }
            (Err(Broke::Handshake(Refusal::Wrong(why))), _) => {
                self.stop();
                Err(Unavailable::NotReady(why))
            }
            (Err(Broke::Handshake(Refusal::Closed { hung: true })), _) => {
                // Alive and silent: end it, so the next start is a fresh one.
                let _ = self.ended(generation, true);
                Err(Unavailable::NotReady(
                    "the read worker did not answer its status in time".into(),
                ))
            }
            _ => Err(Unavailable::NotReady(
                "the read worker closed the connection".into(),
            )),
        }
    }

    fn status(&self) -> Option<WorkerStatus> {
        self.last_status.clone()
    }

    fn report(&self) -> WorkerReport {
        let status = self.handle.as_ref().map(|h| h.status());
        let up = status.as_ref().is_some_and(|s| s.state == State::Ready);
        let why_not = if up {
            None
        } else if !Self::installed() {
            Some(Unavailable::NotInstalled.code())
        } else {
            match status.as_ref().map(|s| s.state) {
                Some(State::Exhausted) => Some(Unavailable::Exhausted.code()),
                Some(State::Starting | State::Backoff) => Some("starting"),
                // No supervision: nothing to read, or it was let go when the
                // queue emptied. It starts on the next job.
                _ => Some("idle"),
            }
        };
        WorkerReport {
            up,
            why_not,
            generation: status.as_ref().and_then(|s| s.generation).map(|g| g.0),
            respawns: self.respawns_before + status.as_ref().map_or(0, |s| s.respawns),
            memory_kills: self.observed.memory_kills.load(Ordering::Relaxed),
            last_exit_code: self
                .observed
                .last_exit
                .lock()
                .ok()
                .and_then(|exit| exit.and_then(|(_, code)| code)),
            memory_limit_bytes: Some(MEMORY_LIMIT_BYTES),
            memory_limit_enforced: MEMORY_LIMIT_ENFORCED,
        }
    }
}

impl Drop for SupervisedReader {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The supervisor's whole knowledge of the read worker.
fn spec(
    root: PathBuf,
    channel: String,
    endpoint: koru_ipc::Endpoint,
    observed: Arc<Observed>,
    model: Option<PathBuf>,
) -> WorkerSpec<ReadChild> {
    let probe_endpoint = endpoint.clone();
    let mut spec = WorkerSpec::new(
        "read",
        move |_generation| {
            let launch_id = uuid::Uuid::new_v4().to_string();
            let bootstrap = serde_json::to_vec(&WorkerBootstrap {
                bootstrap_version: BOOTSTRAP_VERSION,
                launch_id: launch_id.clone(),
                channel: channel.clone(),
            })
            .map_err(|e| format!("could not encode the read bootstrap: {e}"))?;
            Ok(LaunchPlan {
                args: launch_args(&root, model.as_deref()),
                bootstrap,
                payload: ReadChild {
                    launch_id,
                    endpoint: probe_endpoint.clone(),
                },
            })
        },
        // **Readiness is the worker's own `Status`**, with this launch's id in
        // it: a worker left over from another launch, or another build, is not
        // this child being ready.
        |child: &ReadChild| {
            let mut stream = koru_ipc::connect_timed(&child.endpoint, HANDSHAKE_TIMEOUT)
                .map_err(|e| e.to_string())?;
            handshake(&mut stream, &child.launch_id)
                .map(|_| ())
                .map_err(|refusal| match refusal {
                    Refusal::Wrong(why) => why,
                    Refusal::Closed { .. } => "the read worker closed the connection".into(),
                })
        },
    );
    spec.program = Program::Sibling(koru_update::READ_EXE.into());
    spec.memory_limit = Some(MEMORY_LIMIT_BYTES);
    // **Out of the person's way** (TECHNICAL §14 "Staying out of the way"):
    // created below normal priority; the worker puts its own I/O in
    // background mode when it finds itself launched so (OCR Plan 3.1).
    spec.priority = Priority::Background;
    spec.ready_deadline = READY_DEADLINE;
    // No graceful stop: an idle worker holds nothing worth saving, and a
    // working one is only ever stopped because it is hung.
    spec.stop_deadline = Duration::from_secs(2);
    spec.on_event = Some(Box::new(move |event| match event.kind {
        WorkerEventKind::MemoryLimitReached { .. } => {
            // The supervisor logs this unconditionally; it is recorded here so
            // the job that was running is told it was memory.
            observed.memory_kills.fetch_add(1, Ordering::Relaxed);
            observed
                .memory_generation
                .store(event.generation.0, Ordering::Release);
        }
        WorkerEventKind::Exited { code } => {
            if let Ok(mut exit) = observed.last_exit.lock() {
                *exit = Some((event.generation.0, code));
            }
            if code == Some(HOSTILE_EXIT) {
                tracing::warn!(
                    target: "koru_readings",
                    generation = %event.generation,
                    "the read worker's watchdog ended it on a file; that file is not read again"
                );
            }
        }
        WorkerEventKind::SpawnFailed { error } => tracing::warn!(
            target: "koru_readings",
            %error,
            "the read worker could not be started"
        ),
        WorkerEventKind::Exhausted => tracing::error!(
            target: "koru_readings",
            "the read worker's restart budget is exhausted; reading waits"
        ),
        _ => {}
    }));
    spec
}

/// **The worker's command line**: `--root <store dir> --supervised`, and
/// `--model <dir>` when a reader is installed (OCR Plan 2.4). Without it the
/// worker reads text layers and answers every recognition job `needs-reader`.
pub fn launch_args(root: &Path, model: Option<&Path>) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec![
        "--root".into(),
        root.as_os_str().to_owned(),
        "--supervised".into(),
    ];
    if let Some(model) = model {
        args.push("--model".into());
        args.push(model.as_os_str().to_owned());
    }
    args
}

/// Why a conversation with the worker ended without its answer.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The connection closed or went silent — the process is gone or hung.
    Closed { hung: bool },
    /// It answered, and not as the process this supervision launched, or at a
    /// protocol this build does not speak.
    Wrong(String),
}

fn closed(error: std::io::Error) -> Refusal {
    Refusal::Closed {
        hung: matches!(
            error.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ),
    }
}

/// **The first frame on every connection is `Status`** — the worker closes a
/// connection that opens with anything else — and the answer must carry this
/// launch's id and this build's protocol.
pub fn handshake<S: Read + Write>(
    stream: &mut S,
    launch_id: &str,
) -> Result<WorkerStatus, Refusal> {
    koru_ipc::frame::write_frame(stream, FRAME_VERSION, &Request::Status).map_err(closed)?;
    loop {
        let (version, body) = koru_ipc::frame::read_raw(stream).map_err(closed)?;
        if version != FRAME_VERSION {
            return Err(Refusal::Wrong(format!(
                "the read worker frames at version {version}, this build at {FRAME_VERSION}"
            )));
        }
        let event: Event = koru_ipc::frame::decode(&body)
            .map_err(|_| Refusal::Wrong("the read worker's answer did not decode".into()))?;
        let Event::Status(status) = event else {
            continue;
        };
        if status.supervisor_launch_id.as_deref() != Some(launch_id) {
            return Err(Refusal::Wrong(
                "the read worker on this endpoint belongs to another launch".into(),
            ));
        }
        if status.protocol_version != PROTOCOL_VERSION {
            return Err(Refusal::Wrong(format!(
                "the read worker speaks protocol {}, this build {PROTOCOL_VERSION}",
                status.protocol_version
            )));
        }
        return Ok(status);
    }
}

/// Send one `Read` and hand back every event for its job until the terminal
/// one. Events for another job — a late answer to one already over — are
/// dropped by comparison.
pub fn converse<S: Read + Write>(
    stream: &mut S,
    request: &Request,
    events: &mut dyn FnMut(Event),
) -> Result<(), Refusal> {
    let Request::Read { job, .. } = request else {
        return Err(Refusal::Wrong("only a read is a job".into()));
    };
    let job = *job;
    koru_ipc::frame::write_frame(stream, FRAME_VERSION, request).map_err(closed)?;
    loop {
        let (version, body) = koru_ipc::frame::read_raw(stream).map_err(closed)?;
        if version != FRAME_VERSION {
            return Err(Refusal::Wrong(format!(
                "a frame at version {version} arrived mid-job"
            )));
        }
        let event: Event = koru_ipc::frame::decode(&body)
            .map_err(|_| Refusal::Wrong("a worker event did not decode".into()))?;
        if event.job() != Some(job) {
            continue;
        }
        let terminal = event.is_terminal();
        events(event);
        if terminal {
            return Ok(());
        }
    }
}

/// Where a conversation ended without its answer: before the worker proved
/// itself, or during the job.
#[derive(Debug, PartialEq, Eq)]
pub enum Broke {
    Handshake(Refusal),
    Job(Refusal),
}

/// What the conversation's own thread hands back, in order.
enum Heard {
    /// The handshake passed, with what the worker said about itself; the
    /// job's patience applies from here.
    Handshaken(WorkerStatus),
    Event(Event),
    End(Result<(), Broke>),
}

/// **[`handshake`] then [`converse`], on a thread of their own, given up on
/// after a silence.**
///
/// A socket timeout cannot do this on Windows: `koru_ipc::set_timeouts` is
/// best-effort, and `interprocess`'s named-pipe stream answers every
/// `set_recv_timeout` with `Unsupported` (`os/windows/named_pipe/local_socket/
/// stream.rs`), which `koru-ipc` deliberately ignores. A worker that is alive
/// and silent — wedged where its own watchdog cannot see, or suspended — would
/// block the read, and the coordinator with it, for ever. Plan 1.8 saw exactly
/// that on a real store root: a suspended worker held a job for over three
/// minutes against a 5 s handshake deadline and a 120 s job patience.
///
/// So the reads happen here, and this thread only waits on a channel with a
/// deadline that restarts at every frame: `handshake_patience` until the
/// worker has answered `Status`, then `job_patience`. A silence past it is
/// `Refusal::Closed { hung: true }` — the caller ends the worker, which closes
/// the pipe and lets the reading thread finish on its own. Events reach
/// `events` on the calling thread, in the order they arrived.
// The launcher calls `patiently_with` (it learns the worker's status on the
// way); this, the plain form, is what the conversation's own tests drive.
#[cfg_attr(not(test), allow(dead_code))]
pub fn patiently<S: Read + Write + Send + 'static>(
    stream: S,
    launch_id: String,
    request: Request,
    handshake_patience: Duration,
    job_patience: Duration,
    events: &mut dyn FnMut(Event),
) -> Result<(), Broke> {
    patiently_with(
        stream,
        launch_id,
        Some(request),
        handshake_patience,
        job_patience,
        &mut |_| {},
        events,
    )
}

/// [`patiently`], also handing the worker's `Status` to `on_status` once the
/// handshake passes (OCR Plan 2.4: what this launch can recognize). With no
/// `request` it is the handshake alone — the coordinator's probe, under the
/// same patience as a job.
pub fn patiently_with<S: Read + Write + Send + 'static>(
    mut stream: S,
    launch_id: String,
    request: Option<Request>,
    handshake_patience: Duration,
    job_patience: Duration,
    on_status: &mut dyn FnMut(WorkerStatus),
    events: &mut dyn FnMut(Event),
) -> Result<(), Broke> {
    let (heard, hearing) = std::sync::mpsc::channel::<Heard>();
    let talking = std::thread::Builder::new()
        .name("koru-readings-talk".into())
        .spawn(move || {
            let ended = match handshake(&mut stream, &launch_id) {
                Err(refusal) => Err(Broke::Handshake(refusal)),
                Ok(status) => {
                    let _ = heard.send(Heard::Handshaken(status));
                    match &request {
                        Some(request) => converse(&mut stream, request, &mut |event| {
                            let _ = heard.send(Heard::Event(event));
                        })
                        .map_err(Broke::Job),
                        None => Ok(()),
                    }
                }
            };
            let _ = heard.send(Heard::End(ended));
        });
    if talking.is_err() {
        return Err(Broke::Handshake(Refusal::Wrong(
            "the conversation's thread could not be started".into(),
        )));
    }
    let mut patience = handshake_patience;
    let mut handshaken = false;
    loop {
        match hearing.recv_timeout(patience) {
            Ok(Heard::Handshaken(status)) => {
                handshaken = true;
                patience = job_patience;
                on_status(status);
            }
            Ok(Heard::Event(event)) => events(event),
            Ok(Heard::End(ended)) => return ended,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let hung = Refusal::Closed { hung: true };
                return Err(if handshaken {
                    Broke::Job(hung)
                } else {
                    Broke::Handshake(hung)
                });
            }
            // The thread ended without saying how: it panicked mid-read.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let gone = Refusal::Closed { hung: false };
                return Err(if handshaken {
                    Broke::Job(gone)
                } else {
                    Broke::Handshake(gone)
                });
            }
        }
    }
}

#[cfg(test)]
mod tests;
