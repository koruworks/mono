// SPDX-License-Identifier: MPL-2.0

//! **The transport, and the one place a request is written.**
//!
//! ```text
//! derive the endpoint   <- from the channel and the root, on every attempt
//! connect               <- connecting IS the liveness check
//!   ...nothing there?   -> ask the host's ensure closure, then poll
//! ClientHello           -> Welcome | Refused
//! split                 -> a writer thread and a reader thread
//! Request               -> the writer's queue         (the only write)
//! Event                 <- the reader, into the inbox, then wake
//! ```
//!
//! ## Two threads per link, and why not one
//!
//! A reader parked on a blocking read cannot also write, and the events a
//! dictation produces arrive when the engine has something to say rather than
//! in reply to anything. So a link owns a reader and a writer over
//! `koru_ipc::split`, and the host's loop holds only the writer's `Sender` --
//! which means the loop cannot block on a full pipe even if the worker stops
//! reading. That is the same shape `apps/koru/src/agent.rs` uses for the
//! Agent's link, for the same reason.
//!
//! ## End of stream is a session ending, not silence
//!
//! A worker that crashed, was killed, or was stopped by an update leaves a
//! host holding provisional text for a dictation nothing will ever finish. So
//! the reader, on end of stream with a session armed, synthesises the
//! [`koru_voice::Event::Ended`] the worker did not get to send, with
//! [`koru_voice::ErrorCode::WorkerUnavailable`] -- a stable code, and the one
//! the contract crate reserves for exactly this: *"the client's code, not the
//! worker's -- a worker that answers never sends it."*
//!
//! ## Nothing is logged that a person said
//!
//! Codes, counts and versions. A partial arrives several times a second, which
//! is the one frequency [LOGGING.md](../../../docs/contributing/operations/LOGGING.md) names
//! as never, and its payload is the thing that may never be written down at
//! all.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use koru_ipc::{Endpoint, IpcError};
use koru_voice::{
    ClientHello, EndReason, ErrorCode, Event, FRAME_VERSION, PROTOCOL_VERSION, Request, VoiceError,
    WorkerGreeting,
};

use crate::{RETRY_EVERY, START_DEADLINE, Shared};

/// What the worker said it was, taken from the greeting.
///
/// Two fields and no more: a greeting is where two builds find out whether they
/// can talk at all, and everything else a diagnostic wants is in
/// `koru-voice-worker status`, which is a question a supervisor asks rather than
/// something every client is handed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Worker {
    pub version: String,
    pub protocol_version: u32,
}

/// One live connection to a worker.
#[derive(Clone)]
pub struct Link {
    /// Cleared by the reader on end of stream and by the writer on a failed
    /// write, so the send path can tell a dead link from a live one without
    /// touching the socket.
    alive: Arc<AtomicBool>,
    outbound: std::sync::mpsc::Sender<(Request, std::sync::mpsc::Sender<bool>)>,
    worker: Worker,
}

impl Link {
    /// Whether this link is still worth using.
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    /// What the worker said it was when this link was made.
    pub fn worker(&self) -> Worker {
        self.worker.clone()
    }

    /// **Hand one request to the writer.** The only place a [`Request`] leaves
    /// this process, and it is deliberately not public: reconnection belongs to
    /// [`crate::VoiceClient::send`], which owns the link and is therefore the
    /// only thing that can replace it.
    ///
    /// `false` means the writer thread has gone, which means the socket has.
    pub(crate) fn send(&self, request: &Request) -> bool {
        if !self.alive() {
            return false;
        }
        let (done, answer) = std::sync::mpsc::channel();
        if self.outbound.send((request.clone(), done)).is_err() {
            return false;
        }
        answer.recv().unwrap_or(false)
    }

    /// Connect, handshake, and start the two threads that own the socket.
    ///
    /// **The endpoint is derived here, on every attempt.** Holding a resolved
    /// one from construction is the `send_with_retry` mistake; a worker that
    /// exited on its idle timeout and was started again binds the same name,
    /// and a client that remembered an address rather than a rule would be the
    /// only thing that could not find it.
    ///
    /// `start` says whether a machine with nothing listening may be asked for
    /// a worker. `false` for a question that is not somebody dictating -- see
    /// [`crate::VoiceClient::devices`].
    pub(crate) fn dial(shared: &Arc<Shared>, start: bool, id: u64) -> Result<Link, VoiceError> {
        let endpoint = endpoint(shared)?;
        let stream = reach(shared, &endpoint, start)?;
        if !koru_ipc::same_user(&stream) {
            return Err(
                ErrorCode::WorkerUnavailable.with("the voice endpoint belongs to another user")
            );
        }
        // A handshake that hung would hang whatever pressed the key. Back to
        // blocking below, for the life of the link: the reader spends nearly
        // all of it parked, and a timeout there would be a disconnect every two
        // seconds.
        koru_ipc::set_timeouts(&stream, Some(std::time::Duration::from_secs(2)));
        let worker = greet(shared, &stream)?;
        koru_ipc::set_timeouts(&stream, None);

        let (mut read, mut write) = koru_ipc::split(stream);
        let alive = Arc::new(AtomicBool::new(true));
        let (outbound, queued) =
            std::sync::mpsc::channel::<(Request, std::sync::mpsc::Sender<bool>)>();

        let writing = Arc::clone(&alive);
        let writer_host = Arc::downgrade(shared);
        std::thread::Builder::new()
            .name("koru-voice-worker write".into())
            .spawn(move || {
                use std::io::Write as _;
                while let Ok((request, done)) = queued.recv() {
                    let sent = koru_ipc::frame::write_frame(&mut write, FRAME_VERSION, &request)
                        .is_ok()
                        && write.flush().is_ok();
                    let _ = done.send(sent);
                    if !sent {
                        let expected = request_live(&request);
                        if let Some(host) = writer_host.upgrade() {
                            host.retire_link(id, expected);
                        }
                        break;
                    }
                }
                writing.store(false, Ordering::Release);
            })
            .map_err(|error| {
                ErrorCode::WorkerUnavailable.with(format!(
                    "a voice writer thread could not be started: {error}"
                ))
            })?;

        let reading = Arc::clone(&alive);
        let host = Arc::downgrade(shared);
        if std::thread::Builder::new()
            .name("koru-voice-worker read".into())
            .spawn(move || {
                read_events(&host, id, &mut read);
                reading.store(false, Ordering::Release);
                if let Some(host) = host.upgrade() {
                    host.retire_link(id, None);
                }
            })
            .is_err()
        {
            alive.store(false, Ordering::Release);
            return Err(
                ErrorCode::WorkerUnavailable.with("a voice reader thread could not be started")
            );
        }

        tracing::debug!(
            worker = worker.version.as_str(),
            protocol = worker.protocol_version,
            "a voice link is open"
        );
        Ok(Link {
            alive,
            outbound,
            worker,
        })
    }
}

/// Where the worker listens.
///
/// The channel's own Store root unless an explicit one was given, and
/// `Endpoint::for_service_root` either way -- the same call the worker binds
/// with, so a rooted worker and a rooted client meet while a rooted client and
/// an unrooted worker find nothing.
fn endpoint(shared: &Shared) -> Result<Endpoint, VoiceError> {
    let root = match &shared.root {
        Some(root) => root.clone(),
        None => koru_channel::store_root().ok_or_else(|| {
            ErrorCode::WorkerUnavailable.with("this build could not determine the channel's root")
        })?,
    };
    Endpoint::for_service_root(koru_ipc::VOICE, koru_channel::segment(), &root)
        .map_err(|error| ErrorCode::WorkerUnavailable.with(error.to_string()))
}

/// Connect, asking for a worker if nothing is listening.
///
/// **Connect first.** Whatever holds this endpoint is by construction the
/// worker for this channel and this root, so adopting it is right and asking
/// for another would be a second process that could not bind anyway.
fn reach(
    shared: &Shared,
    endpoint: &Endpoint,
    start: bool,
) -> Result<koru_ipc::Stream, VoiceError> {
    match koru_ipc::connect(endpoint) {
        Ok(stream) => return Ok(stream),
        Err(IpcError::NotRunning(_)) => {}
        Err(error) => return Err(ErrorCode::WorkerUnavailable.with(error.to_string())),
    }
    // **A question is not a reason to start an inference process.** A tray
    // drawing its menu and a settings page being built both want to know which
    // microphones there are, and neither is somebody asking to dictate; the
    // caller draws what it knows instead.
    if !start {
        return Err(ErrorCode::WorkerUnavailable
            .with("no voice worker is running, and this request may not start one"));
    }
    let asked = {
        let ensure = shared
            .ensure
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match ensure.as_ref() {
            Some(ensure) => {
                ensure();
                true
            }
            None => false,
        }
    };
    if !asked {
        // Nobody to ask, so there is nothing to wait for: a poll here would be
        // ten seconds of a person holding a key down for a process that is
        // never going to appear.
        return Err(ErrorCode::WorkerUnavailable
            .with("no voice worker is running and this client was given no way to ask for one"));
    }
    // **Binding and accepting are two moments**, and a client that arrives
    // between them has to wait rather than conclude there is no worker.
    let deadline = std::time::Instant::now() + START_DEADLINE;
    loop {
        match koru_ipc::connect(endpoint) {
            Ok(stream) => return Ok(stream),
            Err(IpcError::NotRunning(_)) if std::time::Instant::now() < deadline => {
                std::thread::sleep(RETRY_EVERY);
            }
            Err(IpcError::NotRunning(_)) => {
                return Err(ErrorCode::WorkerUnavailable.with(format!(
                    "the voice worker did not start within {} seconds",
                    START_DEADLINE.as_secs()
                )));
            }
            Err(error) => return Err(ErrorCode::WorkerUnavailable.with(error.to_string())),
        }
    }
}

/// Say hello, and decide whether the two ends can talk at all.
///
/// **A borrow, not a split.** `koru_ipc::split` consumes the stream and the
/// handshake's timeouts have to come off again afterwards, so this runs on
/// `&Stream` -- which is `Read` and `Write` -- and the split happens only once
/// the link is established.
fn greet(shared: &Shared, stream: &koru_ipc::Stream) -> Result<Worker, VoiceError> {
    let mut read = stream;
    let mut write = stream;
    let hello = ClientHello {
        product: shared.product.clone(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
    };
    koru_ipc::frame::write_frame(&mut write, FRAME_VERSION, &hello).map_err(|error| {
        ErrorCode::WorkerUnavailable.with(format!(
            "the voice worker would not take a greeting: {error}"
        ))
    })?;
    let (version, body) = koru_ipc::frame::read_raw(&mut read).map_err(|error| {
        ErrorCode::WorkerUnavailable.with(format!("the voice worker said nothing back: {error}"))
    })?;
    if version != FRAME_VERSION {
        // Closed, not answered: a refusal is itself a frame, and writing one in
        // a format the peer has just shown it does not share is a promise that
        // cannot be kept.
        return Err(ErrorCode::ProtocolMismatch.with(format!(
            "the worker frames its messages at version {version} and this build reads {FRAME_VERSION}"
        )));
    }
    let greeting: WorkerGreeting = koru_ipc::frame::decode(&body).map_err(|error| {
        ErrorCode::ProtocolMismatch.with(format!(
            "the voice worker's greeting could not be read: {error}"
        ))
    })?;
    match greeting {
        WorkerGreeting::Welcome {
            worker_version,
            protocol_version,
        } => Ok(Worker {
            version: worker_version,
            protocol_version,
        }),
        // Both numbers, in the order the worker names them: `ours` is the
        // worker's and `theirs` is this build's, which is the one sentence that
        // says which end is behind.
        WorkerGreeting::Refused {
            what, ours, theirs, ..
        } => Err(ErrorCode::ProtocolMismatch.with(format!(
            "the voice worker refused this client on the {what} version: it speaks {ours}, this build speaks {theirs}"
        ))),
    }
}

/// Read events until the socket closes.
fn read_events(shared: &Weak<Shared>, id: u64, read: &mut (impl std::io::Read + Send)) {
    loop {
        let (version, body) = match koru_ipc::frame::read_raw(read) {
            Ok(frame) => frame,
            // The ordinary end of a link: the worker closed it, or died.
            Err(_) => return,
        };
        if version != FRAME_VERSION {
            tracing::warn!(
                theirs = version,
                ours = FRAME_VERSION,
                "a voice frame arrived at a version this build cannot read"
            );
            return;
        }
        let Ok(event) = koru_ipc::frame::decode::<Event>(&body) else {
            tracing::warn!("a voice frame this build cannot read -- closing");
            return;
        };
        let Some(shared) = shared.upgrade() else {
            return;
        };
        if !shared.current_link(id) {
            return;
        }
        // **A question's answer is not a dictation's event**, so it is routed
        // before the generation filter below. That filter asks which dictation
        // an event belongs to, and a device list belongs to none -- with it
        // first, every answer would be dropped, because a query deliberately
        // arms no session.
        if let Event::Devices {
            session, devices, ..
        } = event
        {
            if !shared.answer_query(session, devices) {
                // The ordinary fate of an answer that arrived after its
                // timeout. A count, never a name: what is in it is a person's
                // hardware.
                tracing::debug!("a voice device list arrived with nobody waiting for it");
            }
            continue;
        }
        // **A meter's level is not a dictation's either**, for the same
        // reason, and it goes to the meter rather than the inbox.
        if shared.answer_meter(&event) {
            continue;
        }
        // **Dropped on the generation, the same rule the worker applies on the
        // way out.** Belt and braces across a socket that can reorder nothing
        // but can certainly deliver late: a decoder still running after a
        // cancel produces finals for a dictation nobody is holding any more.
        let Some(live) = shared.live() else { continue };
        if event.generation() != live.generation || event.session() != live.session {
            continue;
        }
        match &event {
            // Nothing started and nothing will end, so the session is over
            // before the host has even looked at it.
            Event::Refused { code, .. } => {
                shared.note(*code);
                shared.clear_live();
            }
            Event::Ended { reason, .. } => {
                if let EndReason::Failed { code, .. } = reason {
                    shared.note(*code);
                }
                shared.clear_live();
            }
            _ => {}
        }
        shared.deliver(event);
    }
}

fn request_live(request: &Request) -> Option<crate::Live> {
    match request {
        Request::Begin {
            session,
            generation,
            ..
        }
        | Request::Finish {
            session,
            generation,
        }
        | Request::Cancel {
            session,
            generation,
        } => Some(crate::Live {
            session: *session,
            generation: *generation,
        }),
        Request::Devices { .. } | Request::Meter { .. } | Request::MeterStop { .. } => None,
    }
}
