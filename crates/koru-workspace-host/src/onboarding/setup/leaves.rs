// SPDX-License-Identifier: MPL-2.0

//! **The setup card's leaves**: the existing drawables the card is made of,
//! each a named `impl Leaf` that takes what it draws as a value built per
//! frame — the shared `TextInput`, `KoruAvatar`, the workspace `Tile`,
//! `ColorPickerRow` in its wrapped form, and the shared buttons. Colors come
//! from the frame's palette.
//!
//! **No geometry lives here.** The two row leaves forward to their
//! components' own wrapped geometry, so paint, the view's pointer and the
//! screen reader all read one arithmetic, owned by the component. This file
//! is on the source scan's migrated list (`koru_draw::theme::ui_scan`).

use std::cell::RefCell;
use std::rc::Rc;

use koru_draw::el::{ElState, FrameCx, Leaf};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::tile::Tile;
use koru_draw::{inspect, scene};
use koru_ui::button::{ActionButton, ActionStyle, ButtonRole, button_width, draw_button_enabled};
use koru_ui::text_input::{InputStyle, TextInput};
use koru_ui::{ColorPickerRow, KoruAvatar};
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Color;

use super::tokens::{
    SETUP_ACTION_H, SETUP_CHIP, SETUP_DISABLED_ALPHA, SETUP_FIELD_H, SETUP_PRIMARY_H,
    SETUP_RING_GAP, SETUP_RING_RADIUS, SETUP_RING_W,
};

/// The ten named workspace colors the swatch row offers (D6), in the
/// palette's order (`theme::workspace_color_names`).
pub(crate) const SWATCH_COUNT: usize = 10;

/// The swatch row's geometry: ten swatches, no label. Paint fills in the
/// palette's colors; everything that only measures, places or hits reads
/// this, and both give one geometry because the colors do not move it.
const GEOMETRY: [Option<Color>; SWATCH_COUNT] = [None; SWATCH_COUNT];

pub(crate) fn swatch_geometry() -> ColorPickerRow<'static> {
    ColorPickerRow::new(&GEOMETRY)
}

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

/// **The person's chip**: `KoruAvatar`, the initial of what is typed in the
/// name field beside it, live. Decorative: the field is what is named.
pub(crate) struct AvatarLeaf {
    pub(crate) name: String,
}

impl Leaf for AvatarLeaf {
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
        KoruAvatar::initial(&self.name).draw_in(scene, root, engine, at, cx.palette());
    }
}

/// **The workspace tile**: its emoji and color as the workspace will wear
/// them, ringed while the keyboard is on it. Pressing it opens the full
/// picker (D5); that is the view's, not the leaf's.
pub(crate) struct TileLeaf {
    pub(crate) icon: Option<String>,
    pub(crate) color: Option<String>,
    pub(crate) name: String,
    pub(crate) focused: bool,
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
        let _icon = inspect::scope(
            "WorkspaceIcon",
            "component",
            root,
            scene::rect(at.x, at.y, at.w, at.h),
            |record| {
                record.property("control", "Tile", "koru_draw::tile");
                record.property("focused", self.focused, "view state");
            },
        );
        Tile {
            icon: self.icon.as_deref(),
            color: self.color.as_deref(),
            name: &self.name,
        }
        .draw_in(scene, root, engine, at, cx.palette());
        if self.focused {
            let out = SETUP_RING_GAP.value;
            scene::stroke(
                scene,
                root,
                cx.palette().focus_ring,
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

/// **The swatch row**: `ColorPickerRow` in its wrapped form, so a narrow
/// card puts the swatches on as many lines as it needs and never clips one.
/// `measure(Some(width))` is the component's `wrapped_height(width)`.
pub(crate) struct SwatchRowLeaf {
    pub(crate) selected: Option<usize>,
    pub(crate) hovered: Option<usize>,
    pub(crate) focused: Option<usize>,
    pub(crate) disabled: bool,
}

impl Leaf for SwatchRowLeaf {
    fn measure(&self, width: Option<f32>) -> (f32, f32) {
        let row = swatch_geometry();
        match width {
            Some(width) => (width, row.wrapped_height(width)),
            None => (row.width(), row.height()),
        }
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
        let colors = cx.palette().workspace_colors.map(|(_, color)| Some(color));
        ColorPickerRow::new(&colors)
            .selected(self.selected)
            .hovered(self.hovered.filter(|_| !self.disabled))
            .focused(self.focused.filter(|_| !self.disabled))
            .alpha(if self.disabled {
                SETUP_DISABLED_ALPHA
            } else {
                1.0
            })
            .draw_wrapped(scene, root, engine, at);
    }
}

/// What a card button is for, which decides how it is drawn.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ButtonStyle {
    /// The card's one primary: filled, the column's full width.
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
                    ButtonRole::Primary
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
