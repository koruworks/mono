// SPDX-License-Identifier: MPL-2.0

//! Help keeps its existing element page and input controller; only its
//! placement changes when opened from the Profile menu.

use super::super::popover_elements::profiles::MenuAction;
use super::super::*;

impl ExploreView {
    pub(crate) fn hovering_profile_help(&self) -> bool {
        self.live().is_some_and(|p| {
            p.kind == PopoverKind::Profiles
                && p.hover.and_then(|i| self.rows_for(p.kind).get(i).copied())
                    == Some(PopoverRow::ProfileAction(MenuAction::Help))
        })
    }

    pub(crate) fn open_help_flyout(&mut self) -> bool {
        if !self.popover_is(PopoverKind::Profiles) {
            return false;
        }
        let mut parent = self.popover.take().expect("open Profile menu");
        parent.hover = self
            .rows_for(PopoverKind::Profiles)
            .iter()
            .position(|r| *r == PopoverRow::ProfileAction(MenuAction::Help));
        parent.progress = 1.0;
        parent.fade = None;
        parent.slide = None;
        parent.page = 1.0;
        self.open_popover(PopoverKind::Help);
        if let Some(child) = self.popover.as_mut() {
            child.progress = 1.0;
            child.fade = None;
            child.slide = None;
        }
        self.help_parent = Some(parent);
        true
    }

    pub(crate) fn restore_help_parent(&mut self) -> bool {
        let Some(parent) = self.help_parent.take() else {
            return false;
        };
        self.popover = Some(parent);
        self.popover_part = None;
        true
    }

    pub(crate) fn help_parent_panel(&self, area: Rect, viewport: (f32, f32)) -> Option<Rect> {
        let parent = self.help_parent.as_ref()?;
        self.panel_for(area, viewport, parent.page, parent)
    }

    fn help_parent_trigger(&self, area: Rect, viewport: (f32, f32)) -> Option<Rect> {
        let panel = self.help_parent_panel(area, viewport)?;
        let parent = self.help_parent.as_ref()?;
        crate::input_engine::with(|engine| {
            let (_, frame) = self.page_at(
                engine,
                PopoverKind::Profiles,
                panel,
                parent.scroll,
                parent.hover,
                None,
            )?;
            Some(frame.node(MenuAction::Help.key())?.rect)
        })
    }

    pub(crate) fn help_flyout_panel(&self, area: Rect, viewport: (f32, f32)) -> Option<Rect> {
        let parent = self.help_parent_panel(area, viewport)?;
        let w = POPOVER_W.min((viewport.0 - POPOVER_EDGE * 2.0).max(0.0));
        let bottom = parent.y + parent.h;
        let h = self
            .page_natural_h(PopoverKind::Help)
            .min(SWITCHER_MAX_H)
            .min((bottom - POPOVER_EDGE).max(0.0));
        let right = parent.x + parent.w + POPOVER_GAP;
        let x = if right + w <= viewport.0 - POPOVER_EDGE {
            right
        } else {
            parent.x - POPOVER_GAP - w
        };
        Some(Rect::new(x.max(POPOVER_EDGE), bottom - h, w, h))
    }

    /// The small horizontal bridge keeps the submenu reachable from its row.
    fn help_bridge(&self, area: Rect, viewport: (f32, f32)) -> Option<Rect> {
        let parent = self.help_parent_panel(area, viewport)?;
        let child = self.help_flyout_panel(area, viewport)?;
        let trigger = self.help_parent_trigger(area, viewport)?;
        let x = (parent.x + parent.w).min(child.x + child.w);
        let right = parent.x.max(child.x);
        Some(Rect::new(x, trigger.y, (right - x).max(0.0), trigger.h))
    }

    pub(crate) fn popover_contains(
        &self,
        area: Rect,
        viewport: (f32, f32),
        x: f32,
        y: f32,
    ) -> bool {
        self.popover_open()
            && [
                self.popover_panel(area, viewport),
                self.help_parent_panel(area, viewport),
                self.help_bridge(area, viewport),
            ]
            .into_iter()
            .flatten()
            .any(|r| r.contains(x, y))
    }

    pub(crate) fn help_pointer_to_parent(
        &mut self,
        area: Rect,
        viewport: (f32, f32),
        x: f32,
        y: f32,
    ) -> bool {
        // In a narrow window the child can overlap its parent; the topmost
        // panel owns the pointer there.
        if self.help_parent.is_none()
            || self
                .popover_panel(area, viewport)
                .is_some_and(|r| r.contains(x, y))
        {
            return false;
        }
        if !self.popover_contains(area, viewport, x, y)
            || self
                .help_parent_panel(area, viewport)
                .is_some_and(|r| r.contains(x, y))
                && !self
                    .help_parent_trigger(area, viewport)
                    .is_some_and(|r| r.contains(x, y))
        {
            return self.restore_help_parent();
        }
        false
    }

    pub(crate) fn help_parent_or_bridge_contains(
        &self,
        area: Rect,
        viewport: (f32, f32),
        x: f32,
        y: f32,
    ) -> bool {
        if self.help_parent.is_none()
            || self
                .popover_panel(area, viewport)
                .is_some_and(|r| r.contains(x, y))
        {
            return false;
        }
        [
            self.help_parent_trigger(area, viewport),
            self.help_bridge(area, viewport),
        ]
        .into_iter()
        .flatten()
        .any(|r| r.contains(x, y))
    }
}
