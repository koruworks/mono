// SPDX-License-Identifier: MPL-2.0

//! A dropdown: one field showing the current value, which opens a list of the
//! others.
//!
//! **Fewer things on the screen.** A setting with a dozen values — a font, a
//! language, a microphone — drawn as a wall of chips puts every value in front
//! of a person who is choosing one. The field shows only the choice that is in
//! force; the rest are one press away, in the same [`Menu`] every other list in
//! Koru opens, so keyboard stepping, hover and the floating panel are not a
//! second implementation.
//!
//! The field is subject-free like everything in this crate: it draws a label
//! and hands back geometry. What an option *means* is the caller's — it gets
//! back the index the person chose.

use std::borrow::Cow;

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, inspect, scene, theme};
use vello::Scene;
use vello::kurbo::Affine;

use crate::menu::{Anchor, Menu, MenuItem};

/// One value in a dropdown's list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropdownOption {
    pub label: String,
    /// Whether this is the value in force; drawn as the menu's check.
    pub selected: bool,
    /// The font stack the label is set in — a font picker's rows each show
    /// their own family. `None` is the interface face.
    pub font_stack: Option<&'static str>,
}

impl DropdownOption {
    pub fn new(label: impl Into<String>, selected: bool) -> Self {
        Self {
            label: label.into(),
            selected,
            font_stack: None,
        }
    }

    pub fn in_face(mut self, stack: &'static str) -> Self {
        self.font_stack = Some(stack);
        self
    }
}

impl MenuItem for DropdownOption {
    fn label(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.label)
    }
    fn font_stack(&self) -> Option<&'static str> {
        self.font_stack
    }
    fn checked(&self) -> Option<bool> {
        Some(self.selected)
    }
}

/// The closed field.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Dropdown<'a> {
    /// The value in force, as the field shows it.
    pub label: &'a str,
    /// The face the value is set in, for a font picker.
    pub font_stack: Option<&'static str>,
    pub open: bool,
    pub enabled: bool,
}

impl<'a> Dropdown<'a> {
    pub const H: f32 = 30.0;
    const PAD_X: f32 = 10.0;
    const CHEVRON_GAP: f32 = 8.0;
    const CHEVRON: f32 = 12.0;
    const RADIUS: f32 = 6.0;
    /// The narrowest a field is drawn, so a short value does not make a stub.
    pub const MIN_W: f32 = 120.0;

    pub const fn new(label: &'a str) -> Self {
        Self {
            label,
            font_stack: None,
            open: false,
            enabled: true,
        }
    }

    pub const fn in_face(mut self, stack: &'static str) -> Self {
        self.font_stack = Some(stack);
        self
    }

    pub const fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    pub const fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    fn value_layout(self, engine: &mut TextEngine) -> parley::Layout<theme::Brush> {
        match self.font_stack {
            Some(stack) => engine.simple_layout_in(self.label, theme::role::row(), stack),
            None => engine.simple_layout(self.label, theme::role::row()),
        }
    }

    /// The width the field needs for its current value.
    pub fn width(self, engine: &mut TextEngine) -> f32 {
        (self.value_layout(engine).width() + Self::PAD_X * 2.0 + Self::CHEVRON_GAP + Self::CHEVRON)
            .max(Self::MIN_W)
    }

    /// The width that fits the widest of `options` — so a field does not
    /// change size as its value changes.
    pub fn width_for(engine: &mut TextEngine, options: &[DropdownOption]) -> f32 {
        options
            .iter()
            .map(|option| {
                Dropdown {
                    label: &option.label,
                    font_stack: option.font_stack,
                    open: false,
                    enabled: true,
                }
                .width(engine)
            })
            .fold(Self::MIN_W, f32::max)
    }

    /// The list, hanging under the field and at least as wide as it.
    /// `field` is in the coordinates the menu will be drawn in.
    pub fn menu(
        engine: &mut TextEngine,
        field: Rect,
        options: Vec<DropdownOption>,
    ) -> Menu<DropdownOption> {
        let measured = crate::menu::measure_width(engine, &options);
        let selected = options.iter().position(|option| option.selected);
        let mut menu = Menu::at_width(
            (field.x, field.y + field.h + 4.0),
            options,
            Anchor::Cursor,
            measured.max(field.w),
        );
        // Open on the value in force, so Enter keeps it and the arrows move
        // from where the person already is.
        menu.hover = selected;
        menu
    }

    #[track_caller]
    pub fn draw(
        self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        hovered: bool,
        alpha: f32,
    ) {
        self.draw_focused(canvas, root, engine, rect, hovered, false, alpha);
    }

    /// Draw the field with an optional keyboard-focus ring.
    ///
    /// The field's border is the ring: while `focused` it is drawn in the
    /// palette's `focus_ring` at twice a border's weight, the mark
    /// [`crate::chip::Chip`] and [`crate::toggle::Toggle`] draw. An open
    /// field already wears that color at a border's weight, so a focused
    /// field reads as the same field, more so.
    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    pub fn draw_focused(
        self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        hovered: bool,
        focused: bool,
        alpha: f32,
    ) {
        let _component = inspect::scope(
            "KoruDropdown",
            "component",
            root,
            scene::rect(rect.x, rect.y, rect.w, rect.h),
            |r| {
                r.property(
                    "implementation",
                    "koru_ui::dropdown::Dropdown",
                    "shared implementation",
                );
                r.property("value", self.label, "caller value");
                r.property(
                    "face",
                    self.font_stack.unwrap_or("interface"),
                    "caller font stack",
                );
                r.property(
                    "open / hovered / enabled",
                    format!("{} / {} / {}", self.open, hovered, self.enabled),
                    "instance state",
                );
                r.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
            },
        );
        let p = theme::p();
        let alpha = if self.enabled { alpha } else { alpha * 0.45 };
        let shape = scene::rrect(rect.x, rect.y, rect.w, rect.h, Self::RADIUS);
        scene::fill(canvas, root, p.panel_bg.multiply_alpha(alpha), &shape);
        if (hovered || self.open) && self.enabled {
            scene::fill(canvas, root, p.hover_tint.multiply_alpha(alpha), &shape);
        }
        if focused {
            crate::button::focus_stroke(canvas, root, p, rect, Self::RADIUS, alpha);
        } else {
            scene::stroke(
                canvas,
                root,
                if self.open {
                    p.focus_ring
                } else {
                    p.panel_border
                }
                .multiply_alpha(alpha),
                theme::BORDER_W,
                &shape,
            );
        }
        let value = self.value_layout(engine);
        let line_h = scene::first_line_height(&value);
        let chevron_x = rect.x + rect.w - Self::PAD_X - Self::CHEVRON;
        // The value clips short of the chevron rather than running under it.
        let clip = scene::rect(
            rect.x,
            rect.y,
            (chevron_x - Self::CHEVRON_GAP - rect.x).max(0.0),
            rect.h,
        );
        canvas.push_clip_layer(vello::peniko::Fill::NonZero, root, &clip);
        crate::typography::line(
            "Dropdown value",
            canvas,
            root,
            &value,
            rect.x + Self::PAD_X,
            rect.y + (rect.h - line_h) * 0.5,
            theme::role::row(),
            "theme::role::row()",
            p.text.multiply_alpha(alpha),
        );
        canvas.pop_layer();
        let chevron = engine.icon_layout(icons::CARET_DOWN, Self::CHEVRON);
        let _chevron = inspect::scope(
            "Dropdown chevron",
            "part",
            root,
            scene::rect(
                chevron_x,
                rect.y + (rect.h - Self::CHEVRON) * 0.5,
                Self::CHEVRON,
                Self::CHEVRON,
            ),
            |r| r.property("glyph", "icons::CARET_DOWN", "shared implementation"),
        );
        scene::draw_layout(
            canvas,
            root,
            &chevron,
            chevron_x,
            rect.y + (rect.h - chevron.height()) * 0.5,
            Some(p.text_dim.multiply_alpha(alpha)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Vec<DropdownOption> {
        vec![
            DropdownOption::new("Short", false),
            DropdownOption::new("Considerably longer value", true),
        ]
    }

    #[test]
    fn a_field_is_as_wide_as_its_widest_option_so_it_never_resizes() {
        let mut engine = TextEngine::new();
        let widest = Dropdown::width_for(&mut engine, &options());
        assert!(widest >= Dropdown::new("Considerably longer value").width(&mut engine));
        assert!(widest >= Dropdown::new("Short").width(&mut engine));
        assert!(Dropdown::new("x").width(&mut engine) >= Dropdown::MIN_W);
    }

    #[test]
    fn the_list_hangs_under_the_field_and_opens_on_the_value_in_force() {
        let mut engine = TextEngine::new();
        let field = Rect::new(100.0, 50.0, 200.0, Dropdown::H);
        let menu = Dropdown::menu(&mut engine, field, options());
        let panel = menu.panel((1000.0, 800.0));
        assert_eq!(panel.x, field.x);
        assert!(panel.y >= field.y + field.h);
        assert!(panel.w >= field.w);
        assert_eq!(menu.hover, Some(1));
        assert_eq!(menu.items()[1].checked(), Some(true));
        assert_eq!(menu.items()[0].checked(), Some(false));
    }

    fn painted(focused: Option<bool>) -> Scene {
        let mut engine = TextEngine::new();
        let mut canvas = Scene::new();
        let root = Affine::IDENTITY;
        let rect = Rect::new(4.0, 4.0, 160.0, Dropdown::H);
        let field = Dropdown::new("Short");
        match focused {
            None => field.draw(&mut canvas, root, &mut engine, rect, false, 1.0),
            Some(focused) => {
                field.draw_focused(&mut canvas, root, &mut engine, rect, false, focused, 1.0);
            }
        }
        canvas
    }

    /// `draw` is `draw_focused` without the ring, path for path, so no
    /// existing caller changed. Focused, the border is the same one shape
    /// drawn heavier: nothing is added and nothing moves.
    #[test]
    fn the_focus_ring_is_the_fields_own_border_drawn_heavier() {
        let (plain, unfocused, focused) =
            (painted(None), painted(Some(false)), painted(Some(true)));
        let (a, b, c) = (plain.encoding(), unfocused.encoding(), focused.encoding());
        assert!(a.path_tags == b.path_tags);
        assert_eq!(a.path_data, b.path_data);
        assert!(a.draw_tags == b.draw_tags);
        assert_eq!(a.draw_data, b.draw_data);
        assert_eq!(a.styles, b.styles);
        assert!(
            a.draw_tags == c.draw_tags,
            "the same marks, in the same order"
        );
        assert_ne!(a.styles, c.styles, "the border is drawn at another weight");
        assert_ne!(a.draw_data, c.draw_data, "and in the focus color");
    }

    #[test]
    fn a_font_option_is_measured_and_drawn_in_its_own_face() {
        let option = DropdownOption::new("Lora", false).in_face("Lora, serif");
        assert_eq!(option.font_stack(), Some("Lora, serif"));
    }
}
