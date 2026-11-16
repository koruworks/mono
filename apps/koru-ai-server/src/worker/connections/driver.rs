// SPDX-License-Identifier: MPL-2.0

//! The connection service's own thread: `recover()` once at start, then
//! `drive()` on a short timer for as long as the worker lives.
//!
//! Nothing else finishes a consent trip nobody completed, repeats a receipt
//! the delegate did not acknowledge, or closes an obligation whose call
//! ended without settling it. `drive()` never calls a vendor; `recover()`
//! may (it finishes a Disconnect a previous process fenced), which is one
//! more reason neither runs on the writer thread.

use super::*;

/// The wait before `recover()` is tried again after it could not read ai.db.
const RECOVER_RETRY_FIRST: Duration = Duration::from_secs(1);
const RECOVER_RETRY_MAX: Duration = Duration::from_secs(32);

pub(super) fn spawn(
    service: Arc<ConnectionService>,
    shutdown: Arc<AtomicBool>,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name("koru-ai-server connections".into())
        .spawn(move || run(&service, &shutdown))
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn run(service: &ConnectionService, shutdown: &AtomicBool) {
    let mut recovered = false;
    let mut retry = RECOVER_RETRY_FIRST;
    let mut recover_at = Instant::now();
    while !shutdown.load(Ordering::Acquire) {
        if !recovered && Instant::now() >= recover_at {
            // A panic in an adapter must not end the upkeep of every other
            // obligation: the pass is abandoned and tried again.
            match catch_unwind(AssertUnwindSafe(|| service.recover())) {
                Ok(Ok(found)) => {
                    recovered = true;
                    // Counts only: a receipt names no account and no file.
                    tracing::info!(
                        target: "koru_ai::connections",
                        trips_interrupted = found.trips_interrupted,
                        slots_cleared = found.slots_cleared,
                        disconnects_finished = found.disconnects_finished,
                        deferred = found.deferred,
                        "connection commands of a previous process were settled"
                    );
                }
                Ok(Err(error)) => {
                    tracing::debug!(
                        target: "koru_ai::connections",
                        code = error.code(),
                        "connection recovery is not possible yet"
                    );
                    recover_at = Instant::now() + retry;
                    retry = (retry * 2).min(RECOVER_RETRY_MAX);
                }
                Err(_) => {
                    tracing::error!(
                        target: "koru_ai::connections",
                        "connection recovery panicked; it will be tried again"
                    );
                    recover_at = Instant::now() + retry;
                    retry = (retry * 2).min(RECOVER_RETRY_MAX);
                }
            }
        }
        if catch_unwind(AssertUnwindSafe(|| service.drive())).is_err() {
            tracing::error!(
                target: "koru_ai::connections",
                "connection upkeep panicked; the next pass runs as usual"
            );
        }
        std::thread::sleep(DRIVE_EVERY);
    }
}
