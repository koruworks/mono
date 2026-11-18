// SPDX-License-Identifier: MPL-2.0

//! **Time budgets for work that cannot be cancelled.**
//!
//! The PDF library's own README warns that timeouts are still easy to
//! trigger. A page that never finishes is a call into `hayro` that never
//! returns, and nothing can stop a thread from the outside. So the work runs
//! on a thread of its own and the job waits for it with a deadline; when the
//! deadline passes, the job **abandons** the thread — stops waiting, tells it
//! to stop at its next chance, and carries on with the next page on a fresh
//! thread.
//!
//! An abandoned thread still holds the CPU and whatever memory it took. Three
//! backstops bound that, from the inside out:
//!
//! 1. [`Abandoned`] keeps the abandoned threads that are still running. A
//!    third one alive at once ([`MAX_ABANDONED_ALIVE`]), or one still running
//!    [`ABANDONED_CEILING`] after it was abandoned, ends the process: a
//!    watchdog that cannot be argued with. The Store sees the worker exit,
//!    and the coordinator starts a new one (TECHNICAL §11).
//! 2. The idle exit ends the process a minute after the last job anyway.
//! 3. The supervisor's Job Object memory ceiling ends a worker that a
//!    runaway page made too large, whatever the threads are doing.
//!
//! The per-page budget is Plan 0.1's (721 real PDFs: the worst text-layer
//! page took tens of milliseconds, the worst parse under half a second, and
//! nothing timed out or crashed). The memory limit is the supervisor's, not
//! this crate's: Plan 0.1 measured a text-only worker at 180 MiB on the worst
//! real file, and the Store will set 1 GiB.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long one page may take before it is abandoned. *Measured, Plan 0.1.*
pub const PAGE_BUDGET: Duration = Duration::from_secs(30);

/// How long opening a file (parsing a PDF's cross-reference table, repairing
/// a broken one) may take. Plan 0.1 saw under half a second at worst; this
/// gives the same margin the page budget has.
pub const LOAD_BUDGET: Duration = Duration::from_secs(30);

/// More abandoned threads than this, still running at once, end the process.
pub const MAX_ABANDONED_ALIVE: usize = 2;

/// An abandoned thread still running this long after it was abandoned ends
/// the process: four page budgets. Not measured — nothing real was abandoned.
pub const ABANDONED_CEILING: Duration = Duration::from_secs(120);

/// The exit code of a worker ended by its own watchdog, so the Store's log
/// can tell it from a crash.
pub const EXIT_WEDGED: i32 = 3;

/// A page thread's stack. Interpreting nested forms and Type3 glyphs recurses,
/// and a stack overflow aborts the whole process rather than one thread; the
/// reservation is virtual, so only what is touched is committed.
pub const WORK_STACK: usize = 16 * 1024 * 1024;

/// How often a wait looks at the cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(25);

/// How a wait for bounded work ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Waited<T> {
    Got(T),
    /// The budget passed.
    TimedOut,
    /// The other side ended without sending: it panicked, or it had nothing
    /// more to send.
    Ended,
    /// The job was cancelled while waiting.
    Cancelled,
}

/// Wait for one value for at most `budget`, looking at `cancel` as it goes.
pub fn wait<T>(rx: &Receiver<T>, budget: Duration, cancel: &AtomicBool) -> Waited<T> {
    let deadline = Instant::now() + budget;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Waited::Cancelled;
        }
        let now = Instant::now();
        if now >= deadline {
            return Waited::TimedOut;
        }
        match rx.recv_timeout((deadline - now).min(CANCEL_POLL)) {
            Ok(value) => return Waited::Got(value),
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Waited::Ended,
        }
    }
}

/// How a bounded call ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome<T> {
    Done(T),
    /// It panicked, or its thread could not be started.
    Failed,
    /// It ran past its budget and was abandoned.
    TimedOut,
    Cancelled,
}

/// Run `work` on a thread of its own, waiting at most `budget` for it.
pub fn bounded<T: Send + 'static>(
    name: &str,
    budget: Duration,
    cancel: &AtomicBool,
    abandoned: &Arc<Abandoned>,
    work: impl FnOnce() -> T + Send + 'static,
) -> Outcome<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let finished = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&finished);
    let spawned = std::thread::Builder::new()
        .name(name.to_owned())
        .stack_size(WORK_STACK)
        .spawn(move || {
            let result = std::panic::catch_unwind(AssertUnwindSafe(work));
            flag.store(true, Ordering::Release);
            let _ = tx.send(result);
        });
    if spawned.is_err() {
        return Outcome::Failed;
    }
    match wait(&rx, budget, cancel) {
        Waited::Got(Ok(value)) => Outcome::Done(value),
        Waited::Got(Err(_)) | Waited::Ended => Outcome::Failed,
        Waited::TimedOut => {
            abandoned.note(finished);
            Outcome::TimedOut
        }
        Waited::Cancelled => {
            abandoned.note(finished);
            Outcome::Cancelled
        }
    }
}

/// Why the watchdog ended the process.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Wedged {
    /// More than [`MAX_ABANDONED_ALIVE`] abandoned threads at once.
    TooMany,
    /// One still running [`ABANDONED_CEILING`] after it was abandoned.
    TooLong,
}

/// **The threads this process gave up on and that have not finished.**
pub struct Abandoned {
    alive: Mutex<Vec<Arc<AtomicBool>>>,
    max_alive: usize,
    ceiling: Duration,
    on_wedged: Arc<dyn Fn(Wedged) + Send + Sync>,
}

impl Abandoned {
    /// The worker's own: a wedge is logged and ends the process.
    pub fn product() -> Arc<Abandoned> {
        Abandoned::new(MAX_ABANDONED_ALIVE, ABANDONED_CEILING, |why| {
            tracing::error!(
                ?why,
                "work that was abandoned is still running; the read worker is ending itself"
            );
            std::process::exit(EXIT_WEDGED);
        })
    }

    /// Any limits and any wedge action — what a test arms.
    pub fn new(
        max_alive: usize,
        ceiling: Duration,
        on_wedged: impl Fn(Wedged) + Send + Sync + 'static,
    ) -> Arc<Abandoned> {
        Arc::new(Abandoned {
            alive: Mutex::new(Vec::new()),
            max_alive,
            ceiling,
            on_wedged: Arc::new(on_wedged),
        })
    }

    /// Record a thread that was given up on. `finished` is set by the thread
    /// when it ends.
    pub fn note(&self, finished: Arc<AtomicBool>) {
        let alive = {
            let mut alive = self
                .alive
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            alive.retain(|done| !done.load(Ordering::Acquire));
            if !finished.load(Ordering::Acquire) {
                alive.push(Arc::clone(&finished));
            }
            alive.len()
        };
        if alive > self.max_alive {
            (self.on_wedged)(Wedged::TooMany);
            return;
        }
        if finished.load(Ordering::Acquire) {
            return;
        }
        let ceiling = self.ceiling;
        let on_wedged = Arc::clone(&self.on_wedged);
        // The watchdog for this one thread. It sleeps once and looks once.
        let _ = std::thread::Builder::new()
            .name("koru-read-watchdog".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                std::thread::sleep(ceiling);
                if !finished.load(Ordering::Acquire) {
                    on_wedged(Wedged::TooLong);
                }
            });
    }

    /// How many abandoned threads are still running.
    pub fn alive(&self) -> usize {
        let mut alive = self
            .alive
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        alive.retain(|done| !done.load(Ordering::Acquire));
        alive.len()
    }
}

#[cfg(test)]
mod tests;
