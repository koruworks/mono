// SPDX-License-Identifier: MPL-2.0

//! **Ending the process when there is nothing to read.**
//!
//! The worker parses untrusted files, and a hostile one can leave it holding
//! memory or a page thread that never returns. Ending the process is the only
//! way to give all of that back (TECHNICAL D4), and the Store starts a new
//! worker for the next file. So it exits after about a minute with no job
//! (TECHNICAL §14, "Staying out of the way").
//!
//! **Only a job is work.** A status question — a readiness or health probe —
//! does not keep the worker alive, or a supervisor polling every few seconds
//! would hold it for ever.
//!
//! It is a deadline, not a timer: the main thread blocks for exactly
//! [`IdleAction::Wait`]'s duration and wakes once.

use std::time::Duration;

/// How long the worker stays with nothing to do.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct IdlePolicy {
    /// Zero means never — a developer keeping a worker alive by hand.
    pub exit_after: Duration,
}

/// What to do about being idle right now.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum IdleAction {
    /// Nothing is due for this long.
    Wait(Duration),
    /// End the process.
    Exit,
}

impl IdlePolicy {
    /// About a minute: a backlog keeps the worker busy, and a person adding
    /// files one by one keeps it warm, while a quiet machine holds nothing.
    pub const DEFAULT_EXIT: Duration = Duration::from_secs(60);

    /// The default, with `KORU_READ_IDLE_EXIT` over it when set. A value that
    /// is not a whole number of seconds is named and ignored rather than fatal.
    pub fn from_env() -> IdlePolicy {
        let raw = std::env::var(koru_env::name::KORU_READ_IDLE_EXIT).ok();
        IdlePolicy {
            exit_after: parse_seconds(raw.as_deref(), Self::DEFAULT_EXIT),
        }
    }

    /// What to do. `idle_for` is `None` while a job runs.
    pub fn decide(&self, idle_for: Option<Duration>) -> IdleAction {
        // How often a worker that never exits, or a busy one, looks again.
        const RECHECK: Duration = Duration::from_secs(60);
        if self.exit_after.is_zero() {
            return IdleAction::Wait(RECHECK);
        }
        match idle_for {
            None => IdleAction::Wait(self.exit_after.min(RECHECK)),
            Some(idle) if idle >= self.exit_after => IdleAction::Exit,
            // Never zero: a zero wait would turn the main thread's
            // `recv_timeout` into a spin.
            Some(idle) => IdleAction::Wait((self.exit_after - idle).max(Duration::from_millis(1))),
        }
    }
}

/// One value as whole seconds. Takes the raw value so the rule is tested
/// without touching the process environment.
fn parse_seconds(raw: Option<&str>, fallback: Duration) -> Duration {
    let Some(trimmed) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return fallback;
    };
    match trimmed.parse::<u64>() {
        Ok(seconds) => Duration::from_secs(seconds),
        Err(_) => {
            tracing::warn!(
                variable = koru_env::name::KORU_READ_IDLE_EXIT,
                "not a whole number of seconds; using the default"
            );
            fallback
        }
    }
}

#[cfg(test)]
mod tests;
