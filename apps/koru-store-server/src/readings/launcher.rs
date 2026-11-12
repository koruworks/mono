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

use koru_read::{Event, Request};

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
        let answered = (self.script)(request)?;
        let mut ended = false;
        for event in answered {
            ended |= event.is_terminal();
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
