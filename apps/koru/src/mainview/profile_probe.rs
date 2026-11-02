// SPDX-License-Identifier: MPL-2.0

//! Which `+` menu profile rows this machine can launch, probed off the loop
//! thread.
//!
//! The `+` menu's *Shell* and *Agents* flyouts list programs a machine
//! may not have, and a row that launches nothing should say so before it is
//! pressed — the same probe-and-dim Koru Terminal's launch menus do. The probe
//! itself
//! (`koru_session_profiles::probe`) walks `PATH` and stats files, and **a
//! thread that answers a person never waits on the world**
//! (`guide/contributing/PRINCIPLES.md`): so it runs on its own worker, the menu opens
//! immediately on the last known answer, and the worker's wake repaints the
//! rows when a fresh one lands.
//!
//! Why not the session backend's `BackendEvent::Profiles`? That backend is
//! made on first session *use* and probes once at spawn — the `+` menu needs
//! an answer before any session exists and a fresh one each time it opens.
//! The wake plumbing is the same either way: `AgentLink::wake_proxy`, the
//! proxy every per-window worker wakes the loop through.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use koru_session_profiles::{ProfileRow, built_in_profiles, probe};

/// The probe's cached answer, shared with the worker that refreshes it.
pub(super) struct ProfileProbe {
    /// Whether row `i` of `built_in_profiles()` may be launched, per the
    /// latest finished probe. `None` until the first probe answers; the menu
    /// then shows every row enabled rather than dimming over an answer nobody
    /// has given yet — "last known", where the first known is an optimism the
    /// opened tab itself corrects, because a missing program's session tab
    /// presents its launch failure honestly (W4.1).
    latest: Arc<Mutex<Option<Vec<bool>>>>,
    /// A worker is out. One probe at a time — reopening the menu quickly must
    /// not stack `PATH` walks.
    in_flight: Arc<AtomicBool>,
    /// A fresh answer changed something since the last drain. `take_dirty`
    /// hands it to `maintain` as "changed", so an open menu re-dims on the
    /// wake instead of waiting for the pointer to move.
    dirty: Arc<AtomicBool>,
}

impl ProfileProbe {
    pub(super) fn new() -> Self {
        Self {
            latest: Arc::new(Mutex::new(None)),
            in_flight: Arc::new(AtomicBool::new(false)),
            dirty: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Whether the `TerminalProfile(index)` row may be chosen right now.
    pub(super) fn row_enabled(&self, index: usize) -> bool {
        self.latest
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .is_none_or(|rows| rows.get(index).copied().unwrap_or(false))
    }

    /// The probe's answer for row `index`: `Some(present)` once a probe has
    /// answered, `None` before — what *Agent providers* reads as *Checking…*
    /// where the menu reads optimism.
    pub(super) fn presence(&self, index: usize) -> Option<bool> {
        self.latest
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|rows| rows.get(index).copied().unwrap_or(false))
    }

    /// Start a probe on a worker, unless one is already out. Never blocks:
    /// the caller (opening the `+` menu) shows the last known answer and the
    /// wake ends the loop's wait when the fresh one lands. `wake` is `None`
    /// exactly when the shell has no event loop — every test — and the cache
    /// still updates; only the unprompted repaint is lost.
    pub(super) fn refresh(
        &self,
        wake: Option<winit::event_loop::EventLoopProxy<koru_workspace_host::app::Wake>>,
    ) {
        if self
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let latest = Arc::clone(&self.latest);
        let in_flight = Arc::clone(&self.in_flight);
        let dirty = Arc::clone(&self.dirty);
        let spawned = std::thread::Builder::new()
            .name("koru-profile-probe".into())
            .spawn(move || {
                let rows = enabled_rows(&probe(built_in_profiles()));
                {
                    let mut cached = latest.lock().unwrap_or_else(|error| error.into_inner());
                    if cached.as_ref() != Some(&rows) {
                        *cached = Some(rows);
                        dirty.store(true, Ordering::Release);
                    }
                }
                in_flight.store(false, Ordering::Release);
                if let Some(proxy) = wake {
                    // Any wake only ends the loop's wait — `user_event` is
                    // deliberately empty and `about_to_wait` drains everything
                    // — and `Session` is the truthful name for who woke it:
                    // the session profiles' presence changed.
                    let _ = proxy.send_event(koru_workspace_host::app::Wake::Session);
                }
            });
        if spawned.is_err() {
            // No thread means no answer will ever clear the flag; the next
            // menu open should be allowed to try again.
            self.in_flight.store(false, Ordering::Release);
        }
    }

    /// Whether a fresh answer landed since the last ask — `maintain`'s reason
    /// to repaint an open menu.
    pub(super) fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    /// Pretend the probe answered `rows`, so a test decides which agents this
    /// "machine" has instead of depending on the test machine's real `PATH`.
    #[cfg(test)]
    pub(super) fn set_for_test(&self, rows: Vec<bool>) {
        *self
            .latest
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(rows);
    }
}

/// The enabled slice a probed catalogue means for the menu.
///
/// **One uniform rule: every row follows the probe's presence answer** —
/// `ProfileRow::launchable()` — shells and agents alike, which is exactly
/// what Koru Terminal's launch menus do (a Windows machine without Git Bash
/// dims its Bash row there too, and the two surfaces must match; maintainer,
/// 2026-09-04). A missing program of either kind dims with the same
/// *Not installed* note rather than opening a tab that fails.
fn enabled_rows(rows: &[ProfileRow]) -> Vec<bool> {
    rows.iter().map(ProfileRow::launchable).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_session_protocol::{ExecutableAvailability, ProfileKind};

    /// The availability is fabricated, never probed: the policy under test is
    /// the mapping from a probe answer to an enabled slice, and it must hold
    /// on a machine with everything installed and on one with nothing.
    fn catalogue_with(availability: fn() -> ExecutableAvailability) -> Vec<ProfileRow> {
        built_in_profiles()
            .into_iter()
            .map(|profile| ProfileRow {
                profile,
                availability: availability(),
            })
            .collect()
    }

    /// **One rule for every kind.** A probed-missing shell dims exactly like
    /// a probed-missing agent — the surfaces-match decision (maintainers, 2026-09-04)
    /// that ended the earlier shells-always-live special case.
    #[test]
    fn a_missing_program_dims_its_row_whatever_its_kind() {
        let rows = catalogue_with(|| ExecutableAvailability::Missing);
        let enabled = enabled_rows(&rows);
        assert_eq!(enabled.len(), rows.len());
        assert!(enabled.into_iter().all(|row| !row));
        // The catalogue really exercises both kinds under the one rule.
        assert!(
            rows.iter()
                .any(|row| row.profile.kind == ProfileKind::Shell)
        );
        assert!(
            rows.iter()
                .any(|row| row.profile.kind == ProfileKind::Agent)
        );
    }

    #[test]
    fn an_installed_program_is_launchable() {
        let rows = catalogue_with(|| ExecutableAvailability::Available { version: None });
        assert!(enabled_rows(&rows).into_iter().all(|enabled| enabled));
    }

    #[test]
    fn before_the_first_answer_every_row_is_enabled_and_injection_dims() {
        let probe = ProfileProbe::new();
        assert!(probe.row_enabled(0), "last known before there is one");
        assert!(!probe.take_dirty());
        probe.set_for_test(vec![true, false]);
        assert!(probe.row_enabled(0));
        assert!(!probe.row_enabled(1));
        assert!(
            !probe.row_enabled(99),
            "a row the answer does not know is not offered"
        );
    }
}
