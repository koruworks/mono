// SPDX-License-Identifier: MPL-2.0

//! Session admission, independent of process launch and wall-clock time.
//! Reservations and sessions being stopped still occupy a slot. Only a
//! confirmed process end releases it; selecting a victim never does.

use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub const IDLE_WINDOW_MS: u64 = 5 * 60 * 1000;
pub const ROUTINE_GRACE_MS: u64 = 60 * 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionKey {
    pub bot: Uuid,
    pub thread: Uuid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waiting {
    pub turn: Uuid,
    pub session: SessionKey,
    pub queued_at_ms: u64,
    /// None is a person's message. A Routine expires after due + one minute.
    pub routine_due_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Idle {
    pub since_ms: u64,
    pub side_thread: bool,
    pub offered_or_unknown_delivery: bool,
    pub leased_or_unresolved_operation: bool,
    pub resume_ready: bool,
    pub state_saved: bool,
}

impl Idle {
    pub fn may_yield(self) -> bool {
        !self.offered_or_unknown_delivery
            && !self.leased_or_unresolved_operation
            && self.resume_ready
            && self.state_saved
    }
    fn expires(self, now: u64) -> bool {
        !self.offered_or_unknown_delivery
            && !self.leased_or_unresolved_operation
            && ((self.side_thread && self.resume_ready && self.state_saved)
                || now.saturating_sub(self.since_ms) >= IDLE_WINDOW_MS)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Live {
    Running(Uuid),
    Idle(Idle),
    Stopping,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Start { request: Waiting, reuse: bool },
    End { session: SessionKey },
    Missed { turn: Uuid },
}

pub struct Scheduler {
    limit: u32,
    waiting: BTreeMap<Uuid, Waiting>,
    live: BTreeMap<SessionKey, Live>,
}

impl Scheduler {
    pub fn new(limit: u32) -> Self {
        Self {
            limit: limit.max(1),
            waiting: BTreeMap::new(),
            live: BTreeMap::new(),
        }
    }
    pub fn set_capacity(&mut self, limit: u32) {
        self.limit = limit.max(1);
    }
    pub fn occupied(&self) -> usize {
        self.live.len()
    }
    pub fn queued(&self) -> usize {
        self.waiting.len()
    }
    pub fn enqueue(&mut self, request: Waiting) -> Result<(), crate::Error> {
        if request.turn.is_nil() || request.session.bot.is_nil() || request.session.thread.is_nil()
        {
            return Err(crate::Error::InvalidRequest);
        }
        if let Some(old) = self.waiting.get(&request.turn) {
            return if old == &request {
                Ok(())
            } else {
                Err(crate::Error::InvalidRequest)
            };
        }
        if self
            .live
            .values()
            .any(|state| *state == Live::Running(request.turn))
        {
            return Err(crate::Error::InvalidRequest);
        }
        self.waiting.insert(request.turn, request);
        Ok(())
    }
    pub fn cancel_waiting(&mut self, turn: Uuid) {
        self.waiting.remove(&turn);
    }
    /// Only the owner of this exact running turn can mark its session idle.
    pub fn completed(&mut self, session: SessionKey, turn: Uuid, idle: Idle) -> bool {
        if self.live.get(&session) != Some(&Live::Running(turn)) {
            return false;
        }
        self.live.insert(session, Live::Idle(idle));
        true
    }
    /// Caller has confirmed the process is gone. A generation-fenced caller
    /// must reject stale acknowledgements before reaching this method.
    pub fn ended(&mut self, session: SessionKey) {
        self.live.remove(&session);
    }

    pub fn next(&mut self, now: u64) -> Option<Action> {
        if let Some(turn) = self
            .waiting
            .values()
            .filter(|w| {
                w.routine_due_ms
                    .is_some_and(|due| now > due.saturating_add(ROUTINE_GRACE_MS))
            })
            .min_by_key(|w| (w.queued_at_ms, w.turn))
            .map(|w| w.turn)
        {
            self.waiting.remove(&turn);
            return Some(Action::Missed { turn });
        }
        // One occupied running turn per Bot before a second, within the
        // person-before-Routine priority. Idle sessions are not active turns.
        let active: BTreeSet<_> = self
            .live
            .iter()
            .filter_map(|(key, state)| matches!(state, Live::Running(_)).then_some(key.bot))
            .collect();
        let candidate = self
            .waiting
            .values()
            .filter(|w| match self.live.get(&w.session) {
                Some(Live::Running(_) | Live::Stopping) => false,
                Some(Live::Idle(idle)) => {
                    !idle.offered_or_unknown_delivery && !idle.leased_or_unresolved_operation
                }
                None => true,
            })
            .min_by_key(|w| {
                (
                    w.routine_due_ms.is_some(),
                    active.contains(&w.session.bot),
                    w.queued_at_ms,
                    w.turn,
                )
            })
            .cloned();
        if let Some(request) = candidate {
            let reuse = matches!(self.live.get(&request.session), Some(Live::Idle(_)));
            if reuse || self.live.len() < self.limit as usize {
                self.waiting.remove(&request.turn);
                self.live
                    .insert(request.session, Live::Running(request.turn));
                return Some(Action::Start { request, reuse });
            }
            if let Some(session) = self
                .live
                .iter()
                .filter_map(|(key, state)| match state {
                    Live::Idle(idle) if idle.may_yield() => Some((*key, idle.since_ms)),
                    _ => None,
                })
                .min_by_key(|(key, since)| (*since, *key))
                .map(|(key, _)| key)
            {
                self.live.insert(session, Live::Stopping);
                return Some(Action::End { session });
            }
        }
        if let Some(session) = self.live.iter().find_map(|(key, state)| match state {
            Live::Idle(idle) if idle.expires(now) => Some(*key),
            _ => None,
        }) {
            self.live.insert(session, Live::Stopping);
            return Some(Action::End { session });
        }
        None
    }
}

pub fn default_capacity(installed_bytes: u64) -> u32 {
    (installed_bytes / (2 * 1024 * 1024 * 1024)).clamp(1, u32::MAX as u64) as u32
}

/// Called on a lifecycle thread at admission, never from dispatch or a timer.
/// Uses the same typed read-only authority adapter as delegated run roots.
pub fn configured(root: &std::path::Path) -> u32 {
    koru_settings::SettingsPaths::for_channel_root(
        root,
        koru_channel::channel_root().as_deref(),
        koru_channel::segment(),
    )
    .map(koru_settings::SettingsAuthority::new)
    .and_then(|authority| authority.global().bots_session_capacity)
    .filter(|limit| *limit > 0)
    .unwrap_or_else(|| default_capacity(installed_memory()))
}

#[cfg(windows)]
fn installed_memory() -> u64 {
    use windows_sys::Win32::System::SystemInformation::GetPhysicallyInstalledSystemMemory;
    let mut kilobytes = 0;
    // SAFETY: Windows writes one u64 through the valid out-pointer.
    if unsafe { GetPhysicallyInstalledSystemMemory(&mut kilobytes) } != 0 {
        kilobytes.saturating_mul(1024)
    } else {
        0
    }
}
#[cfg(not(windows))]
fn installed_memory() -> u64 {
    0
}

#[cfg(test)]
mod tests;
