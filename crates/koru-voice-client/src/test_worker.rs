// SPDX-License-Identifier: MPL-2.0

//! **A worker that is real about the socket and fictional about everything
//! else.**
//!
//! No process is launched and no microphone exists. What is real is the
//! `koru-ipc` endpoint -- a named pipe on Windows, a socket elsewhere -- the
//! framing, the greeting, and the fact that a second client of one worker meets
//! the *same* state the first one armed. Those are the properties Plan 1.4's
//! tests are about, and none of them needs an engine.
//!
//! **Every root here is a fresh uuid path nothing has ever bound**, and it is
//! never created on disk: `Endpoint::for_service_root` only hashes the path. So
//! a suite can neither reach nor stop the worker somebody is dictating through,
//! and `a_client_on_another_root_finds_nothing` arms the state it tests rather
//! than asserting the absence of somebody else's service.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use koru_voice::{
    ClientHello, EndReason, ErrorCode, Event, FRAME_VERSION, InputDevice, LevelBucket,
    PROTOCOL_VERSION, Request, SessionId, State, Transcript, WorkerGreeting,
};

/// The level this fictional microphone always hears.
pub const METER_LEVEL: u8 = 5;

/// **What this fictional installation says is plugged in.**
///
/// A fixture, and it is the only kind of device list any test in this
/// workspace may see: `no-capture` is what stops `apps/koru-voice` from
/// enumerating, and this crate never links `cpal` at all. Two devices, one of
/// them the default, because a picker with one row cannot get the highlight
/// wrong.
pub fn fixture_devices() -> Vec<InputDevice> {
    vec![
        InputDevice::new("Microphone Array (Realtek(R) Audio)", true),
        InputDevice::new("Yeti Nano", false),
    ]
}

/// A path nothing has ever bound and nothing will create.
pub fn unbound_root() -> PathBuf {
    std::env::temp_dir().join(format!("koru-voice-client-test-{}", uuid::Uuid::new_v4()))
}

#[derive(Default)]
struct Held {
    /// The dictation this fictional installation is running, if any. **One at a
    /// time per worker**, which is the whole of the scope guarantee: it is a
    /// fact about this endpoint and nothing wider.
    live: Option<(SessionId, u64)>,
    /// Close the next connection that sends anything -- what "the worker was
    /// killed mid-session" looks like from a client's side.
    hangup: bool,
    requests: Vec<&'static str>,
}

pub struct TestWorker {
    root: PathBuf,
    held: Arc<Mutex<Held>>,
    stopping: Arc<AtomicBool>,
    /// How many clients completed the handshake. A counter, so a reconnect can
    /// be asserted as a fact rather than inferred from a request succeeding.
    handshakes: Arc<AtomicU64>,
    accepting: Option<std::thread::JoinHandle<()>>,
}

impl TestWorker {
    /// Bind the voice endpoint on `root` and answer until [`Self::stop`].
    pub fn start(root: &Path) -> TestWorker {
        TestWorker::start_speaking(root, PROTOCOL_VERSION)
    }

    /// A worker that speaks `protocol` — an older build's, to show what a
    /// client meets when the two were not shipped together.
    pub fn start_speaking(root: &Path, protocol: u32) -> TestWorker {
        let endpoint =
            koru_ipc::Endpoint::for_service_root(koru_ipc::VOICE, koru_channel::segment(), root)
                .expect("the voice endpoint could not be named");
        let listener =
            koru_ipc::Listener::bind(endpoint).expect("the voice endpoint could not be claimed");
        let held = Arc::new(Mutex::new(Held::default()));
        let stopping = Arc::new(AtomicBool::new(false));
        let handshakes = Arc::new(AtomicU64::new(0));
        let accepting = {
            let held = Arc::clone(&held);
            let stopping = Arc::clone(&stopping);
            let handshakes = Arc::clone(&handshakes);
            std::thread::spawn(move || {
                while let Ok(stream) = listener.accept() {
                    if stopping.load(Ordering::Acquire) {
                        break;
                    }
                    let held = Arc::clone(&held);
                    let handshakes = Arc::clone(&handshakes);
                    std::thread::spawn(move || connection(stream, &held, &handshakes, protocol));
                }
            })
        };
        TestWorker {
            root: root.to_path_buf(),
            held,
            stopping,
            handshakes,
            accepting: Some(accepting),
        }
    }

    pub fn handshakes(&self) -> u64 {
        self.handshakes.load(Ordering::Acquire)
    }

    /// Whether this worker believes a dictation is running.
    pub fn busy(&self) -> bool {
        self.held
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .live
            .is_some()
    }

    pub fn requests(&self) -> Vec<&'static str> {
        self.held
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .requests
            .clone()
    }

    /// Drop the next connection that says anything, without a word.
    pub fn hang_up_on_the_next_request(&self) {
        self.held
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .hangup = true;
    }

    /// Stop listening and let go of the endpoint, so another worker can bind
    /// the same name.
    pub fn stop(mut self) {
        self.stopping.store(true, Ordering::Release);
        // The accept loop is parked in `accept()`; one connection is what
        // unparks it. Nothing is sent -- the loop checks the flag first.
        if let Ok(endpoint) = koru_ipc::Endpoint::for_service_root(
            koru_ipc::VOICE,
            koru_channel::segment(),
            &self.root,
        ) {
            let _ = koru_ipc::connect(&endpoint);
        }
        if let Some(accepting) = self.accepting.take() {
            let _ = accepting.join();
        }
    }
}

fn connection(
    stream: koru_ipc::Stream,
    held: &Arc<Mutex<Held>>,
    handshakes: &Arc<AtomicU64>,
    protocol: u32,
) {
    let (mut read, mut write) = koru_ipc::split(stream);
    let Ok((version, hello)) = koru_ipc::frame::read_frame::<_, ClientHello>(&mut read) else {
        return;
    };
    if version != FRAME_VERSION || hello.protocol_version != protocol {
        let refusal = WorkerGreeting::Refused {
            what: "protocol".into(),
            ours: protocol,
            theirs: hello.protocol_version,
            message: "a test worker".into(),
        };
        let _ = koru_ipc::frame::write_frame(&mut write, FRAME_VERSION, &refusal);
        return;
    }
    let welcome = WorkerGreeting::Welcome {
        worker_version: "0.0.0-test".into(),
        protocol_version: protocol,
    };
    if koru_ipc::frame::write_frame(&mut write, FRAME_VERSION, &welcome).is_err() {
        return;
    }
    handshakes.fetch_add(1, Ordering::Release);

    let mut mine: Option<(SessionId, u64)> = None;
    loop {
        let Ok((_, request)) = koru_ipc::frame::read_frame::<_, Request>(&mut read) else {
            break;
        };
        {
            let mut state = held.lock().unwrap_or_else(|error| error.into_inner());
            if state.hangup {
                state.hangup = false;
                break;
            }
        }
        let replies = answer(request, held, &mut mine);
        let mut lost = false;
        for reply in replies {
            if koru_ipc::frame::write_frame(&mut write, FRAME_VERSION, &reply).is_err() {
                lost = true;
                break;
            }
        }
        if lost {
            break;
        }
    }
    // **An interruption discards**, exactly as the real worker's
    // `handle_disconnect` does: a session whose client went away is not one
    // anybody is going to finish, and leaving it armed would tell the next
    // client it was busy forever.
    if let Some((session, _)) = mine {
        let mut state = held.lock().unwrap_or_else(|error| error.into_inner());
        if state.live.map(|(id, _)| id) == Some(session) {
            state.live = None;
        }
    }
}

fn answer(
    request: Request,
    held: &Arc<Mutex<Held>>,
    mine: &mut Option<(SessionId, u64)>,
) -> Vec<Event> {
    let mut state = held.lock().unwrap_or_else(|error| error.into_inner());
    state.requests.push(match &request {
        Request::Begin { .. } => "begin",
        Request::Finish { .. } => "finish",
        Request::Cancel { .. } => "cancel",
        Request::Devices { .. } => "devices",
        Request::Meter { .. } => "meter",
        Request::MeterStop { .. } => "meter_stop",
    });
    match request {
        Request::Begin {
            session,
            generation,
            ..
        } => {
            if state.live.is_some() {
                // **The refusal goes to whoever was refused**, never down the
                // live session's socket -- telling the person who *is*
                // dictating that their dictation was refused is the failure
                // this arrangement exists to avoid.
                return vec![Event::Refused {
                    session,
                    generation,
                    code: ErrorCode::Busy,
                    message: "a dictation is already running on this installation".into(),
                }];
            }
            state.live = Some((session, generation));
            *mine = Some((session, generation));
            vec![
                Event::State {
                    session,
                    generation,
                    state: State::Listening,
                },
                Event::Partial {
                    session,
                    generation,
                    text: Transcript::new("over the"),
                },
            ]
        }
        Request::Finish {
            session,
            generation,
        } => {
            if state.live != Some((session, generation)) {
                return vec![Event::Refused {
                    session,
                    generation,
                    code: ErrorCode::NoSession,
                    message: "no such dictation".into(),
                }];
            }
            state.live = None;
            *mine = None;
            vec![
                Event::Final {
                    session,
                    generation,
                    text: Transcript::new("over the socket"),
                },
                Event::Ended {
                    session,
                    generation,
                    reason: EndReason::Completed,
                },
            ]
        }
        // **A question, not a dictation.** It answers whatever the live
        // session is doing and leaves `state.live` exactly where it was --
        // which is what the client's own test asserts against.
        Request::Devices {
            session,
            generation,
        } => {
            return vec![Event::Devices {
                session,
                generation,
                devices: fixture_devices(),
            }];
        }
        // **A meter, which arms nothing.** A busy microphone refuses it, as
        // the real worker's does; otherwise one level comes straight back.
        Request::Meter {
            session,
            generation,
            ..
        } => {
            if state.live.is_some() {
                return vec![Event::Refused {
                    session,
                    generation,
                    code: ErrorCode::Busy,
                    message: "a dictation has the microphone".into(),
                }];
            }
            vec![Event::Level {
                session,
                generation,
                bucket: LevelBucket::new(METER_LEVEL),
            }]
        }
        Request::MeterStop { .. } => Vec::new(),
        Request::Cancel {
            session,
            generation,
        } => {
            if state.live == Some((session, generation)) {
                state.live = None;
                *mine = None;
            }
            vec![Event::Ended {
                session,
                generation,
                reason: EndReason::Cancelled,
            }]
        }
    }
}
