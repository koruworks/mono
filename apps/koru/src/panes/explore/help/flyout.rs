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
        self.help_aim = None;
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

    /// The *Help* row extended across the parent's side and bottom padding so
    /// moving out of the row's highlight plate toward the flyout never drops
    /// into a dead margin before reaching the gap.
    fn help_trigger_band(&self, area: Rect, viewport: (f32, f32)) -> Option<Rect> {
        let parent = self.help_parent_panel(area, viewport)?;
        let trigger = self.help_parent_trigger(area, viewport)?;
        let bottom = (parent.y + parent.h).max(trigger.y + trigger.h);
        Some(Rect::new(
            parent.x,
            trigger.y,
            parent.w,
            (bottom - trigger.y).max(0.0),
        ))
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

    /// The horizontal gap between parent and submenu over their full vertical
    /// span, so crossing diagonally or brushing the gap beside an upper row
    /// keeps the submenu reachable.
    fn help_bridge(&self, area: Rect, viewport: (f32, f32)) -> Option<Rect> {
        let parent = self.help_parent_panel(area, viewport)?;
        let child = self.help_flyout_panel(area, viewport)?;
        let x = (parent.x + parent.w).min(child.x + child.w);
        let right = parent.x.max(child.x);
        let top = parent.y.min(child.y);
        let bottom = (parent.y + parent.h).max(child.y + child.h);
        Some(Rect::new(
            x,
            top,
            (right - x).max(0.0),
            (bottom - top).max(0.0),
        ))
    }

    /// Whether `(x, y)` inside `parent` lies in the safe triangle from the last
    /// aim point toward `child`'s near vertical edge. Moving toward `child`
    /// keeps the submenu open across sibling rows; moving straight up or away
    /// from `child` leaves the corridor immediately.
    fn help_in_aim_corridor(&self, parent: Rect, child: Rect, x: f32, y: f32) -> bool {
        let Some((ax, ay)) = self.help_aim else {
            return false;
        };
        if (x - ax).abs() <= 0.5 && (y - ay).abs() <= 0.5 {
            return true;
        }
        let child_on_right = child.x + child.w * 0.5 >= parent.x + parent.w * 0.5;
        let (span, progress) = if child_on_right {
            (child.x - ax, x - ax)
        } else {
            (ax - (child.x + child.w), ax - x)
        };
        if span <= 0.0 || progress <= 0.0 {
            return false;
        }
        let t = (progress / span).min(1.0);
        let top = ay + (child.y - ay) * t;
        let bottom = ay + (child.y + child.h - ay) * t;
        y >= top - 1.0 && y <= bottom + 1.0
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
        if self.help_parent.is_none() {
            return false;
        }
        // In a narrow window the child can overlap its parent; the topmost
        // panel owns the pointer there.
        if self
            .popover_panel(area, viewport)
            .is_some_and(|r| r.contains(x, y))
        {
            self.help_aim = None;
            return false;
        }
        if self
            .help_bridge(area, viewport)
            .is_some_and(|r| r.contains(x, y))
        {
            return false;
        }
        if self
            .help_trigger_band(area, viewport)
            .is_some_and(|r| r.contains(x, y))
        {
            self.help_aim = Some((x, y));
            return false;
        }
        if let (Some(parent), Some(child)) = (
            self.help_parent_panel(area, viewport),
            self.help_flyout_panel(area, viewport),
        ) && parent.contains(x, y)
            && self.help_in_aim_corridor(parent, child, x, y)
        {
            self.help_aim = Some((x, y));
            return false;
        }
        self.restore_help_parent()
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
            self.help_trigger_band(area, viewport),
            self.help_bridge(area, viewport),
        ]
        .into_iter()
        .flatten()
        .any(|r| r.contains(x, y))
    }
}
