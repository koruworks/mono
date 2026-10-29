// SPDX-License-Identifier: MPL-2.0

//! **One session connection for every terminal on screen.**
//!
//! A session client holds one controller lease, and attaching a terminal takes
//! it and starts that terminal's screen stream. So the window's single
//! connection could drive one terminal at a time, and a second terminal beside
//! it — another pane, a split, another window's worth of agents — had no
//! stream and drew nothing until it was clicked. A terminal that is visible
//! should be live.
//!
//! The window's **main connection** (`session_backend`) keeps its jobs: the
//! catalog, the project tree, provider readiness, and one terminal. Every other
//! *visible* terminal gets a **lane**: a lighter connection of its own
//! ([`koru_session_backend::spawn_lane`]) that attaches that terminal's
//! session and delivers its screen. A lane lives exactly as long as its
//! terminal is on screen; hiding the tab, closing it, or the session going
//! away ends the lane and the process keeps running.
//!
//! The attach and event code was written for one connection, and it is
//! subtle. Rather than fork it, a lane runs **the same code**: the five fields
//! that say *which connection this is* — its backend, which tab it is attached
//! to or attaching, what the last settle drove, and the terminate it is
//! waiting for — live in a [`SessionLane`], and [`MainView::with_lane`] swaps
//! them in for the duration of one call. Everything that reads or writes them
//! then meets the lane's connection, and the main connection's state is put
//! back untouched.

use koru_session_backend::{BackendEvent, InteractionSender};
use koru_session_protocol::SessionId;

use super::MainView;
use super::session_backend::SessionBackend;
use super::tab::TabView;
use crate::session_tab::SessionSender;

/// A terminal's own connection, and the per-connection state the attach code
/// keeps. `Default` is the empty placeholder left in the list while the lane
/// is swapped in.
#[derive(Default)]
pub(super) struct SessionLane {
    pub(super) backend: Option<SessionBackend>,
    pub(super) attached: Option<u64>,
    pub(super) attaching: Option<u64>,
    pub(super) settled: Option<u64>,
    pub(super) terminate_after_attach: Option<SessionId>,
}

impl MainView {
    fn swap_lane(&mut self, lane: &mut SessionLane) {
        std::mem::swap(&mut self.session_backend, &mut lane.backend);
        std::mem::swap(&mut self.session_attached, &mut lane.attached);
        std::mem::swap(&mut self.session_attaching, &mut lane.attaching);
        std::mem::swap(&mut self.session_settled, &mut lane.settled);
        std::mem::swap(
            &mut self.terminate_after_attach,
            &mut lane.terminate_after_attach,
        );
    }

    /// Run `f` as though lane `index` were the window's one connection.
    pub(super) fn with_lane<R>(&mut self, index: usize, f: impl FnOnce(&mut Self) -> R) -> R {
        let mut lane = std::mem::take(&mut self.session_lanes[index]);
        self.swap_lane(&mut lane);
        let result = f(self);
        self.swap_lane(&mut lane);
        self.session_lanes[index] = lane;
        result
    }

    /// Whether some connection of this window holds this tab's session.
    pub(super) fn tab_attached_anywhere(&self, tab: u64) -> bool {
        self.session_attached == Some(tab)
            || self
                .session_lanes
                .iter()
                .any(|lane| lane.attached == Some(tab))
    }

    /// The sender of the connection that holds `session`, when one does — the
    /// only one a *Terminate* may go through, since it acts on whatever that
    /// connection is attached to.
    pub(super) fn controller_sender_for(&self, session: SessionId) -> Option<InteractionSender> {
        let tab = self.session_tab_for(session)?;
        if self.session_attached == Some(tab) {
            return self
                .session_backend
                .as_ref()
                .map(|backend| backend.sender.clone());
        }
        self.session_lanes
            .iter()
            .find(|lane| lane.attached == Some(tab))
            .and_then(|lane| lane.backend.as_ref())
            .map(|backend| backend.sender.clone())
    }

    /// The tabs a lane owns, which the main connection must leave alone.
    pub(super) fn lane_owned_tabs(&self) -> Vec<u64> {
        self.session_lanes
            .iter()
            .filter_map(|lane| lane.settled)
            .collect()
    }

    /// The terminals that are on screen and ready to be connected: the active
    /// tab of every pane, once it has been measured (a grid asked for before
    /// that is a resize the person watches happen), with something to attach
    /// or launch, and not showing a recovery offer or watching a run — those
    /// have their own path.
    fn connectable_terminals(&self) -> Vec<u64> {
        self.panes
            .iter_groups()
            .filter_map(|(_, group)| {
                let tab = group.tabs().active_tab();
                let TabView::Session(view) = &tab.view else {
                    return None;
                };
                let measured = view.surface.bounds.w > 0.0 && view.surface.bounds.h > 0.0;
                let has_work = view.session.is_some() || view.pending_launch.is_some();
                (measured
                    && has_work
                    && view.offer.is_none()
                    && !view.surface.watching().watching())
                .then_some(tab.id)
            })
            .collect()
    }

    /// Give every connectable terminal the main connection is not driving a
    /// lane of its own, and end the lanes whose terminal has left the screen.
    pub(super) fn settle_session_lanes(&mut self) -> bool {
        // Lanes exist beside a main connection, and only where one can be
        // started: a test container has neither.
        if self.session_backend.is_none() {
            return false;
        }
        let visible = self.connectable_terminals();
        let main = self.session_settled;
        let mut changed = false;

        let mut index = 0;
        while index < self.session_lanes.len() {
            let keep = self.session_lanes[index]
                .settled
                .is_some_and(|tab| visible.contains(&tab) && Some(tab) != main);
            if keep {
                index += 1;
                continue;
            }
            let lane = self.session_lanes.remove(index);
            changed |= self.end_lane(lane);
        }

        for tab in visible {
            if Some(tab) == main || self.lane_owned_tabs().contains(&tab) {
                continue;
            }
            let Some(backend) = self.new_backend_connection(true) else {
                break;
            };
            let sender = backend.sender.clone();
            self.session_lanes.push(SessionLane {
                backend: Some(backend),
                ..SessionLane::default()
            });
            let index = self.session_lanes.len() - 1;
            // The terminal sends its keys, resizes and pastes down its own
            // connection from here on.
            if let Some(view) = self.session_view_by_tab(tab) {
                view.surface.set_sender(&SessionSender(sender));
            }
            changed |= self.with_lane(index, |this| {
                this.session_settled = Some(tab);
                this.take_session_controller(tab)
            });
        }
        changed
    }

    /// Close a lane's connection and hand its terminal's input back to the
    /// main one. The session keeps running.
    fn end_lane(&mut self, lane: SessionLane) -> bool {
        if let Some(backend) = lane.backend.as_ref() {
            backend.sender.close();
        }
        let Some(tab) = lane.settled else {
            return false;
        };
        let main = self
            .session_backend
            .as_ref()
            .map(|backend| backend.sender.clone());
        if let (Some(main), Some(view)) = (main, self.session_view_by_tab(tab)) {
            view.surface.set_sender(&SessionSender(main));
        }
        true
    }

    /// Deliver what each lane's connection queued, through the same handler
    /// the main connection's events take — minus what only the main connection
    /// speaks for (the catalog, projects, provider readiness), which the lane
    /// asked for no part of.
    pub(super) fn drain_session_lanes(&mut self) -> bool {
        let mut changed = false;
        let mut index = 0;
        while index < self.session_lanes.len() {
            let events = self.session_lanes[index]
                .backend
                .as_ref()
                .map(SessionBackend::drain)
                .unwrap_or_default();
            let mut lost = false;
            for event in events {
                match event {
                    // A lane whose connection dropped is started again by the
                    // next settle, and attaches afresh.
                    BackendEvent::Disconnected
                    | BackendEvent::SessionsLost { .. }
                    | BackendEvent::Recovered { .. } => lost = true,
                    BackendEvent::Profiles(_)
                    | BackendEvent::ProviderModels { .. }
                    | BackendEvent::Providers(_)
                    | BackendEvent::Projects(_)
                    | BackendEvent::Connected(_)
                    | BackendEvent::Sessions(_)
                    | BackendEvent::Backpressure => {}
                    event => {
                        changed |= self.with_lane(index, |this| this.handle_session_event(event));
                    }
                }
            }
            if lost {
                let lane = self.session_lanes.remove(index);
                changed |= self.end_lane(lane);
                continue;
            }
            index += 1;
        }
        changed
    }
}

#[cfg(test)]
mod tests;
