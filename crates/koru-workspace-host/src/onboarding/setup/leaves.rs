// SPDX-License-Identifier: MPL-2.0

//! **The setup card's leaves**: the existing drawables the card is made of,
//! each a named `impl Leaf` that takes what it draws as a value built per
//! frame — the shared `TextInput`, the square profile and workspace `Tile`s
//! with hover pencil affordances, and the shared buttons. Colors come from
//! the frame's palette.
//!
//! This file is on the source scan's migrated list
//! (`koru_draw::theme::ui_scan`).

use std::cell::RefCell;
use std::rc::Rc;

use koru_draw::el::{ElState, FrameCx, Leaf};
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::text::TextEngine;
use koru_draw::tile::Tile;
use koru_draw::{inspect, scene};
use koru_ui::button::{ActionButton, ActionStyle, ButtonRole, button_width, draw_button_enabled};
use koru_ui::text_input::{InputStyle, TextInput};
use vello::Scene;
use vello::kurbo::Affine;

use super::tokens::{
    SETUP_ACTION_H, SETUP_CHIP, SETUP_FIELD_H, SETUP_PRIMARY_H, SETUP_RING_GAP, SETUP_RING_RADIUS,
    SETUP_RING_W, SETUP_TILE_HOVER_ICON, SETUP_TILE_HOVER_SCRIM, SETUP_TILE_RADIUS_RATIO,
};

/// **A name field**: the shared `TextInput`, drawn as a form's field in the
/// allocation the frame gives it. Held through `Rc<RefCell<…>>` because
/// `Leaf::paint` takes `&self` and the input draws with `&mut`.
pub(crate) struct NameField {
    pub(crate) input: Rc<RefCell<TextInput>>,
    pub(crate) placeholder: String,
    pub(crate) focused: bool,
    pub(crate) caret_on: bool,
    /// While the card is finishing: drawn without focus or caret.
    pub(crate) disabled: bool,
}

impl Leaf for NameField {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, SETUP_FIELD_H.value)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        _state: ElState,
    ) {
        let focused = self.focused && !self.disabled;
        self.input.borrow_mut().draw(
            scene,
            root,
            engine,
            at,
            InputStyle::form(focused).caret_shown(self.caret_on && focused),
            Some(&self.placeholder),
            focused,
        );
    }
}

/// **A square appearance tile** for either the profile or the workspace: its
/// emoji (or initial) and color, wearing a dark scrim with a centered pencil
/// on hover and a focus ring while the keyboard is on it. Clicking it opens
/// the full icon-and-color modal; that is the view's, not the leaf's.
pub(crate) struct TileLeaf {
    pub(crate) component: &'static str,
    pub(crate) icon: Option<String>,
    pub(crate) color: Option<String>,
    pub(crate) name: String,
    /// When `false`, an unset icon with a blank name draws an empty rounded
    /// square rather than `Tile`'s `'W'` workspace fallback.
    pub(crate) fallback_initial: bool,
    pub(crate) hovered: bool,
    pub(crate) focused: bool,
    pub(crate) disabled: bool,
}

impl Leaf for TileLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (SETUP_CHIP.value, SETUP_CHIP.value)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        let hovered = self.hovered && !self.disabled;
        let focused = self.focused && !self.disabled;
        let _icon = inspect::scope(
            self.component,
            "component",
            root,
            scene::rect(at.x, at.y, at.w, at.h),
            |record| {
                record.property("control", "Tile", "koru_draw::tile");
                record.property("hovered", hovered, "view state");
                record.property("focused", focused, "view state");
            },
        );
        let trimmed = self.name.trim();
        let icon = match self.icon.as_deref() {
            Some(icon) => Some(icon),
            None if !self.fallback_initial && trimmed.is_empty() => Some(""),
            None => None,
        };
        let palette = cx.palette();
        Tile {
            icon,
            color: self.color.as_deref(),
            name: trimmed,
        }
        .draw_in(scene, root, engine, at, palette);
        if hovered {
            let radius = at.w.min(at.h) * SETUP_TILE_RADIUS_RATIO;
            scene::fill(
                scene,
                root,
                palette.bubble_bg.multiply_alpha(SETUP_TILE_HOVER_SCRIM),
                &scene::rrect(at.x, at.y, at.w, at.h, radius),
            );
            let pencil = engine.icon_layout(icons::PENCIL, SETUP_TILE_HOVER_ICON.value);
            scene::draw_layout(
                scene,
                root,
                &pencil,
                at.x + (at.w - pencil.width()) * 0.5,
                at.y + (at.h - pencil.height()) * 0.5,
                Some(palette.bubble_fg),
            );
        }
        if focused {
            let out = SETUP_RING_GAP.value;
            scene::stroke(
                scene,
                root,
                palette.focus_ring,
                f64::from(SETUP_RING_W.value),
                &scene::rrect(
                    at.x - out,
                    at.y - out,
                    at.w + out * 2.0,
                    at.h + out * 2.0,
                    SETUP_RING_RADIUS.value,
                ),
            );
        }
    }
}

/// What a card button is for, which decides how it is drawn.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ButtonStyle {
    /// The card's one primary: monochrome ink-filled, the column's full width.
    Primary,
    /// *Resume account creation*: the outlined dialog button.
    Secondary,
    /// *Back*: the quiet compact action.
    Ghost,
}

impl ButtonStyle {
    /// The width the button needs for `label`, measured before the tree is
    /// built because a leaf measures without an engine. The primary takes
    /// the column, so it asks for nothing.
    pub(crate) fn width(self, engine: &mut TextEngine, label: &str) -> f32 {
        match self {
            Self::Primary => 0.0,
            Self::Secondary => button_width(engine, label),
            Self::Ghost => ActionButton::new(label, ActionStyle::Ghost, None).width(engine),
        }
    }

    fn height(self) -> f32 {
        match self {
            Self::Primary => SETUP_PRIMARY_H.value,
            Self::Secondary | Self::Ghost => SETUP_ACTION_H.value,
        }
    }
}

/// **A card button**: the shared dialog button or compact action, lit under
/// the pointer, ringed while the keyboard is on it, drawn inert while it
/// cannot act.
pub(crate) struct ButtonLeaf {
    pub(crate) label: String,
    pub(crate) width: f32,
    pub(crate) style: ButtonStyle,
    pub(crate) hovered: bool,
    pub(crate) focused: bool,
    pub(crate) disabled: bool,
}

impl Leaf for ButtonLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (self.width, self.style.height())
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        _state: ElState,
    ) {
        let enabled = !self.disabled;
        match self.style {
            ButtonStyle::Primary | ButtonStyle::Secondary => draw_button_enabled(
                scene,
                root,
                engine,
                at,
                &self.label,
                if self.style == ButtonStyle::Primary {
                    ButtonRole::Inverted
                } else {
                    ButtonRole::Secondary
                },
                self.hovered && enabled,
                self.focused && enabled,
                enabled,
            ),
            ButtonStyle::Ghost => {
                let button = ActionButton::new(&self.label, ActionStyle::Ghost, None);
                if enabled {
                    button.draw_focused(scene, root, engine, at, self.hovered, self.focused);
                } else {
                    button.draw_disabled(scene, root, engine, at);
                }
            }
        }
    }
}
