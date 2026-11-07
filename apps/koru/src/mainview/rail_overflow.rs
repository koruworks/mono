// SPDX-License-Identifier: MPL-2.0

use super::*;

#[cfg(test)]
mod tests;

impl MainView {
    pub(super) fn open_rail_overflow(&mut self) -> bool {
        let rail = self.rail_area();
        let projection = self.explore.view_projection(rail.h);
        if projection.folded.is_empty() {
            return false;
        }
        self.explore.close_popover();
        let modes = self.explore.rail_modes();
        let items = projection
            .folded
            .into_iter()
            .filter_map(|mode| {
                modes
                    .iter()
                    .position(|m| *m == mode)
                    .map(|slot| MenuAction::RailView { mode, slot })
            })
            .collect();
        let anchor = self.explore.apps_rail_rects(rail).overflow;
        let mut menu = ContextMenu::new(
            &mut self.engine,
            MenuSubject::RailOverflow,
            (anchor.x + anchor.w, anchor.y),
            items,
        );
        let enabled = self.enabled_items(&menu);
        menu.step(&enabled, 1);
        if let Some(index) = menu.items().iter().position(|item| {
            matches!(item, MenuAction::RailView { mode, .. } if Some(*mode) == self.folded_rail_focus)
        }) { menu.hover = Some(index); }
        self.menu = Some(menu);
        true
    }

    pub(super) fn focused_rail_identity(&self) -> Option<crate::panes::sidebar::apps_rail::Hit> {
        let chrome_focus::ChromeStop::Apps(index) = self.chrome_focus? else {
            return None;
        };
        self.explore
            .rail_controls(self.rail_area())
            .get(index)
            .copied()
    }

    pub(super) fn retain_rail_focus(
        &mut self,
        previous: Option<crate::panes::sidebar::apps_rail::Hit>,
    ) {
        use crate::panes::sidebar::apps_rail::Hit;
        let controls = self.explore.rail_controls(self.rail_area());
        let overflow = controls.iter().position(|hit| *hit == Hit::Overflow);
        let menu_focus = self
            .menu
            .as_ref()
            .filter(|menu| menu.subject == MenuSubject::RailOverflow)
            .and_then(|menu| menu.hover.and_then(|i| menu.items().get(i)))
            .and_then(|item| match item {
                MenuAction::RailView { mode, .. } => Some(*mode),
                _ => None,
            });
        if menu_focus.is_some() {
            self.folded_rail_focus = menu_focus;
        }
        let mut desired = previous;
        if let Some(Hit::App(mode)) = previous {
            if !controls.contains(&Hit::App(mode)) {
                self.folded_rail_focus = Some(mode);
                desired = Some(Hit::Overflow);
            } else {
                self.folded_rail_focus = None;
            }
        } else if previous == Some(Hit::Overflow) && overflow.is_none() {
            desired = self
                .folded_rail_focus
                .take()
                .map(Hit::App)
                .filter(|hit| controls.contains(hit))
                .or_else(|| {
                    controls
                        .iter()
                        .rev()
                        .find(|hit| matches!(hit, Hit::App(_)))
                        .copied()
                });
        }
        let close = self.menu.as_ref().is_some_and(|menu| {
            menu.subject == MenuSubject::RailOverflow &&
                (overflow.is_none() || menu.hover.and_then(|i| menu.items().get(i)).is_some_and(|item| {
                    matches!(item, MenuAction::RailView { mode, .. } if controls.contains(&Hit::App(*mode)))
                }))
        });
        if close {
            self.menu = None;
            if overflow.is_some() {
                desired = Some(Hit::Overflow);
            } else if let Some(mode) = menu_focus.filter(|mode| controls.contains(&Hit::App(*mode)))
            {
                desired = Some(Hit::App(mode));
                self.folded_rail_focus = None;
            }
        }
        if let Some(hit) = desired {
            self.chrome_focus = Some(chrome_focus::ChromeStop::Apps(
                controls
                    .iter()
                    .position(|candidate| *candidate == hit)
                    .unwrap_or(0),
            ));
        }
    }
}
