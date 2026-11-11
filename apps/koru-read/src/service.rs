// SPDX-License-Identifier: MPL-2.0

//! **The message rules**, independent of any socket (TECHNICAL §10 "Worker
//! IPC"; the rules are `koru_read::wire`'s):
//!
//! - `Status` is answered at once with [`WorkerStatus`], and starts nothing;
//! - **one job at a time**: a `Read` while another runs is answered
//!   `Failed { Busy }` for the *new* job, and the running one continues;
//! - every `Read` ends with exactly one `Done` or `Failed`;
//! - `Cancel` ends a running job with `Failed { Cancelled }`; cancelling a job
//!   that is not running answers nothing;
//! - a connection that goes away cancels the job it started, since nobody is
//!   left to hear the result.
//!
//! A job runs on a thread of its own, so the connection that asked keeps
//! reading — which is how a `Cancel` or a `Status` arrives while a long file
//! is being read. Events go to the asking connection's sink.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use koru_read::{Event, FailureCode, JobId, PROTOCOL_VERSION, Request, WorkerStatus};

use crate::job::{self, Engine, ReadJob};

/// Which connection a request came from.
pub type ConnectionId = u64;

struct Running {
    job: JobId,
    connection: ConnectionId,
    cancel: Arc<AtomicBool>,
}

struct State {
    running: Option<Running>,
    pages_read: u64,
    /// When the last job ended, or the worker started.
    idle_since: Instant,
}

struct Shared {
    engine: Engine,
    launch_id: Option<String>,
    state: Mutex<State>,
}

/// The worker's one job slot and what it says about itself.
#[derive(Clone)]
pub struct Service {
    shared: Arc<Shared>,
}

impl Service {
    pub fn new(engine: Engine, launch_id: Option<String>) -> Service {
        Service {
            shared: Arc::new(Shared {
                engine,
                launch_id,
                state: Mutex::new(State {
                    running: None,
                    pages_read: 0,
                    idle_since: Instant::now(),
                }),
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// What this worker is doing, as the answer to `Status`.
    pub fn status(&self) -> WorkerStatus {
        let state = self.state();
        WorkerStatus {
            worker_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION,
            process_id: std::process::id(),
            supervisor_launch_id: self.shared.launch_id.clone(),
            // This build has no recognition.
            recognition: false,
            job: state.running.as_ref().map(|running| running.job),
            pages_read: state.pages_read,
        }
    }

    /// How long the worker has had no job, or `None` while one runs.
    pub fn idle_for(&self, now: Instant) -> Option<Duration> {
        let state = self.state();
        match state.running {
            Some(_) => None,
            None => Some(now.saturating_duration_since(state.idle_since)),
        }
    }

    /// Answer one request from `connection`, whose events go to `sink`.
    pub fn handle(&self, connection: ConnectionId, request: Request, sink: &Sender<Event>) {
        match request {
            Request::Status => {
                let _ = sink.send(Event::Status(self.status()));
            }
            Request::Cancel { job } => {
                let state = self.state();
                if let Some(running) = state.running.as_ref().filter(|running| running.job == job) {
                    running.cancel.store(true, Ordering::Release);
                }
            }
            Request::Read {
                job,
                path,
                mime,
                from_page,
                max_text_pages,
                max_read_pages: _,
                want,
            } => self.start(
                connection,
                ReadJob {
                    job,
                    path,
                    mime,
                    from_page,
                    max_text_pages,
                    want,
                },
                sink.clone(),
            ),
        }
    }

    /// The connection is gone: a job it started has nobody to report to.
    pub fn connection_closed(&self, connection: ConnectionId) {
        let state = self.state();
        if let Some(running) = state
            .running
            .as_ref()
            .filter(|running| running.connection == connection)
        {
            running.cancel.store(true, Ordering::Release);
        }
    }

    fn start(&self, connection: ConnectionId, request: ReadJob, sink: Sender<Event>) {
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut state = self.state();
            if state.running.is_some() {
                drop(state);
                let _ = sink.send(Event::Failed {
                    job: request.job,
                    code: FailureCode::Busy,
                });
                return;
            }
            state.running = Some(Running {
                job: request.job,
                connection,
                cancel: Arc::clone(&cancel),
            });
        }
        let service = self.clone();
        let id = request.job;
        let spawned = std::thread::Builder::new()
            .name(format!("koru-read-job-{id}"))
            .spawn({
                let sink = sink.clone();
                move || service.run(request, &cancel, &sink)
            });
        if spawned.is_err() {
            self.finish();
            let _ = sink.send(Event::Failed {
                job: id,
                code: FailureCode::Internal,
            });
        }
    }

    /// The job thread: run, count pages, then free the slot **before** the
    /// terminal event goes out, so a `Read` sent the moment `Done` arrives is
    /// never refused as busy.
    fn run(&self, request: ReadJob, cancel: &AtomicBool, sink: &Sender<Event>) {
        let id = request.job;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            job::run(&self.shared.engine, &request, cancel, &mut |event| {
                self.state().pages_read += 1;
                sink.send(event).is_ok()
            })
        }));
        let end = outcome.unwrap_or(Event::Failed {
            job: id,
            code: FailureCode::Internal,
        });
        tracing::debug!(job = %id, end = end_label(&end), "a read job ended");
        self.finish();
        let _ = sink.send(end);
    }

    fn finish(&self) {
        let mut state = self.state();
        state.running = None;
        state.idle_since = Instant::now();
    }
}

/// A token for the debug line: no words, no path.
fn end_label(event: &Event) -> &'static str {
    match event {
        Event::Done { status, .. } => status.as_str(),
        Event::Failed { code, .. } => code.as_str(),
        Event::Page { .. } | Event::Status(_) => "unexpected",
    }
}

#[cfg(test)]
mod tests;
