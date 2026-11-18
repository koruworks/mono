// SPDX-License-Identifier: MPL-2.0

//! **When the Agent re-applies the remembered Cloud sync choices** —
//! `InspectCloudSync`, the reconcile that activates a Workspace that appeared
//! after a Profile's switch was turned on.
//!
//! It ran every thirty seconds, whatever had happened: a Store request per
//! Profile and a hosted listing of the account's Workspaces, per device, all
//! day (four thousand Store requests a day on one quiet device, 2026-11-17).
//! What it exists to notice is a change on this device — a Workspace created,
//! moved into a Profile, restored, a sync choice changed — and the Store
//! already says when its catalog moved: the content-free catalog watch the
//! delegate host follows. So the reconcile runs when that watch says so.
//!
//! - **On a change**, a short moment after it, so a burst — a Profile's
//!   fifteen Workspaces restored at once — is one reconcile.
//! - **When it was owed and could not run** (the account still restoring, a
//!   person's command in the slot), again shortly, until it has.
//! - **Rarely, on a safety net**: an event the watch missed while it was
//!   reconnecting costs at most that long. It is not how anything is learned.
//!
//! What another device does to the account is not seen here; that is the
//! Cloud coordinator's discovery, which has its own cadence.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

/// The longest the reconcile waits with nothing having told it to run.
pub(crate) const SAFETY_NET: Duration = Duration::from_secs(15 * 60);

/// How long a change is left to gather its burst before the reconcile runs.
pub(crate) const SETTLE: Duration = Duration::from_millis(500);

/// How soon a reconcile that was owed and could not run is tried again.
pub(crate) const OWED_RETRY: Duration = Duration::from_secs(5);

/// How long the loop waits, as one value so a test can shrink all three.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    pub net: Duration,
    pub settle: Duration,
    pub owed_retry: Duration,
}

impl Timing {
    pub(crate) const PRODUCTION: Self = Self {
        net: SAFETY_NET,
        settle: SETTLE,
        owed_retry: OWED_RETRY,
    };
}

/// What one attempt came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Attempt {
    /// It was queued.
    Ran,
    /// It could not be queued now; it is still owed.
    Deferred,
    /// The Agent is gone: stop.
    Gone,
}

/// The loop, apart from the thread and the Store so a test can drive it.
///
/// Starts owed — the watch's first generation and the Agent's start both
/// mean *look now* — and answers when `attempt` reports the Agent gone or
/// every sender is dropped and nothing is owed.
pub(crate) fn run(
    changes: &Receiver<()>,
    timing: Timing,
    mut attempt: impl FnMut() -> Attempt,
    mut settle: impl FnMut(Duration),
) {
    let mut owed = true;
    loop {
        let wait = if owed { timing.owed_retry } else { timing.net };
        match changes.recv_timeout(wait) {
            Ok(()) => {
                settle(timing.settle);
                while changes.try_recv().is_ok() {}
                owed = true;
            }
            // The net, or the retry of something owed: either way, look.
            Err(RecvTimeoutError::Timeout) => owed = true,
            Err(RecvTimeoutError::Disconnected) if owed => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        match attempt() {
            Attempt::Ran => owed = false,
            Attempt::Deferred => {}
            Attempt::Gone => return,
        }
    }
}

/// Follow the Store's catalog watch and send one `()` per generation,
/// reopening it when it ends, and call `on_change` for each — the Cloud
/// coordinator's wake. Answers when the receiving side is gone.
pub(crate) fn follow_catalog(
    root: Option<std::path::PathBuf>,
    changes: Sender<()>,
    on_change: impl Fn(),
) {
    let mut delay = Duration::from_millis(500);
    loop {
        let client = match &root {
            Some(root) => koru_api_client::Client::open_at(koru_channel::segment(), root),
            None => koru_api_client::Client::open(koru_channel::segment()),
        };
        let watch = client.ok().and_then(|client| {
            koru_store_client::CatalogWatch::open(&client.store().discovery()).ok()
        });
        match watch {
            Some(mut watch) => {
                delay = Duration::from_millis(500);
                while watch.next_generation().is_ok() {
                    on_change();
                    if changes.send(()).is_err() {
                        return;
                    }
                }
                // Ended: whatever changed while it was down is looked at now.
                if changes.send(()).is_err() {
                    return;
                }
            }
            None => {
                // Not up yet, or gone for a moment. Nothing is owed by the
                // watch failing to open; the loop's own net still runs.
            }
        }
        std::thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::mpsc;

    const FAST: Timing = Timing {
        net: Duration::from_millis(5),
        settle: Duration::ZERO,
        owed_retry: Duration::from_millis(1),
    };

    /// A change runs one reconcile however many generations came with it;
    /// one owed is retried until it runs; the Agent gone stops the loop.
    #[test]
    fn a_burst_is_one_reconcile_and_an_owed_one_is_retried_until_it_runs() {
        let (send, receive) = mpsc::channel();
        // The start, then a burst of five generations.
        for _ in 0..5 {
            send.send(()).unwrap();
        }
        let answers = RefCell::new(vec![
            Attempt::Ran,      // the burst
            Attempt::Deferred, // owed again by the net below: not now
            Attempt::Deferred,
            Attempt::Ran,
            Attempt::Gone,
        ]);
        let attempts = RefCell::new(0);
        let settled = RefCell::new(0);
        run(
            &receive,
            FAST,
            || {
                *attempts.borrow_mut() += 1;
                answers.borrow_mut().remove(0)
            },
            |_| *settled.borrow_mut() += 1,
        );
        assert_eq!(*settled.borrow(), 1, "one settle for the whole burst");
        assert_eq!(*attempts.borrow(), 5);
        drop(send);
    }

    /// With the watch gone and nothing owed, the loop ends rather than
    /// spinning.
    #[test]
    fn a_closed_watch_with_nothing_owed_ends_the_loop() {
        let (send, receive) = mpsc::channel::<()>();
        drop(send);
        let attempts = RefCell::new(0);
        run(
            &receive,
            FAST,
            || {
                *attempts.borrow_mut() += 1;
                Attempt::Ran
            },
            |_| {},
        );
        assert_eq!(*attempts.borrow(), 1, "the start, owed, then nothing more");
    }
}
