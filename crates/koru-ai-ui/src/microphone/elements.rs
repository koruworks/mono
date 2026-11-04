// SPDX-License-Identifier: MPL-2.0

//! **The microphone panel as elements** (koru-ai D103; `koru_draw::el`): one
//! tree gives every part of the open panel its place, and the same tree,
//! painted inside the panel's `FloatingPanel` record, nests everything drawn
//! in the panel under it in the inspector.
//!
//! ```text
//! col  Microphone panel                   the panel, MIC_PANEL_PAD round
//!   row  Microphone row                   MIC_ROW_H: Default – <name> first
//!     Microphone name                     row role, elided
//!     col  Check lane                     TRAILING_LANE_W, the ✓ at its end
//!   …                                     a row a device the worker saw
//!   col  Microphone divider               MIC_DIVIDER_GAP above and below
//!     Divider
//!   row  Hold to record row               MIC_ROW_H
//!     Hold to record words                row role, elided
//!     Hold switch                         the shared `Toggle`, in a leaf
//! ```
//!
//! A row lights on the hover plate under the pointer, and stays lit while
//! the keyboard is on it; the device recorded from has the accent ✓. Every
//! value is a token or a tagged metric ([`super::tokens`]); this file is on
//! the source scan's migrated list (`koru_draw::theme::ui_scan`).

use koru_draw::el::{
    El, ElState, Frame, FrameCx, Key, Leaf, Look, Role, StateRecipe, col, divider, icon, leaf, row,
    text,
};
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::text::TextEngine;
use koru_draw::theme::{IconSize, Ink, Line, Radius, Surface, metrics, roles};
use koru_ui::toggle::Toggle;
use vello::Scene;
use vello::kurbo::Affine;

pub(crate) use super::tokens::*;

/// The developer names the panel reads its geometry back by.
pub(crate) const PANEL: &str = "Microphone panel";
pub(crate) const DEVICE_ROW: &str = "Microphone row";
pub(crate) const DEVICE_NAME: &str = "Microphone name";
pub(crate) const CHECK_LANE: &str = "Check lane";
pub(crate) const CHECK: &str = "Chosen check";
pub(crate) const DIVIDER: &str = "Microphone divider";
pub(crate) const HOLD_ROW: &str = "Hold to record row";
pub(crate) const HOLD_WORDS: &str = "Hold to record words";
pub(crate) const HOLD_SWITCH: &str = "Hold switch";

const SURFACE: &str = "composer-microphone";
/// *Default – …*: the system's own device, whatever it is.
pub(crate) const DEFAULT_KEY: Key = Key::surface(SURFACE, "default");
/// *Hold to record*.
pub(crate) const HOLD_KEY: Key = Key::surface(SURFACE, "hold");

/// A device's row, by its name: the same device gives the same key in every
/// frame, wherever the worker lists it.
pub(crate) fn device_key(name: &str) -> Key {
    Key::model(crate::model_picker::stable_id(name), "microphone")
}

/// One microphone row as the panel draws it.
pub(crate) struct DeviceSpec {
    pub(crate) key: Key,
    pub(crate) label: String,
    /// Dictation records from it: the ✓.
    pub(crate) chosen: bool,
    /// The keyboard is on it.
    pub(crate) lit: bool,
}

/// What the panel shows.
pub(crate) struct Spec {
    /// *Default – …* first, then a row a device.
    pub(crate) devices: Vec<DeviceSpec>,
    pub(crate) hold_label: String,
    /// *Hold to record* is on: the switch's knob at its end.
    pub(crate) hold: bool,
    /// The keyboard is on *Hold to record*.
    pub(crate) hold_lit: bool,
}

/// **A row's states**: lit on the hover plate under the pointer.
pub(crate) static ROW_STATES: StateRecipe = StateRecipe {
    name: "microphone row",
    base: Look::NONE,
    selected: Look::NONE,
    hover: Look::bg(Surface::Hover),
    pressed: Look::NONE,
    selected_hover: Look::NONE,
    selected_pressed: Look::NONE,
    disabled: Look::NONE,
    focus: None,
};

/// The row the keyboard is on: on the hover plate whatever the pointer does.
pub(crate) static ROW_LIT_STATES: StateRecipe = StateRecipe {
    name: "microphone row, the keyboard's",
    base: Look::bg(Surface::Hover),
    ..ROW_STATES
};

fn states(lit: bool) -> &'static StateRecipe {
    if lit { &ROW_LIT_STATES } else { &ROW_STATES }
}

/// **A device's row**: its name, elided to the room before the ✓'s lane,
/// which is kept whether or not the ✓ shows.
fn device_row(spec: &DeviceSpec) -> El {
    let mut lane = col(CHECK_LANE).w(metrics::TRAILING_LANE_W).items_end();
    if spec.chosen {
        lane = lane.child(
            icon(icons::CHECK, IconSize::S)
                .decorative()
                .ink(Ink::Accent)
                .named(CHECK),
        );
    }
    row(DEVICE_ROW)
        .h(ROW_HEIGHT)
        .radius(Radius::M)
        .pad_x(ROW_INSET)
        .gap(ROW_GAP)
        .items_center()
        .states(states(spec.lit))
        .id(spec.key)
        .hit()
        .semantics(Role::MenuItem)
        .name_text(spec.label.clone())
        .child(
            text(spec.label.clone())
                .named(DEVICE_NAME)
                .role(roles::ROW)
                .ink(Ink::Primary)
                .fill()
                .truncate(),
        )
        .child(lane)
}

/// **The shared switch** (`koru_ui::toggle::Toggle`), drawn in its own
/// size at the row's end.
struct HoldSwitch {
    on: bool,
}

impl Leaf for HoldSwitch {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        let toggle = Toggle::new(self.on);
        (toggle.width(), Toggle::H)
    }

    fn paint(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        state: ElState,
    ) {
        Toggle::new(self.on).draw(
            canvas,
            root,
            engine,
            at,
            false,
            state.hovered,
            state.focused,
        );
    }
}

/// **The panel as one tree**: the devices, a divider, and *Hold to record*.
pub(crate) fn tree(spec: &Spec) -> El {
    let hold = row(HOLD_ROW)
        .h(ROW_HEIGHT)
        .radius(Radius::M)
        .pad_x(ROW_INSET)
        .gap(ROW_GAP)
        .items_center()
        .states(states(spec.hold_lit))
        .id(HOLD_KEY)
        .hit()
        .semantics(Role::MenuItem)
        .name(koru_i18n::Key::AiMicrophoneHoldToRecord)
        .child(
            text(spec.hold_label.clone())
                .named(HOLD_WORDS)
                .role(roles::ROW)
                .ink(Ink::Primary)
                .fill()
                .truncate(),
        )
        .child(
            leaf(HOLD_SWITCH, HoldSwitch { on: spec.hold })
                .decorative()
                .named(HOLD_SWITCH),
        );
    col(PANEL)
        .pad(PANEL_INSET)
        .semantics(Role::Menu)
        .name(koru_i18n::Key::AiMicrophoneChoose)
        .children(spec.devices.iter().map(device_row))
        .child(
            col(DIVIDER)
                .pad_y(DIVIDER_GAP)
                .child(divider(Line::Divider)),
        )
        .child(hold)
}

/// `tree` laid out in `area`, the panel, under `cx`: on the engine and cache
/// the composer's panels share with the model picker.
pub(crate) fn layout(tree: &El, area: Rect, cx: FrameCx) -> Frame {
    crate::model_picker::layout_elements(tree, area, cx)
}
