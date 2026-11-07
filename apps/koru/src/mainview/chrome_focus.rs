// SPDX-License-Identifier: MPL-2.0

//! `F6`'s stops over the sidebar's chrome: the lane and the rail.
//!
//! The rotation was two stops — a failed reminder pill and the bell — and
//! [ADR-348](../../../../docs/decisions/ADR-326-350.md) made it a rotation
//! over what is *standing* rather than a chord per surface: "a stop that is
//! not there is never a dead press". The stops here follow that rule:
//! the lane is always there (its toggle is, in the pane or in the caption's
//! chain), and the rail is reachable only while the pane is open.
//!
//! **One key contract**, written in
//! `docs/features/koruui/CONTRIBUTING.md`: ← and → move between the stop's
//! controls, Enter or Space activates the focused one, Escape returns the
//! keyboard to the content, `F6` moves on. The focused control wears the
//! `focus_ring`; a press anywhere moves the keyboard and clears it.
//!
//! **On a rail view, the move chords move it** — `Ctrl+Shift+↑`/`↓`, the
//! registry's `MoveRowUp`/`MoveRowDown`, the same entries the Agent providers
//! rows move with (D41) — and the keyboard goes with it. No row of its own in
//! the registry: those two are what *move this* means wherever a list can be
//! reordered, and a third row on the chord could not say which of the chat's
//! and the editor's it overlaps. Asked here, it is answered only while `F6`
//! has the keyboard on the rail; the document's line move and the chat's
//! *Previous / Next chat* keep the chord everywhere else.

use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::{MainView, WorkspaceDoc};
use crate::panes::explore::ExploreChromeFocus;

/// Where the keyboard is, when it is on the sidebar's chrome.
/// The rail's controls after the apps: Help, Settings, the profile square.
/// (*Manage apps* is out until the manager lands, E68.)

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(super) enum ChromeStop {
    Lane(LaneControl),
    /// An index down the rail: workspaces, shown apps, then Help, Settings
    /// and the profile square. `MainView::rail_focus` is the one reading of
    /// it, for the ring and for Enter alike.
    Apps(usize),
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(super) enum LaneControl {
    Toggle,
    Search,
    /// The active app's primary create; a stop only while the app has one.
    New,
}

/// The rotation's order, as the eye reads the chrome: the reminder pill and
/// the bell (when reminders are shown), then the lane and the visible rail.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Stop {
    Pill,
    Bell,
    Lane,
    Apps,
}

impl MainView {
    fn standing_stops(&self) -> Vec<Stop> {
        let mut out = Vec::with_capacity(5);
        if koru_features::reminders() {
            if self.reminder_creation.has_failed_pill() {
                out.push(Stop::Pill);
            }
            out.push(Stop::Bell);
        }
        out.push(Stop::Lane);
        if self.sidebar.is_open() {
            out.push(Stop::Apps);
        }
        out
    }

    fn current_stop(&self) -> Option<Stop> {
        if self.reminder_creation.pill_focused() {
            return Some(Stop::Pill);
        }
        if self.reminder_shell.bell_is_focused() {
            return Some(Stop::Bell);
        }
        self.chrome_focus.map(|stop| match stop {
            ChromeStop::Lane(_) => Stop::Lane,
            ChromeStop::Apps(_) => Stop::Apps,
        })
    }

    /// `F6`: move the keyboard to the next standing stop. Returns whether
    /// anything moved.
    pub(super) fn focus_chrome(&mut self) -> bool {
        let stops = self.standing_stops();
        let next = match self
            .current_stop()
            .and_then(|s| stops.iter().position(|x| *x == s))
        {
            Some(i) => stops[(i + 1) % stops.len()],
            None => stops[0],
        };
        let was = self.current_stop();
        // Leave whichever stop had the keyboard, so two rings are never
        // painted at once.
        let mut moved = self.reminder_shell.blur_bell();
        moved |= self.reminder_creation.blur_pill();
        moved |= self.chrome_focus.take().is_some();
        match next {
            Stop::Pill => moved |= self.reminder_creation.focus_first_failed_pill(),
            Stop::Bell => moved |= self.reminder_shell.focus_bell(),
            Stop::Lane => {
                self.chrome_focus = Some(ChromeStop::Lane(LaneControl::Toggle));
                moved = true;
            }
            Stop::Apps => {
                // Land on the active app, where the eye already is.
                let i = self
                    .explore
                    .rail_controls(self.rail_area())
                    .iter()
                    .position(|hit| {
                        *hit == crate::panes::sidebar::apps_rail::Hit::App(
                            self.explore.navigator_mode(),
                        )
                    })
                    .unwrap_or(0);
                self.chrome_focus = Some(ChromeStop::Apps(i));
                moved = true;
            }
        }
        super::chrome_debug::chrome_debug!(
            "focus rotation {:?} -> {:?}; chrome={:?}",
            was,
            next,
            self.chrome_focus
        );
        moved || was != Some(next)
    }

    /// The keys a focused stop answers: the arrows within it, an activation,
    /// and Escape. Everything else falls through, the way the bell's do.
    pub(super) fn chrome_focus_key(
        &mut self,
        ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        let Some(stop) = self.chrome_focus else {
            return false;
        };
        // An open switcher owns the keyboard (its own Esc, arrows and Enter)
        // until it shuts; the stop takes over again after.
        if self.explore.popover_open() {
            return false;
        }
        if let ChromeStop::Apps(i) = stop
            && let ExploreChromeFocus::Workspace(Some(workspace)) = self.rail_focus(i)
            && let Some(up) = move_chord(key, text, mods)
        {
            if let Some(outcome) = self.explore.nudge_workspace(workspace, up) {
                self.apply(ws, outcome);
                if let Some(index) = self
                    .explore
                    .rail_controls(self.rail_area())
                    .iter()
                    .position(|hit| {
                        *hit == crate::panes::sidebar::apps_rail::Hit::Workspace(workspace)
                    })
                {
                    self.chrome_focus = Some(ChromeStop::Apps(index));
                }
            }
            return true;
        }
        if let ChromeStop::Apps(i) = stop
            && let ExploreChromeFocus::App(app) = self.rail_focus(i)
            && let Some(up) = move_chord(key, text, mods)
        {
            // Taken at either end too: the view is the keyboard's, and a
            // chord that cannot move it still belongs to it (D41's rule).
            if let Some(to) = self.explore.nudge_rail_view(self.rail_area(), app, up) {
                let controls = self.explore.rail_controls(self.rail_area());
                if let Some(index) = controls.iter().position(|hit| matches!(hit, crate::panes::sidebar::apps_rail::Hit::App(mode) if Some(*mode) == self.explore.rail_mode_at(to))) {
                    self.chrome_focus = Some(ChromeStop::Apps(index));
                }
            }
            return true;
        }
        match key {
            Key::Named(NamedKey::Escape) => {
                self.chrome_focus = None;
                true
            }
            // The rail runs top to bottom, so ↑↓ walk it as ←→ do; the lane
            // takes both too rather than refusing a pair.
            Key::Named(NamedKey::ArrowLeft)
            | Key::Named(NamedKey::ArrowRight)
            | Key::Named(NamedKey::ArrowUp)
            | Key::Named(NamedKey::ArrowDown) => {
                let forward = matches!(
                    key,
                    Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::ArrowDown)
                );
                self.chrome_focus = Some(self.step_within(stop, forward));
                true
            }
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => {
                self.activate_stop(ws, stop);
                true
            }
            _ => false,
        }
    }

    /// The next control of a stop, in the order it is drawn, wrapping.
    fn step_within(&self, stop: ChromeStop, forward: bool) -> ChromeStop {
        match stop {
            ChromeStop::Lane(control) => {
                // Left to right as drawn: in the lane Search · New · toggle,
                // in the chain the toggle leads. Sorted by where each sits
                // rather than by a second copy of either order.
                let lane = self.lane_controls();
                let mut shown: Vec<(f32, LaneControl)> = [
                    (lane.toggle, LaneControl::Toggle),
                    (lane.search, LaneControl::Search),
                    (lane.new, LaneControl::New),
                ]
                .into_iter()
                .filter(|(r, _)| r.w > 0.0)
                .map(|(r, c)| (r.x, c))
                .collect();
                shown.sort_by(|a, b| a.0.total_cmp(&b.0));
                let shown: Vec<LaneControl> = shown.into_iter().map(|(_, c)| c).collect();
                let i = shown.iter().position(|c| *c == control).unwrap_or(0);
                let n = shown.len().max(1);
                let j = if forward {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                };
                ChromeStop::Lane(shown.get(j).copied().unwrap_or(LaneControl::Toggle))
            }
            ChromeStop::Apps(i) => {
                // The apps, then the foot: Help, Settings, the profile
                // square.
                let n = self.explore.rail_controls(self.rail_area()).len().max(1);
                ChromeStop::Apps(if forward {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                })
            }
        }
    }

    /// Enter or Space on a stop: the same thing a press on it does.
    fn activate_stop(&mut self, ws: &mut WorkspaceDoc, stop: ChromeStop) {
        match stop {
            ChromeStop::Lane(LaneControl::Toggle) => {
                self.toggle_sidebar();
                // The lane keeps the keyboard; the apps row and the floor
                // stops may have gone with the pane.
            }
            ChromeStop::Lane(LaneControl::Search) => {
                self.open_search();
            }
            ChromeStop::Lane(LaneControl::New) => {
                self.open_launcher();
            }
            // The control the ring is on, read the way the ring reads it:
            // Help, Settings and the profile square each do what a press on
            // them does (`ExploreView::apps_rail_mouse_down`). This read its
            // own copy of the foot once, with *Manage apps* still first in
            // it, so Enter on Help did nothing and on the profile square
            // opened Help (found in D96).
            ChromeStop::Apps(i) => match self.rail_focus(i) {
                ExploreChromeFocus::Workspace(Some(id)) if Some(id) != self.current_workspace() => {
                    self.open_workspace(id);
                }
                ExploreChromeFocus::Workspace(_) => {
                    self.open_workspaces_popover();
                }
                ExploreChromeFocus::App(i) => {
                    if let Some(mode) = self.explore.rail_mode_at(i) {
                        self.show_navigator(mode);
                    }
                }
                ExploreChromeFocus::RailHelp => {
                    self.open_help_beside();
                }
                ExploreChromeFocus::RailOverflow => {
                    self.open_rail_overflow();
                }
                ExploreChromeFocus::RailSettings => {
                    self.explore.close_popover();
                    self.open_settings_tab();
                }
                ExploreChromeFocus::RailProfile => self.explore.toggle_profile_popover(),
            },
        }
        let _ = ws;
    }

    /// The part of the focus the explore column draws: an apps-row chip or
    /// a floor control. The lane's ring is the window's.
    pub(super) fn explore_chrome_focus(&self) -> Option<ExploreChromeFocus> {
        match self.chrome_focus? {
            ChromeStop::Apps(i) => Some(self.rail_focus(i)),
            ChromeStop::Lane(_) => None,
        }
    }

    /// Which control index `i` down the rail is: the shown views in order,
    /// then the foot top to bottom — Help, Settings, the profile square, as
    /// `apps_rail::solve` stacks them. **The one reading**, for the ring the
    /// explore column draws and for what Enter does.
    pub(super) fn rail_focus(&self, i: usize) -> ExploreChromeFocus {
        self.explore.rail_focus(self.rail_area(), i)
    }

    /// The lane control the keyboard is on, for its ring.
    pub(super) fn lane_chrome_focus(&self) -> Option<LaneControl> {
        match self.chrome_focus? {
            ChromeStop::Lane(control) => Some(control),
            _ => None,
        }
    }
}

/// `Some(true)` for the move-up chord, `Some(false)` for move-down — the
/// registry's, so the chord is the one the shortcut sheet prints.
fn move_chord(key: &Key, text: Option<&str>, mods: ModifiersState) -> Option<bool> {
    let is = |id| koru_keys::registry::get(id).chord.matches(key, text, mods);
    if is(koru_keys::ShortcutId::MoveRowUp) {
        Some(true)
    } else if is(koru_keys::ShortcutId::MoveRowDown) {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
