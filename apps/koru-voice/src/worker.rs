// SPDX-License-Identifier: MPL-2.0

//! **The inference thread: it owns the session, the engine and the device, and
//! performs the effects the state machine emits.**
//!
//! `koru_voice::Session` decides; this decides nothing. Every branch here is
//! *how* to do a thing — lease a model, open a microphone, ask an engine to
//! finalise — and never *whether*, which is
//! [ADR-249](../../../docs/decisions/ADR-226-250.md)'s whole point: a phone has
//! no worker process and no socket, hosts the same machine, and would otherwise
//! disagree with this file about what a cancelled session does with a late
//! final result.
//!
//! ## The three threads, and which one this is
//!
//! - The **IPC thread** (`crate::server`) answers messages and does no
//!   inference. It hands [`Command`]s here and never touches the session.
//! - The **capture callback** (`crate::capture`) writes into
//!   [`crate::ring::Ring`] and returns. It allocates nothing and blocks on
//!   nothing.
//! - **This thread** loads the model and runs the engine. A
//!   several-hundred-megabyte load is *the world*, and a thread that answers a
//!   person never waits on it.
//!
//! ## What it does while it is listening, and what it does while it is not
//!
//! While a session is live it wakes every [`POLL_INTERVAL`] to drain the ring
//! into the engine and to drain the engine of what it has decided. That is a
//! poll, and it is deliberate: the alternative is having the device callback
//! signal a condvar, which is a lock on the real-time path.
//!
//! While there is **no** session it wakes exactly once, at whichever of the two
//! idle deadlines is next — see [`crate::idle`]. An idle worker costs one wake
//! every few minutes, which is the difference between a background process a
//! person can leave running and one they learn to kill.
//!
//! ## A level meter, beside the session rather than in it
//!
//! `Request::Meter` opens a microphone so a picker can show that it hears
//! something. It is answered here, because the device belongs to this
//! thread, but it never reaches [`Session`]: no model is resolved, nothing is
//! leased, and the samples are reduced to the dictation's own
//! [`LevelBucket`] and dropped. While it runs this thread wakes once per
//! [`LEVEL_INTERVAL`] and sends a bucket only when it moved. One meter at a
//! time, the newest replacing the last; a meter is refused while a dictation
//! runs, and a dictation that begins closes the meter before it opens the
//! device.
//!
//! ## The one place this bends the contract, and why
//!
//! `koru_voice::HostReply` has three failure variants — a model load, a capture
//! start, a finalisation — and no variant for *an engine failed while audio was
//! arriving*. All three end the session as `Failed { code }` with whatever code
//! the host passed, so the outcome is expressible; only the name is not.
//! [`Worker::engine_failed`] therefore reports an `accept_audio` failure as
//! [`HostReply::FinishFailed`] carrying the engine's own code, because that
//! variant's documented consequence — no final transcript, the composition
//! discarded rather than committed from the last partial — is exactly what
//! happens. Recorded in `docs/features/voice/archive/QUESTIONS.md`, with the alternative:
//! a fourth `HostReply` variant in `koru-voice`.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender};
use std::time::{Duration, Instant};

use koru_voice::{
    Effect, EngineEvent, ErrorCode, Event, HostReply, InputChoice, LevelBucket, ModelId, Request,
    SAMPLE_RATE, Session, SessionId, SessionRequest, SpeechEngine, State, VoiceError,
};
use koru_voice_models::{InstalledModel, LoadedModel, ModelError, ModelErrorCode, ModelStore};

use crate::capture::Capture;
use crate::engine::EngineHost;
use crate::idle::{IdleAction, IdlePolicy};
use crate::ring::Ring;
use koru_voice::control::Outgoing;
use koru_voice::status::DeviceCategory;
use koru_voice::status::{EngineSummary, ModelSummary, STAGES_KEPT, StageTiming, StatusReport};

/// How often the inference thread wakes **while a session is live**.
///
/// Ten milliseconds is 160 samples at [`SAMPLE_RATE`] and is well inside the
/// ring's two seconds, so it is a latency choice rather than a safety one: a
/// partial transcript that took an extra 10 ms to leave is not something a
/// person can perceive, and a shorter interval buys nothing but wakes.
pub const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How often a level bucket is sent while listening.
///
/// A meter is drawn at ten steps a second by every audio application anybody
/// has used, and one event per drain would be a hundred — which is the "a line
/// per frame" anti-pattern applied to a socket.
pub const LEVEL_INTERVAL: Duration = Duration::from_millis(100);

/// How many engine events one pump drains before going back around the loop.
///
/// A bound rather than a budget: an adapter that produced events faster than
/// they are consumed would otherwise starve the command channel, and a worker
/// that stops answering `cancel` is worse than one that is a pump behind.
const MAX_POLLED: usize = 64;

/// Which client a message came from. Assigned by the IPC thread, meaningful
/// only within this process, and never persisted.
pub type ClientId = u64;

/// Where a client's events go. The IPC thread owns the other end.
pub type EventSink = Sender<Outgoing>;

/// Where the audio comes from.
///
/// An enum rather than a trait object because the product variant holds a
/// `cpal::Stream`, which is not `Send` on every backend — a trait would have
/// pushed that constraint onto every implementor for the benefit of one.
pub enum CaptureSource {
    /// The product: the machine's default input device.
    Device,
    /// A recording, pushed into the ring when capture starts.
    ///
    /// What `a_recorded_session_reaches_a_final_transcript` drives, and what the
    /// M0 benchmark will drive. It is not a "test-only" variant in any sense
    /// that matters: measuring an engine against a corpus needs exactly this.
    Recorded(Arc<Vec<f32>>),
}

impl std::fmt::Debug for CaptureSource {
    /// **A count, never the samples.** A derived `Debug` here would print a
    /// recording into whatever formatted the enclosing struct, which is the
    /// mistake `koru_voice::Transcript`'s redacted `Debug` exists to prevent
    /// one type over.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureSource::Device => f.write_str("Device"),
            CaptureSource::Recorded(samples) => {
                write!(f, "Recorded({} samples)", samples.len())
            }
        }
    }
}

/// Everything the inference thread needs, decided before it starts.
pub struct WorkerConfig {
    pub store: Arc<dyn ModelStore>,
    pub engine: Box<dyn EngineHost>,
    pub capture: CaptureSource,
    pub ring_capacity: usize,
    pub idle: IdlePolicy,
    pub supervisor_launch_id: Option<String>,
}

impl WorkerConfig {
    /// The product configuration: the real model cache, both product engines,
    /// and the machine's default microphone.
    ///
    /// This file does not name either runtime. The signed model descriptor and
    /// the request's quality tier select one through the product host.
    pub fn product(root: Option<&std::path::Path>) -> Result<WorkerConfig, ModelError> {
        Ok(WorkerConfig {
            store: crate::models::open_store(root)?,
            engine: crate::engine::product_host(),
            capture: CaptureSource::Device,
            ring_capacity: Ring::DEFAULT_CAPACITY,
            idle: IdlePolicy::from_env(),
            supervisor_launch_id: None,
        })
    }
}

/// What the IPC thread asks of the inference thread.
pub enum Command {
    /// A dictation request, and where its events should go.
    Request {
        client: ClientId,
        request: Request,
        sink: EventSink,
    },
    /// A client's socket closed. **An interruption discards**: if it owned the
    /// live session, that session is cancelled and nothing is written. Half a
    /// sentence landing in a document you were not looking at is worse than
    /// losing it.
    Disconnected(ClientId),
    /// Answer with a snapshot. A `SyncSender` so the IPC thread waits for the
    /// real answer rather than being handed a promise it would have to poll.
    Status(SyncSender<Box<StatusReport>>),
    /// **Which capture devices are there?** Answered on this thread rather
    /// than on the connection's, for the same reason `Status` is: enumerating
    /// touches the audio host, and the rule this file is built on is that the
    /// thread which answers a person never waits on the world. Bounded by the
    /// server's own timeout.
    Devices(SyncSender<Vec<koru_voice::InputDevice>>),
    /// **Drop the model now**, and answer with the snapshot that proves it.
    ///
    /// The same `SyncSender` shape as [`Command::Status`] and for the same
    /// reason: the caller is a person waiting on a reply, and a fire-and-forget
    /// release would leave the notification area redrawing on a guess. The
    /// answer is taken *after* the unload, so `model_loaded` in it is the
    /// result rather than the state before it.
    Release(SyncSender<Box<StatusReport>>),
    /// End the process.
    Stop,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The request's *shape*, never its fields: `Begin` carries a model
            // id and a language, which are fine, and naming them here would be
            // one edit away from naming something that is not.
            Command::Request {
                client, request, ..
            } => {
                let verb = match request {
                    Request::Begin { .. } => "begin",
                    Request::Finish { .. } => "finish",
                    Request::Cancel { .. } => "cancel",
                    Request::Devices { .. } => "devices",
                    Request::Meter { .. } => "meter",
                    Request::MeterStop { .. } => "meter_stop",
                };
                write!(f, "Request({verb} from {client})")
            }
            Command::Disconnected(client) => write!(f, "Disconnected({client})"),
            Command::Status(_) => f.write_str("Status"),
            Command::Devices(_) => f.write_str("Devices"),
            Command::Release(_) => f.write_str("Release"),
            Command::Stop => f.write_str("Stop"),
        }
    }
}

/// The inference thread's end of the command channel.
#[derive(Clone, Debug)]
pub struct WorkerHandle {
    commands: Sender<Command>,
}

impl WorkerHandle {
    /// `false` when the inference thread has gone, which the IPC thread reads
    /// as *stop accepting*.
    pub fn send(&self, command: Command) -> bool {
        self.commands.send(command).is_ok()
    }

    /// Ask for a snapshot, waiting at most `timeout`.
    ///
    /// **Bounded, because this runs on the thread answering a person.** A
    /// `status` that waited forever would hang behind an engine mid-utterance,
    /// which is precisely the moment somebody runs it.
    /// Ask the inference thread which capture devices there are. `None` when
    /// it did not answer inside `timeout`, which is the same reading
    /// [`WorkerHandle::status`] takes and for the same reason.
    pub fn devices(&self, timeout: Duration) -> Option<Vec<koru_voice::InputDevice>> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        if !self.send(Command::Devices(tx)) {
            return None;
        }
        rx.recv_timeout(timeout).ok()
    }

    pub fn status(&self, timeout: Duration) -> Option<Box<StatusReport>> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        if !self.send(Command::Status(tx)) {
            return None;
        }
        rx.recv_timeout(timeout).ok()
    }

    /// Ask for the model to be dropped, and for the snapshot that says whether
    /// it was. Bounded for the reason [`WorkerHandle::status`] gives.
    pub fn release(&self, timeout: Duration) -> Option<Box<StatusReport>> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        if !self.send(Command::Release(tx)) {
            return None;
        }
        rx.recv_timeout(timeout).ok()
    }
}

/// A one-shot "the process should end now".
///
/// Cloned into the inference thread and into the IPC thread; the main thread
/// blocks on the receiver, so the process ends by *returning* from `main` and
/// dropping the `koru_log::Logging` guard rather than by `process::exit`, which
/// would silently lose whatever the file writer had not flushed.
#[derive(Clone, Debug)]
pub struct Shutdown(Sender<()>);

impl Shutdown {
    pub fn channel() -> (Shutdown, Receiver<()>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (Shutdown(tx), rx)
    }

    pub fn signal(&self) {
        let _ = self.0.send(());
    }
}

/// Start the inference thread.
pub fn spawn(config: WorkerConfig, shutdown: Shutdown) -> WorkerHandle {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("koru-voice-worker-inference".into())
        .spawn(move || Worker::new(config).run(rx, shutdown))
        .expect("the inference thread could not be started");
    WorkerHandle { commands: tx }
}

/// **A level meter that recognises nothing** (`Request::Meter`).
///
/// The microphone open, its samples reduced to the dictation's own
/// [`LevelBucket`] by [`crate::capture::level_of`] and dropped. No model, no
/// engine and no session: it lives beside the session machine rather than in
/// it, because nothing about it is a dictation, and it runs only while that
/// machine is idle — a dictation that begins stops it first.
struct Meter {
    client: ClientId,
    sink: EventSink,
    /// The request's correlation pair, stamped on every level it sends.
    session: SessionId,
    generation: u64,
    /// The open device. `None` for a recorded source, whose samples went into
    /// the ring when the meter began.
    capture: Option<Capture>,
    /// The loudest bucket drained since the last one was weighed.
    peak: LevelBucket,
    /// The bucket last sent. **A meter speaks only when it moves**, so a
    /// silent room wakes nobody ten times a second.
    sent: LevelBucket,
    /// When the last bucket was weighed: one per [`LEVEL_INTERVAL`], the
    /// dictation's own pace.
    weighed: Option<Instant>,
}

/// Everything the inference thread owns.
///
/// Constructed on that thread and never moved off it: `capture` holds a
/// `cpal::Stream`, which is not `Send` on every backend. That is not a
/// limitation to work around — the thread that performs `StartCapture` and
/// `StopCapture` is the thread that should own the device.
struct Worker {
    config: WorkerConfig,
    session: Session,
    /// The model this session resolved to, before it was leased.
    pending: Option<InstalledModel>,
    /// The lease. Held for as long as the engine is, so nothing on another
    /// channel prunes the files out from under a running recogniser.
    loaded: Option<LoadedModel>,
    engine: Option<Box<dyn SpeechEngine>>,
    /// **The generation the *engine run* was begun with**, not the session's
    /// current one. That is the whole of the late-message mechanism: a poll
    /// that drains a decoder belonging to a cancelled session passes the old
    /// number, and `Session` drops the event rather than committing it.
    ///
    /// Deliberately **not** cleared by `Release`. An engine that produces a
    /// final result two seconds after a cancel must still be polled, so that
    /// the drop is counted rather than never observed.
    engine_generation: Option<u64>,
    capture: Option<Capture>,
    /// **Which microphone the live dictation asked for.**
    ///
    /// Set when a `Begin` is accepted and read when `Effect::StartCapture`
    /// arrives, because those are two moments: the session machine loads the
    /// model in between. It is not on `SessionRequest` because the session
    /// machine has no opinion about hardware -- it emits `StartCapture` and
    /// the host performs it, which is ADR-249's whole division.
    requested_input: koru_voice::InputChoice,
    /// The level meter, while one runs. Never at the same time as a
    /// dictation: both read [`Worker::ring`].
    meter: Option<Meter>,
    ring: Arc<Ring>,
    /// The client whose dictation this is, and where its events go.
    owner: Option<(ClientId, EventSink)>,
    /// Reused between drains so the audio path allocates once.
    scratch: Vec<f32>,
    started: Instant,
    idle_since: Instant,
    last_level: Option<Instant>,
    finalise_started: Option<Instant>,
    stages: VecDeque<StageTiming>,
    /// The default input device's category, read once at startup so `status`
    /// can answer before anybody has dictated.
    device: Option<DeviceCategory>,
    /// The last model that was actually loaded. **Never cleared**, including by
    /// an idle unload: the question after "it went quiet" is which model it had
    /// been using, and a field that emptied itself would answer *none* to
    /// exactly the person asking.
    last_model: Option<ModelSummary>,
    last_error: Option<ErrorCode>,
}

impl Worker {
    fn new(config: WorkerConfig) -> Worker {
        let ring = Arc::new(Ring::new(config.ring_capacity));
        let device = match &config.capture {
            CaptureSource::Device => crate::capture::describe_default(),
            CaptureSource::Recorded(_) => None,
        };
        Worker {
            config,
            session: Session::new(),
            pending: None,
            loaded: None,
            engine: None,
            engine_generation: None,
            capture: None,
            requested_input: koru_voice::InputChoice::SystemDefault,
            meter: None,
            ring,
            owner: None,
            scratch: Vec::new(),
            started: Instant::now(),
            idle_since: Instant::now(),
            last_level: None,
            finalise_started: None,
            stages: VecDeque::new(),
            device,
            last_model: None,
            last_error: None,
        }
    }

    fn run(mut self, commands: Receiver<Command>, shutdown: Shutdown) {
        tracing::info!(
            engine = %self.config.engine.describe().engine,
            device = %self.device.map(|d| d.to_string()).unwrap_or_else(|| "unknown".into()),
            "the voice worker's inference thread is running"
        );
        loop {
            self.pump();
            let wait = match self.next_wait() {
                Some(wait) => wait,
                None => {
                    tracing::info!(
                        idle_ms = self.idle_since.elapsed().as_millis() as u64,
                        "no dictation for the idle deadline; the voice worker is ending"
                    );
                    shutdown.signal();
                    return;
                }
            };
            match commands.recv_timeout(wait) {
                Ok(Command::Request {
                    client,
                    request,
                    sink,
                }) => self.handle_request(client, sink, request),
                Ok(Command::Disconnected(client)) => self.handle_disconnect(client),
                Ok(Command::Status(reply)) => {
                    let _ = reply.send(Box::new(self.status()));
                }
                // **A question, and it starts nothing.** No model is loaded,
                // no device is opened, and the session machine is not touched
                // -- so this is answerable while a dictation is running and
                // does not disturb it.
                Ok(Command::Devices(reply)) => {
                    let _ = reply.send(crate::capture::input_devices());
                }
                Ok(Command::Release(reply)) => {
                    // **Only while idle.** A menu row may not take the weights
                    // out from under an utterance somebody is halfway through,
                    // and the snapshot that goes back says which of the two
                    // happened -- `model_loaded` is still true when the release
                    // was refused, so the caller reports what is rather than
                    // what it asked for.
                    if self.session.state() == State::Idle {
                        self.unload("the model was released on request");
                    } else {
                        tracing::info!(
                            state = ?self.session.state(),
                            "a release was refused because a dictation is running"
                        );
                    }
                    let _ = reply.send(Box::new(self.status()));
                }
                Ok(Command::Stop) => {
                    tracing::info!("the voice worker was asked to stop");
                    self.discard_live_session("the worker was asked to stop");
                    shutdown.signal();
                    return;
                }
                Err(RecvTimeoutError::Timeout) => {}
                // Every sender is gone, which means the IPC thread has stopped
                // and nothing can reach this worker again.
                Err(RecvTimeoutError::Disconnected) => {
                    shutdown.signal();
                    return;
                }
            }
        }
    }

    /// How long to block on the command channel, or `None` to end the process.
    ///
    /// This is also where the idle policy is *applied* — reading it as "decide,
    /// then wait exactly that long" is the reason an idle worker wakes once
    /// rather than ticking.
    fn next_wait(&mut self) -> Option<Duration> {
        if self.session.state() != State::Idle {
            return Some(POLL_INTERVAL);
        }
        // **A running meter is somebody looking at it**, so the worker is not
        // idle: it wakes once a level interval to weigh what arrived, and the
        // idle clock starts again when the meter stops.
        if self.meter.is_some() {
            return Some(LEVEL_INTERVAL);
        }
        // Bound before the match: `decide` borrows `self.config.idle`, and a
        // match scrutinee's temporaries live to the end of the match — so an
        // arm that unloads (which is `&mut self`) would not compile.
        let idle = self.config.idle;
        let action = idle.decide(self.idle_since.elapsed(), self.engine.is_some());
        match action {
            IdleAction::Exit => None,
            IdleAction::UnloadModel => {
                self.unload("the speech model was unloaded after the idle deadline");
                // Round the loop once more so the next wait is computed against
                // a worker that no longer holds a model.
                Some(Duration::from_millis(1))
            }
            IdleAction::Wait(wait) => Some(wait),
        }
    }

    /// **Let go of the model.** The reason this process exists: several hundred
    /// megabytes are only genuinely released by dropping the engine and its
    /// lease, and a released lease is also what lets another channel prune.
    ///
    /// `why` is the log line, because there are now two ways here — the idle
    /// deadline and [`Control::Release`](koru_voice::control::Control::Release) —
    /// and a single sentence naming the deadline would have made an asked-for
    /// release read in the log as a timer that fired early.
    fn unload(&mut self, why: &'static str) {
        if self.engine.is_none() && self.loaded.is_none() {
            return;
        }
        let model = self.loaded.as_ref().map(|m| m.installed().id().to_string());
        self.engine = None;
        self.loaded = None;
        tracing::info!(
            model = model.as_deref().unwrap_or("none"),
            idle_ms = self.idle_since.elapsed().as_millis() as u64,
            "{why}"
        );
    }

    // ------------------------------------------------------------ the pump

    /// One turn: audio into the engine, decisions out of it.
    fn pump(&mut self) {
        self.pump_meter();
        self.pump_audio();
        if let Some(generation) = self.engine_generation {
            let mut polled = Vec::new();
            if let Some(engine) = self.engine.as_mut() {
                while let Some(event) = engine.poll() {
                    polled.push(event);
                    if polled.len() >= MAX_POLLED {
                        break;
                    }
                }
            }
            for event in polled {
                if matches!(event, EngineEvent::Final(_)) {
                    if let Some(started) = self.finalise_started.take() {
                        self.record_stage("finalise", started.elapsed());
                    }
                }
                let effects = self.session.engine_event(generation, event);
                self.perform(effects);
            }
        }
        // Partials and levels produce no effect, so nothing else would send
        // them.
        self.flush();
    }

    /// Drain the ring, authorise it, and feed the engine.
    ///
    /// **`Session::audio` authorises rather than accepts** — the session holds
    /// no buffer — so the order is fixed: ask, then feed. A host that fed first
    /// would push audio from a cancelled session into a decoder.
    fn pump_audio(&mut self) {
        if self.session.state() != State::Listening {
            return;
        }
        let mut samples = std::mem::take(&mut self.scratch);
        samples.clear();
        self.ring.drain(&mut samples);
        if samples.is_empty() {
            self.scratch = samples;
            return;
        }
        if self.session.audio(&samples).is_ok() {
            if self
                .last_level
                .is_none_or(|at| at.elapsed() >= LEVEL_INTERVAL)
            {
                self.last_level = Some(Instant::now());
                self.session.level(crate::capture::level_of(&samples));
            }
            let failure = self
                .engine
                .as_mut()
                .and_then(|engine| engine.accept_audio(&samples).err());
            if let Some(error) = failure {
                self.scratch = samples;
                self.engine_failed(error);
                return;
            }
        }
        self.scratch = samples;
    }

    // ------------------------------------------------------- the level meter

    /// **Weigh what the meter's microphone heard**, and send the bucket when
    /// a level interval has passed and it moved.
    ///
    /// The samples are drained and dropped: the loudest of them, as the
    /// dictation's own [`crate::capture::level_of`] buckets it, is all that is
    /// kept, and only until it is sent. Nothing here reaches a model.
    fn pump_meter(&mut self) {
        // A dictation owns the ring. The meter has been stopped by then
        // anyway; this keeps the two from ever draining it at once.
        if self.session.state() != State::Idle {
            return;
        }
        let Some(meter) = self.meter.as_mut() else {
            return;
        };
        let mut samples = std::mem::take(&mut self.scratch);
        samples.clear();
        self.ring.drain(&mut samples);
        if !samples.is_empty() {
            meter.peak = meter.peak.max(crate::capture::level_of(&samples));
        }
        self.scratch = samples;
        if meter
            .weighed
            .is_some_and(|at| at.elapsed() < LEVEL_INTERVAL)
        {
            return;
        }
        meter.weighed = Some(Instant::now());
        let bucket = std::mem::replace(&mut meter.peak, LevelBucket::SILENT);
        if bucket == meter.sent {
            return;
        }
        meter.sent = bucket;
        let level = Event::Level {
            session: meter.session,
            generation: meter.generation,
            bucket,
        };
        if meter.sink.send(Outgoing::Event(level)).is_err() {
            self.stop_meter("the meter's client went away");
        }
    }

    /// **Open a microphone for a level meter**, replacing any meter already
    /// running: one microphone, and the newest question wins, which is how a
    /// picker switches devices.
    ///
    /// Refused while a dictation runs — it has the microphone, and the meter
    /// is the one that yields. No model is resolved or leased here, so a
    /// meter works on an installation with nothing downloaded.
    fn start_meter(
        &mut self,
        client: ClientId,
        sink: EventSink,
        session: SessionId,
        generation: u64,
        input: InputChoice,
    ) {
        if self.session.state() != State::Idle {
            return self.refuse(
                &sink,
                session,
                generation,
                ErrorCode::Busy.with("a dictation is running and has the microphone"),
            );
        }
        self.stop_meter("a meter was replaced");
        self.ring.clear();
        let recorded = match &self.config.capture {
            CaptureSource::Device => None,
            CaptureSource::Recorded(samples) => Some(samples.clone()),
        };
        let capture = match recorded {
            Some(samples) => {
                for sample in samples.iter() {
                    self.ring.push(*sample);
                }
                None
            }
            None => match crate::capture::open(&input, self.ring.clone()) {
                Ok(capture) => Some(capture),
                Err(error) => {
                    tracing::warn!(
                        code = %error.code,
                        "the microphone would not open for a level meter"
                    );
                    return self.refuse(&sink, session, generation, error);
                }
            },
        };
        tracing::debug!(client, %session, generation, "a level meter began");
        self.meter = Some(Meter {
            client,
            sink,
            session,
            generation,
            capture,
            peak: LevelBucket::SILENT,
            sent: LevelBucket::SILENT,
            weighed: None,
        });
    }

    /// **Close the meter's microphone**, if one is open. The device first and
    /// the ring after, so the callback cannot refill what was just emptied.
    fn stop_meter(&mut self, why: &'static str) {
        let Some(meter) = self.meter.take() else {
            return;
        };
        drop(meter);
        self.ring.clear();
        self.idle_since = Instant::now();
        tracing::debug!(why, "a level meter ended");
    }

    // ------------------------------------------------------------- effects

    /// Perform effects until none is left, feeding each reply back in.
    ///
    /// A queue rather than recursion, because `LoadModel` answers `ModelLoaded`
    /// which asks for `StartCapture` which answers `CaptureStarted`, and a
    /// recursive `perform` would make that chain a stack depth nobody can see
    /// in a backtrace.
    fn perform(&mut self, effects: Vec<Effect>) {
        let mut queue: VecDeque<Effect> = effects.into_iter().collect();
        let mut performed = 0usize;
        while let Some(effect) = queue.pop_front() {
            performed += 1;
            if performed > MAX_POLLED {
                // Unreachable with a correct state machine, and it is here
                // because the failure mode without it is a hung worker rather
                // than a message.
                tracing::error!("the voice session asked for more effects than one step can hold");
                break;
            }
            let replies = self.do_effect(effect);
            for reply in replies {
                queue.extend(self.session.host_reply(reply));
            }
        }
        self.flush();
        if self.session.state() == State::Idle {
            self.owner = None;
            self.idle_since = Instant::now();
        }
    }

    fn do_effect(&mut self, effect: Effect) -> Vec<HostReply> {
        // **`Release` and `StopCapture` can both arrive after the session has
        // already ended** — `Session::cancel` calls `end` before it pushes
        // `Release`, so `current()` is already `None`. Answering a reply for a
        // session nobody is running would move `dropped_replies`, which is a
        // counter people read, so those two answer nothing instead.
        let generation = self.session.current().map(|(_, generation)| generation);
        match effect {
            Effect::LoadModel(id) => match generation {
                Some(generation) => vec![self.load_model(&id, generation)],
                None => Vec::new(),
            },
            Effect::StartCapture => match generation {
                Some(generation) => vec![self.start_capture(generation)],
                None => Vec::new(),
            },
            Effect::StopCapture => {
                self.stop_capture();
                generation
                    .map(|generation| HostReply::CaptureStopped { generation })
                    .into_iter()
                    .collect()
            }
            Effect::Finish => match generation {
                Some(generation) => self.finish_engine(generation),
                None => Vec::new(),
            },
            Effect::Release => {
                self.release();
                Vec::new()
            }
        }
    }

    /// Lease the model, build the engine over it, and begin the run.
    ///
    /// **A model already in memory costs nothing here**, which is the whole
    /// point of `Effect::Release` not meaning *unload*: a person who dictates
    /// twice in ten seconds must not pay a several-hundred-megabyte load twice.
    fn load_model(&mut self, id: &ModelId, generation: u64) -> HostReply {
        let failed = |code: ErrorCode| HostReply::ModelLoadFailed { generation, code };
        let already = self
            .loaded
            .as_ref()
            .is_some_and(|loaded| loaded.installed().id() == id)
            && self.engine.is_some();
        if !already {
            // Drop the old one *first*: two models in memory at once is the one
            // thing this process exists to prevent.
            self.engine = None;
            self.loaded = None;
            let Some(pending) = self.pending.clone() else {
                return failed(ErrorCode::ModelMissing);
            };
            let started = Instant::now();
            let engine_id = pending.descriptor.engine.clone();
            let opened = LoadedModel::open(
                &*self.config.store,
                pending.id(),
                pending.version(),
                &engine_id,
            );
            let loaded = match opened {
                Ok(loaded) => loaded,
                Err(error) => {
                    let code = model_error_code(&error);
                    tracing::warn!(
                        model = %pending.id(),
                        code = %code,
                        "a speech model could not be leased"
                    );
                    self.last_error = Some(code);
                    return failed(code);
                }
            };
            let built = self.config.engine.open(&loaded);
            let engine = match built {
                Ok(engine) => engine,
                Err(error) => {
                    tracing::warn!(
                        model = %pending.id(),
                        code = %error.code,
                        "a speech engine could not be built over a leased model"
                    );
                    self.last_error = Some(error.code);
                    return failed(error.code);
                }
            };
            self.record_stage("model_load", started.elapsed());
            tracing::info!(
                model = %loaded.installed().id(),
                version = %loaded.installed().version(),
                engine = %engine_id,
                millis = started.elapsed().as_millis() as u64,
                "a speech model was loaded"
            );
            self.last_model = Some(summarise(loaded.descriptor()));
            self.loaded = Some(loaded);
            self.engine = Some(engine);
        }
        let Some(request) = self.session.request().cloned() else {
            return failed(ErrorCode::WrongState);
        };
        if let Some(engine) = self.engine.as_mut() {
            if let Err(error) = engine.begin(&request) {
                tracing::warn!(code = %error.code, "the speech engine refused the session");
                self.last_error = Some(error.code);
                return failed(error.code);
            }
        }
        self.engine_generation = Some(generation);
        HostReply::ModelLoaded { generation }
    }

    fn start_capture(&mut self, generation: u64) -> HostReply {
        let started = Instant::now();
        self.ring.clear();
        self.last_level = None;
        // The source is read out first: a `match &self.config.capture` would
        // hold a borrow of `self` across arms that need `&mut self`.
        let recorded = match &self.config.capture {
            CaptureSource::Device => None,
            CaptureSource::Recorded(samples) => Some(samples.clone()),
        };
        if let Some(samples) = recorded {
            for sample in samples.iter() {
                self.ring.push(*sample);
            }
            self.record_stage("capture_start", started.elapsed());
            return HostReply::CaptureStarted { generation };
        }
        match crate::capture::open(&self.requested_input, self.ring.clone()) {
            Ok(capture) => {
                self.capture = Some(capture);
                self.record_stage("capture_start", started.elapsed());
                HostReply::CaptureStarted { generation }
            }
            Err(error) => {
                tracing::warn!(code = %error.code, "the microphone would not open");
                self.last_error = Some(error.code);
                HostReply::CaptureFailed {
                    generation,
                    code: error.code,
                }
            }
        }
    }

    /// Close the device. Dropping the stream is what stops it.
    fn stop_capture(&mut self) {
        self.capture = None;
    }

    fn finish_engine(&mut self, generation: u64) -> Vec<HostReply> {
        self.finalise_started = Some(Instant::now());
        let error = match self.engine.as_mut() {
            Some(engine) => engine.finish().err(),
            None => Some(ErrorCode::EngineFailed.with("there is no engine to finalise")),
        };
        match error {
            None => Vec::new(),
            Some(error) => {
                tracing::warn!(code = %error.code, "the speech engine would not finalise");
                self.last_error = Some(error.code);
                vec![HostReply::FinishFailed {
                    generation,
                    code: error.code,
                }]
            }
        }
    }

    /// **Let go of the session — not of the model.**
    ///
    /// The engine's per-session state, the device and the ring. The lease and
    /// the loaded weights stay: unloading is a separate idle deadline, because
    /// a person who dictates twice in ten seconds must not pay for the model
    /// twice.
    fn release(&mut self) {
        if let Some(engine) = self.engine.as_mut() {
            engine.cancel();
        }
        self.stop_capture();
        self.ring.clear();
        self.pending = None;
        self.finalise_started = None;
        self.last_level = None;
        self.idle_since = Instant::now();
    }

    /// An engine failure after the run began.
    ///
    /// See the module note: `FinishFailed` is the closest of the three failure
    /// replies, and the outcome — the composition discarded rather than
    /// committed from the last partial — is the one it documents.
    fn engine_failed(&mut self, error: VoiceError) {
        tracing::warn!(
            code = %error.code,
            "the speech engine failed; the dictation is discarded"
        );
        self.last_error = Some(error.code);
        let Some((_, generation)) = self.session.current() else {
            return;
        };
        let effects = self.session.host_reply(HostReply::FinishFailed {
            generation,
            code: error.code,
        });
        self.perform(effects);
    }

    // ------------------------------------------------------------ requests

    fn handle_request(&mut self, client: ClientId, sink: EventSink, request: Request) {
        match request {
            // **Answered by the IPC thread, never here.** A device query is a
            // question rather than a dictation: it arms no session and takes
            // no generation, so routing it through the machine would give the
            // high-water mark a number nothing ever dictated with. The arm is
            // named rather than swallowed by a wildcard, so a new *session*
            // verb cannot be added and quietly ignored.
            Request::Devices { .. } => {
                tracing::debug!(client, "a device query reached the inference thread");
            }
            // **A meter is answered here, not on the IPC thread**, because it
            // opens a device and the device belongs to this thread. It still
            // arms no session and takes no generation.
            Request::Meter {
                session,
                generation,
                input,
            } => self.start_meter(client, sink, session, generation, input),
            Request::MeterStop { session, .. } => {
                if self
                    .meter
                    .as_ref()
                    .is_some_and(|meter| meter.session == session)
                {
                    self.stop_meter("the meter was stopped on request");
                }
            }
            Request::Begin {
                session,
                generation,
                language,
                tier,
                model,
                input,
            } => {
                // Resolve before the machine is touched, so a refusal costs
                // nothing and leaves no half-started session behind.
                let engine_ids: Vec<_> = self
                    .config
                    .engine
                    .descriptions()
                    .into_iter()
                    .map(|description| description.engine)
                    .collect();
                let found = crate::models::resolve(
                    &*self.config.store,
                    model.as_ref(),
                    language,
                    tier,
                    &engine_ids,
                );
                let resolved = match found {
                    Ok(resolved) => resolved,
                    Err(error) => return self.refuse(&sink, session, generation, error),
                };
                let id = resolved.id().clone();
                let begun = self.session.begin(SessionRequest {
                    id: session,
                    generation,
                    model: id,
                    language,
                });
                match begun {
                    Ok(effects) => {
                        // **Recorded only once the machine has accepted it.** A
                        // refused `Begin` -- busy, or a stale generation -- must
                        // not change which microphone the dictation that *is*
                        // running will reopen.
                        self.requested_input = input;
                        self.pending = Some(resolved);
                        self.owner = Some((client, sink));
                        // **A dictation wins the microphone.** The meter is
                        // closed before the effects run, because they open
                        // the device for the dictation in this same call.
                        self.stop_meter("a dictation began");
                        tracing::debug!(%session, generation, "a dictation began");
                        self.perform(effects);
                    }
                    Err(error) => self.refuse(&sink, session, generation, error),
                }
            }
            Request::Finish {
                session,
                generation,
            } => {
                // **Everything captured up to this moment, before the machine
                // moves.** Once `finish` is applied the session is `Finalising`
                // and `Session::audio` refuses, so audio still sitting in the
                // ring would be lost — which is the last syllable of every
                // dictation. What the device itself is still holding is lost
                // regardless; `cpal` offers no flush. See QUESTIONS.
                self.pump_audio();
                let stepped = self.session.finish(session, generation);
                match stepped {
                    Ok(effects) => self.perform(effects),
                    Err(error) => self.refuse(&sink, session, generation, error),
                }
            }
            Request::Cancel {
                session,
                generation,
            } => {
                let stepped = self.session.cancel(session, generation);
                match stepped {
                    Ok(effects) => {
                        tracing::debug!(%session, generation, "a dictation was cancelled");
                        self.perform(effects);
                    }
                    Err(error) => self.refuse(&sink, session, generation, error),
                }
            }
        }
    }

    fn handle_disconnect(&mut self, client: ClientId) {
        if self
            .meter
            .as_ref()
            .is_some_and(|meter| meter.client == client)
        {
            self.stop_meter("the meter's client went away");
        }
        if self.owner.as_ref().is_some_and(|(id, _)| *id == client) {
            self.owner = None;
            self.discard_live_session("the client went away");
        }
    }

    /// Throw away whatever session is running. **An interruption discards.**
    fn discard_live_session(&mut self, why: &str) {
        let Some((session, generation)) = self.session.current() else {
            return;
        };
        tracing::debug!(%session, generation, why, "a dictation was discarded");
        let stepped = self.session.cancel(session, generation);
        if let Ok(effects) = stepped {
            self.perform(effects);
        }
    }

    /// Tell the asking client, and only it, that a request was not acted on.
    ///
    /// **Never the owner**, because a `Busy` refusal belongs to whoever was
    /// refused; sending it down the live session's socket would tell the person
    /// who *is* dictating that their dictation was refused.
    fn refuse(&mut self, sink: &EventSink, session: SessionId, generation: u64, error: VoiceError) {
        // The code, never the message: `message` is untranslated developer text
        // that may name an id or a count, and a log line carries the token a
        // person can search for.
        tracing::debug!(code = %error.code, %session, generation, "a voice request was refused");
        self.last_error = Some(error.code);
        let _ = sink.send(Outgoing::Event(Event::Refused {
            session,
            generation,
            code: error.code,
            message: error.message,
        }));
    }

    /// Send everything the session decided to say.
    fn flush(&mut self) {
        let events = self.session.take_events();
        if events.is_empty() {
            return;
        }
        let Some((_, sink)) = self.owner.as_ref() else {
            tracing::debug!(
                events = events.len(),
                "voice events with no client to send them to"
            );
            return;
        };
        let mut lost = false;
        for event in events {
            // The discriminant would be safe to log and the payload would not,
            // and there is no per-event line here at all: a partial arrives
            // several times a second, which is the one frequency
            // docs/contributing/operations/LOGGING.md names as never.
            if sink.send(Outgoing::Event(event)).is_err() {
                lost = true;
                break;
            }
        }
        if lost {
            self.owner = None;
            self.discard_live_session("the client's socket closed");
        }
    }

    // ------------------------------------------------------------- status

    fn record_stage(&mut self, stage: &str, elapsed: Duration) {
        if self.stages.len() == STAGES_KEPT {
            self.stages.pop_front();
        }
        self.stages.push_back(StageTiming {
            stage: stage.to_string(),
            millis: elapsed.as_millis() as u64,
        });
    }

    fn status(&self) -> StatusReport {
        let available = self.config.engine.descriptions();
        let engine = self
            .engine
            .as_ref()
            .map(|engine| engine.describe())
            .or_else(|| {
                self.last_model.as_ref().and_then(|model| {
                    available
                        .iter()
                        .find(|engine| engine.engine.as_str() == model.engine)
                        .cloned()
                })
            })
            .unwrap_or_else(|| self.config.engine.describe());
        StatusReport {
            worker_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: koru_voice::PROTOCOL_VERSION,
            channel: koru_channel::segment().to_string(),
            process_id: std::process::id(),
            supervisor_launch_id: self.config.supervisor_launch_id.clone(),
            engine: engine.engine.to_string(),
            engine_version: engine.version,
            available_engines: available
                .into_iter()
                .map(|engine| EngineSummary {
                    engine: engine.engine.to_string(),
                    version: engine.version,
                })
                .collect(),
            model_loaded: self.engine.is_some() && self.loaded.is_some(),
            model: self.last_model.clone(),
            device: self.device,
            // The dictation's device, or else the meter's: what is open.
            capture: self
                .capture
                .as_ref()
                .or(self.meter.as_ref().and_then(|meter| meter.capture.as_ref()))
                .map(Capture::info),
            sample_rate: SAMPLE_RATE,
            ring_overruns: self.ring.dropped(),
            ring_capacity: self.ring.capacity(),
            session: self.session.stats().clone(),
            state: self.session.state(),
            last_error: self
                .last_error
                .or(self.session.stats().last_error)
                .map(|code| code.as_str().to_string()),
            stages: self.stages.iter().cloned().collect(),
            uptime_millis: self.started.elapsed().as_millis() as u64,
            idle_millis: if self.session.state() == State::Idle {
                self.idle_since.elapsed().as_millis() as u64
            } else {
                0
            },
            unload_after_millis: self.config.idle.unload_after.as_millis() as u64,
            exit_after_millis: self.config.idle.exit_after.as_millis() as u64,
        }
    }
}

fn summarise(descriptor: &koru_voice_models::ModelDescriptor) -> ModelSummary {
    ModelSummary {
        id: descriptor.id.to_string(),
        version: descriptor.version.to_string(),
        digest: descriptor.digest.to_string(),
        tier: descriptor.tier.as_str().to_string(),
        engine: descriptor.engine.to_string(),
        languages: descriptor
            .languages
            .iter()
            .map(|language| language.tag().to_string())
            .collect(),
    }
}

/// A model-store failure as one of the session vocabulary's codes.
///
/// **Only three of them mean anything different to a person**, which is why
/// this is a small match with a catch-all rather than a table: *download it*,
/// *this build cannot run it*, *it cannot transcribe that*, and everything else
/// is `model_load_failed`. A wider mapping would put a store's internal
/// vocabulary on a wire that a window branches on.
fn model_error_code(error: &ModelError) -> ErrorCode {
    match error.code {
        ModelErrorCode::NotInstalled => ErrorCode::ModelMissing,
        ModelErrorCode::EngineMismatch => ErrorCode::EngineFailed,
        ModelErrorCode::LanguageUnsupported => ErrorCode::LanguageUnsupported,
        _ => ErrorCode::ModelLoadFailed,
    }
}

#[cfg(test)]
mod tests;
