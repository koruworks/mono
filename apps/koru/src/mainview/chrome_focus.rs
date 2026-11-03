// SPDX-License-Identifier: MPL-2.0

//! `F6`'s stops over the sidebar's chrome: the lane, the apps row and the
//! floor (`docs/features/workspace-foundation/TECHNICAL.final.md` §10).
//!
//! The rotation was two stops — a failed reminder pill and the bell — and
//! [ADR-348](../../../../docs/decisions/ADR-326-350.md) made it a rotation
//! over what is *standing* rather than a chord per surface: "a stop that is
//! not there is never a dead press". The three stops here follow that rule:
//! the lane is always there (its toggle is, in the pane or in the caption's
//! chain), the apps row and the floor only while the pane is open.
//!
//! **One key contract for the three**, written in
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
const RAIL_FOOT: usize = 3;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(super) enum ChromeStop {
    Lane(LaneControl),
    /// An index down the rail: the shown apps in order, then *Manage apps*
    /// at `visible.len()`, then Help, Settings and the profile square.
    Apps(usize),
    Floor(FloorControl),
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(super) enum LaneControl {
    Toggle,
    Search,
    /// The active app's primary create; a stop only while the app has one.
    New,
}

/// The floor's one control. The `?` that was its second moved into the
/// who/where popover's toolbar (QUESTIONS.md E30), where the popover's own
/// arrows reach it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(super) enum FloorControl {
    Chip,
}

/// The rotation's order, as the eye reads the chrome: the reminder pill and
/// the bell (when reminders are shown), then the lane, the apps row and the
/// floor (the last two when the pane is open).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Stop {
    Pill,
    Bell,
    Lane,
    Apps,
    Floor,
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
            out.push(Stop::Floor);
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
            ChromeStop::Floor(_) => Stop::Floor,
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
                    .rail_modes()
                    .iter()
                    .position(|m| *m == self.explore.navigator_mode())
                    .unwrap_or(0);
                self.chrome_focus = Some(ChromeStop::Apps(i));
                moved = true;
            }
            Stop::Floor => {
                self.chrome_focus = Some(ChromeStop::Floor(FloorControl::Chip));
                moved = true;
            }
        }
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
            && i < self.explore.rail_modes().len()
            && let Some(up) = move_chord(key, text, mods)
        {
            // Taken at either end too: the view is the keyboard's, and a
            // chord that cannot move it still belongs to it (D41's rule).
            if let Some(to) = self.explore.nudge_rail_view(self.rail_area(), i, up) {
                self.chrome_focus = Some(ChromeStop::Apps(to));
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
                let n = self.explore.rail_modes().len() + RAIL_FOOT;
                ChromeStop::Apps(if forward {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                })
            }
            ChromeStop::Floor(FloorControl::Chip) => ChromeStop::Floor(FloorControl::Chip),
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
                let square = self.settled_lane_controls().new;
                self.open_new_tab_menu_at(square);
            }
            ChromeStop::Apps(i) => {
                let apps = self.explore.rail_modes();
                if let Some(mode) = apps.get(i).copied() {
                    self.show_navigator(mode);
                } else {
                    match i - apps.len() {
                        // *Manage apps*, inert until the manager lands
                        // (PLAN.final P11).
                        0 => {}
                        1 => {
                            self.open_settings_tab();
                        }
                        2 => {
                            self.open_help_beside();
                        }
                        _ => self.explore.toggle_profile_popover(),
                    }
                }
            }
            ChromeStop::Floor(FloorControl::Chip) => {
                if self.explore.popover_open() {
                    self.explore.close_popover();
                } else {
                    self.open_workspaces_popover();
                }
            }
        }
        let _ = ws;
    }

    /// The part of the focus the explore column draws: an apps-row chip or
    /// a floor control. The lane's ring is the window's.
    pub(super) fn explore_chrome_focus(&self) -> Option<ExploreChromeFocus> {
        match self.chrome_focus? {
            ChromeStop::Apps(i) => {
                let apps = self.explore.rail_modes().len();
                Some(if i < apps {
                    ExploreChromeFocus::App(i)
                } else {
                    match i - apps {
                        0 => ExploreChromeFocus::RailHelp,
                        1 => ExploreChromeFocus::RailSettings,
                        _ => ExploreChromeFocus::RailProfile,
                    }
                })
            }
            ChromeStop::Floor(FloorControl::Chip) => Some(ExploreChromeFocus::FloorChip),
            ChromeStop::Lane(_) => None,
        }
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
