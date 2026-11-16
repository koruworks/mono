// SPDX-License-Identifier: MPL-2.0

//! **What the coordinator talks to when it wants a file read** (TECHNICAL
//! §10 "Worker launch").
//!
//! A small trait, so the coordinator neither knows nor cares how a reader
//! runs. A desktop supervises the sibling `koru-read-worker`
//! ([`super::supervised::SupervisedReader`]); tests use [`StubLauncher`]; a
//! phone, and any host without the executable, has no launcher at all, and
//! there the mirror still runs and the coordinator reads nothing.
//!
//! The messages are the contract's own ([`koru_read::Request`],
//! [`koru_read::Event`]), so a stub and the real worker are held to one shape.

use std::path::PathBuf;

use koru_read::{Event, Request, WorkerStatus};

/// Why no reader can be asked right now. Each is a stable token the debug
/// route prints as "why not".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// This host runs no reader: a phone, a test, an in-memory composition.
    NoLauncher,
    /// The worker executable is not beside this one. Jobs wait (TECHNICAL
    /// §11); nothing is spawned, so nothing exhausts a restart budget.
    NotInstalled,
    /// The supervisor gave up restarting it within its budget.
    Exhausted,
    /// It was started and did not become ready, or did not answer as the
    /// process this supervision launched. The detail is the supervisor's own
    /// developer text, which names no file.
    NotReady(String),
}

impl Unavailable {
    pub fn code(&self) -> &'static str {
        match self {
            Unavailable::NoLauncher => "no-launcher",
            Unavailable::NotInstalled => "not-installed",
            Unavailable::Exhausted => "exhausted",
            Unavailable::NotReady(_) => "not-ready",
        }
    }
}

/// Whether a reader can be asked now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    Ready,
    Unavailable(Unavailable),
}

/// How a job ended without one of the worker's own terminal events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Interrupted {
    /// The worker process ended mid-job. `memory_limit` when its Job Object
    /// reported the ceiling first — the supervisor counts that as a crash,
    /// and so does the coordinator, but the debug route says which.
    Crashed { memory_limit: bool },
    /// The worker's own watchdog ended it on this file (exit code 3): the file
    /// is hostile, and is not fed again at this generation.
    Hostile,
    /// No reader could be asked after all. The job did not run.
    Unavailable(Unavailable),
}

/// What the debug route says about the worker. Ids and counts only.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct WorkerReport {
    pub up: bool,
    pub why_not: Option<&'static str>,
    pub generation: Option<u64>,
    pub respawns: u64,
    pub memory_kills: u64,
    pub last_exit_code: Option<i32>,
    pub memory_limit_bytes: Option<u64>,
    pub memory_limit_enforced: bool,
}

/// **A reader, from the coordinator's side.**
///
/// Every call is made on the coordinator's own thread and may block for as
/// long as a file takes; nothing here is ever called from a thread that
/// answers a person.
pub trait Launcher: Send {
    /// Whether a reader can be asked now. Cheap: for the real launcher, a
    /// look at whether the executable is there and at the supervisor's state.
    fn availability(&mut self) -> Availability;

    /// Run one [`Request::Read`] to its end, handing every event for its job
    /// to `events` as it arrives, in order. `Ok` means the job's one terminal
    /// event (`Done` or `Failed`) was delivered.
    fn read(&mut self, request: &Request, events: &mut dyn FnMut(Event))
    -> Result<(), Interrupted>;

    /// The coordinator's queue is empty: a reader that holds a process may let
    /// it go. The next [`read`](Self::read) starts one again.
    fn idle(&mut self) {}

    /// For the debug route.
    fn report(&self) -> WorkerReport;

    /// **The installed model's directory the worker is launched with**, or
    /// `None` when no reader is installed (OCR Plan 2.4). A change stops the
    /// running worker, so the next one starts with the new argument: a
    /// worker's reader never changes under a running job.
    fn set_model(&mut self, _model: Option<PathBuf>) {}

    /// **Start the worker if it is not running, and ask what it is**, with no
    /// job. How the coordinator learns, before it offers a download or sends
    /// a picture, whether this worker can recognize. On the coordinator's
    /// thread, like every other call here.
    fn probe(&mut self) -> Result<WorkerStatus, Unavailable> {
        Err(Unavailable::NoLauncher)
    }

    /// What the current worker answered its last `Status` with — the probe's,
    /// or the handshake before a job's — or `None` when no worker has been
    /// asked since it was started.
    fn status(&self) -> Option<WorkerStatus> {
        None
    }
}

/// The launcher of a host that runs no reader. The coordinator, given this,
/// reads nothing and says why.
pub struct NoLauncher;

impl Launcher for NoLauncher {
    fn availability(&mut self) -> Availability {
        Availability::Unavailable(Unavailable::NoLauncher)
    }

    fn read(&mut self, _: &Request, _: &mut dyn FnMut(Event)) -> Result<(), Interrupted> {
        Err(Interrupted::Unavailable(Unavailable::NoLauncher))
    }

    fn report(&self) -> WorkerReport {
        WorkerReport {
            why_not: Some(Unavailable::NoLauncher.code()),
            ..WorkerReport::default()
        }
    }
}

/// **A launcher for tests**: it answers every request with whatever its
/// script says, and records every request it was given. No process, no pipe,
/// no file.
#[cfg(test)]
type Script = Box<dyn FnMut(&Request) -> Result<Vec<Event>, Interrupted> + Send>;

#[cfg(test)]
pub struct StubLauncher {
    script: Script,
    pub requests: std::sync::Arc<std::sync::Mutex<Vec<Request>>>,
    pub availability: Availability,
    pub idled: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Whether this stub worker was built with recognition. With a model set,
    /// its status then says `recognition: true` — until a script answers a
    /// recognition job `Failed needs-reader`, as a real worker whose model
    /// would not load does; then `false` until it restarts.
    pub recognition_built: bool,
    /// The model the stub was last launched with, as `set_model` said.
    pub model: std::sync::Arc<std::sync::Mutex<Option<PathBuf>>>,
    /// Probes and restarts, counted.
    pub probes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub restarts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// **How many of the next probes fail `not-ready`**, each taking one: a
    /// worker that is starting, backing off after crashes, or not answering
    /// its status in time, as the real one is for a while after it was ended
    /// (OCR Plan 3.6). Failed probes are not counted in `probes`.
    pub failing_probes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Bumped on every restart, as a real launch id is new per launch.
    launch: u64,
    /// Asked since the last restart.
    asked: bool,
    /// The worker answered a recognition job `needs-reader`: its next status
    /// says `recognition: false`, until it restarts.
    lost: bool,
}

#[cfg(test)]
impl StubLauncher {
    pub fn new(
        script: impl FnMut(&Request) -> Result<Vec<Event>, Interrupted> + Send + 'static,
    ) -> Self {
        Self {
            script: Box::new(script),
            requests: Default::default(),
            availability: Availability::Ready,
            idled: Default::default(),
            recognition_built: false,
            model: Default::default(),
            probes: Default::default(),
            restarts: Default::default(),
            failing_probes: Default::default(),
            launch: 1,
            asked: false,
            lost: false,
        }
    }

    fn current_status(&self) -> WorkerStatus {
        let model = self
            .model
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        WorkerStatus {
            worker_version: "stub".into(),
            protocol_version: koru_read::PROTOCOL_VERSION,
            process_id: 1,
            supervisor_launch_id: Some(format!("stub-{}", self.launch)),
            recognition: self.recognition_built && model && !self.lost,
            recognition_built: self.recognition_built,
            job: None,
            pages_read: 0,
        }
    }
}

#[cfg(test)]
impl Launcher for StubLauncher {
    fn availability(&mut self) -> Availability {
        self.availability.clone()
    }

    fn read(
        &mut self,
        request: &Request,
        events: &mut dyn FnMut(Event),
    ) -> Result<(), Interrupted> {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(request.clone());
        self.asked = true;
        let answered = match (self.script)(request) {
            Ok(answered) => answered,
            Err(crashed @ Interrupted::Crashed { .. }) => {
                // **The worker is gone**, as the real launcher's is once it
                // ends one that went past its patience: the next is another
                // launch, and nothing is known of it until it is asked.
                self.launch += 1;
                self.asked = false;
                self.lost = false;
                return Err(crashed);
            }
            Err(other) => return Err(other),
        };
        let mut ended = false;
        for event in answered {
            ended |= event.is_terminal();
            if matches!(
                (&event, request),
                (
                    Event::Failed {
                        code: koru_read::FailureCode::NeedsReader,
                        ..
                    },
                    Request::Read {
                        want: koru_read::Want::Recognize,
                        ..
                    }
                )
            ) {
                self.lost = true;
            }
            events(event);
        }
        if ended {
            Ok(())
        } else {
            Err(Interrupted::Crashed {
                memory_limit: false,
            })
        }
    }

    fn idle(&mut self) {
        self.idled
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    fn set_model(&mut self, model: Option<PathBuf>) {
        let mut slot = self.model.lock().unwrap_or_else(|e| e.into_inner());
        if *slot != model {
            *slot = model;
            self.launch += 1;
            self.asked = false;
            self.lost = false;
            self.restarts
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn probe(&mut self) -> Result<WorkerStatus, Unavailable> {
        if let Availability::Unavailable(why) = &self.availability {
            return Err(why.clone());
        }
        let left = self
            .failing_probes
            .load(std::sync::atomic::Ordering::Relaxed);
        if left > 0 {
            self.failing_probes
                .store(left - 1, std::sync::atomic::Ordering::Relaxed);
            return Err(Unavailable::NotReady(
                "the stub worker did not answer its status".into(),
            ));
        }
        self.probes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.asked = true;
        Ok(self.current_status())
    }

    fn status(&self) -> Option<WorkerStatus> {
        self.asked.then(|| self.current_status())
    }

    fn report(&self) -> WorkerReport {
        WorkerReport {
            up: self.availability == Availability::Ready,
            why_not: match &self.availability {
                Availability::Ready => None,
                Availability::Unavailable(why) => Some(why.code()),
            },
            ..WorkerReport::default()
        }
    }
}
