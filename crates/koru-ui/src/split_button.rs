// SPDX-License-Identifier: MPL-2.0

//! A compact primary action with an independently enabled menu target.
//! Geometry, hit testing and painting share one split. The caller owns the
//! actions, focus order and the menu; this component knows only their states.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, inspect, scene, theme};
use vello::Scene;
use vello::kurbo::Affine;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitTone {
    Accent,
    Success,
    Neutral,
    Warning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitPart {
    Action,
    Menu,
}

/// The shared split-button geometry. `rect` includes both targets; the menu
/// keeps its own hit area even when the action is disabled.
#[derive(Clone, Copy, Debug)]
pub struct SplitButton {
    pub rect: Rect,
    pub tone: SplitTone,
    pub action_enabled: bool,
    pub action_hot: bool,
    pub action_focused: bool,
    pub menu_hot: bool,
    pub menu_focused: bool,
    pub menu_open: bool,
}

impl SplitButton {
    pub const ACTION_W: f32 = crate::button::ActionButton::H;
    pub const MENU_W: f32 = 36.0;
    /// The mode badge's resting lane; the chevron occupies the reserved right end on hover.
    pub const RESTING_MENU_W: f32 = 25.0;
    pub const H: f32 = crate::button::ActionButton::H;
    pub const W: f32 = Self::ACTION_W + Self::MENU_W;

    pub fn action_rect(self) -> Rect {
        Rect::new(self.rect.x, self.rect.y, Self::ACTION_W, self.rect.h)
    }

    pub fn menu_rect(self) -> Rect {
        Rect::new(
            self.rect.x + Self::ACTION_W,
            self.rect.y,
            self.rect.w - Self::ACTION_W,
            self.rect.h,
        )
    }

    fn show_chevron(self) -> bool {
        self.menu_hot || self.menu_focused || self.menu_open
    }

    /// The painted pill grows to the right without moving either icon.
    pub fn painted_rect(self) -> Rect {
        Rect::new(
            self.rect.x,
            self.rect.y,
            if self.show_chevron() {
                self.rect.w
            } else {
                Self::ACTION_W + Self::RESTING_MENU_W
            },
            self.rect.h,
        )
    }

    pub fn hit(self, x: f32, y: f32) -> Option<SplitPart> {
        if self.action_rect().contains(x, y) {
            Some(SplitPart::Action)
        } else if self.menu_rect().contains(x, y) {
            Some(SplitPart::Menu)
        } else {
            None
        }
    }

    /// Draw the action glyph, mode icon and a chevron on hover or focus.
    /// The menu icon remains visible at rest as the mode badge.
    #[track_caller]
    pub fn draw(
        self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        glyph: char,
        badge: char,
    ) {
        let _scope = inspect::scope(
            "KoruSplitButton",
            "component",
            root,
            scene::rect(self.rect.x, self.rect.y, self.rect.w, self.rect.h),
            |record| {
                record.property("tone", format!("{:?}", self.tone), "SplitTone");
                record.property("action enabled", self.action_enabled, "caller state");
                record.property(
                    "menu focused / open",
                    format!("{} / {}", self.menu_focused, self.menu_open),
                    "caller state",
                );
            },
        );
        let p = theme::p();
        let fill = match self.tone {
            SplitTone::Accent => p.accent,
            SplitTone::Success => p.success,
            SplitTone::Neutral => p.row_selected,
            SplitTone::Warning => p.notice_fg,
        };
        let painted = self.painted_rect();
        let shape = scene::rrect(painted.x, painted.y, painted.w, painted.h, painted.h * 0.5);
        scene::fill(canvas, root, fill, &shape);
        let action = self.action_rect();
        let menu = self.menu_rect();
        if self.action_hot && self.action_enabled {
            scene::fill(
                canvas,
                root,
                p.hover_tint,
                &scene::rect(action.x, action.y, action.w, action.h),
            );
        }
        if self.action_focused {
            scene::stroke(
                canvas,
                root,
                p.focus_ring,
                theme::BORDER_W * 2.0,
                &scene::rrect(action.x, action.y, action.w, action.h, action.h * 0.5),
            );
        }
        if self.menu_hot || self.menu_open {
            scene::fill(
                canvas,
                root,
                p.hover_tint,
                &scene::rect(menu.x, menu.y, menu.w, menu.h),
            );
        }
        if self.menu_focused {
            scene::stroke(
                canvas,
                root,
                p.focus_ring,
                theme::BORDER_W * 2.0,
                &scene::rrect(menu.x, menu.y, menu.w, menu.h, menu.h * 0.5),
            );
        }
        scene::fill(
            canvas,
            root,
            p.panel_border,
            &scene::rect(menu.x, menu.y + 6.0, theme::BORDER_W as f32, menu.h - 12.0),
        );
        let ink = if self.tone == SplitTone::Neutral {
            p.text
        } else {
            p.on_accent
        };
        // An unavailable send is already inert and has no action hover. Keep
        // its glyph legible against the filled tone even with an empty draft.
        let action_ink = ink;
        let action_icon = engine.icon_layout(glyph, 15.0);
        scene::draw_layout_first_line(
            canvas,
            root,
            &action_icon,
            action.x + (action.w - action_icon.width()) * 0.5,
            action.y + (action.h - action_icon.height()) * 0.5,
            Some(action_ink),
        );
        let badge_icon = engine.icon_layout(badge, 12.0);
        let show_chevron = self.show_chevron();
        let badge_x = menu.x + 5.0;
        scene::draw_layout_first_line(
            canvas,
            root,
            &badge_icon,
            badge_x,
            menu.y + (menu.h - badge_icon.height()) * 0.5,
            Some(ink),
        );
        if show_chevron {
            let caret = engine.icon_layout(icons::CARET_DOWN, 11.0);
            scene::draw_layout_first_line(
                canvas,
                root,
                &caret,
                menu.x + menu.w - caret.width() - 8.0,
                menu.y + (menu.h - caret.height()) * 0.5,
                Some(ink),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_action_keeps_an_independent_menu_target() {
        let button = SplitButton {
            rect: Rect::new(10.0, 20.0, SplitButton::W, SplitButton::H),
            tone: SplitTone::Success,
            action_enabled: false,
            action_hot: false,
            action_focused: false,
            menu_hot: false,
            menu_focused: false,
            menu_open: false,
        };
        assert_eq!(button.hit(20.0, 30.0), Some(SplitPart::Action));
        assert_eq!(button.hit(50.0, 30.0), Some(SplitPart::Menu));
        assert_eq!(
            button.action_rect().x + button.action_rect().w,
            button.menu_rect().x
        );
        assert_eq!(
            button.menu_rect().x + button.menu_rect().w,
            button.rect.x + button.rect.w
        );
    }

    #[test]
    fn hover_extends_the_pill_right_without_moving_its_targets() {
        let idle = SplitButton {
            rect: Rect::new(10.0, 20.0, SplitButton::W, SplitButton::H),
            tone: SplitTone::Warning,
            action_enabled: false,
            action_hot: false,
            action_focused: false,
            menu_hot: false,
            menu_focused: false,
            menu_open: false,
        };
        let hot = SplitButton {
            menu_hot: true,
            ..idle
        };
        assert_eq!(idle.action_rect(), hot.action_rect());
        assert_eq!(idle.menu_rect(), hot.menu_rect());
        assert_eq!(idle.painted_rect().x, hot.painted_rect().x);
        assert!(idle.painted_rect().w < hot.painted_rect().w);
        assert_eq!(hot.painted_rect().w, SplitButton::W);
    }
}
