// SPDX-License-Identifier: MPL-2.0

//! **The microphone panel as elements** (koru-ai D103; `koru_draw::el`): one
//! tree gives every part of the open panel its place, and the same tree,
//! painted inside the panel's `FloatingPanel` record, nests everything drawn
//! in the panel under it in the inspector.
//!
//! ```text
//! col  Microphone panel                   the panel, MIC_PANEL_PAD round
//!   row  Microphone level row             MIC_ROW_H
//!     Microphone level icon               the microphone glyph, secondary ink
//!     Microphone level bar                MIC_LEVEL_BAR_H, round, filled to the level
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
//! the keyboard is on it; the device recorded from has the accent ✓. The
//! level bar is the dictation footer's waveform made one bar: its track a
//! divider line, its fill the primary ink the footer's lit bars use. Every
//! value is a token or a tagged metric ([`super::tokens`]); this file is on
//! the source scan's migrated list (`koru_draw::theme::ui_scan`).

use koru_draw::el::{
    El, ElState, Frame, FrameCx, Key, Leaf, Look, Role, StateRecipe, col, divider, icon, leaf, row,
    text,
};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::{IconSize, Ink, Line, Radius, Surface, metrics, roles};
use koru_draw::{icons, scene};
use koru_ui::toggle::Toggle;
use vello::Scene;
use vello::kurbo::Affine;

pub(crate) use super::tokens::*;

/// The developer names the panel reads its geometry back by.
pub(crate) const PANEL: &str = "Microphone panel";
pub(crate) const LEVEL_ROW: &str = "Microphone level row";
pub(crate) const LEVEL_ICON: &str = "Microphone level icon";
pub(crate) const LEVEL_BAR: &str = "Microphone level bar";
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
    /// The input level, `0..=`[`LEVEL_MAX`](crate::dictation::LEVEL_MAX).
    pub(crate) level: u8,
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

/// **The input level bar**: a rounded track the panel's width, filled from
/// its start to the level.
struct LevelBar {
    level: u8,
}

impl Leaf for LevelBar {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, LEVEL_BAR_HEIGHT.value)
    }

    fn paint(
        &self,
        canvas: &mut Scene,
        root: Affine,
        _engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        let palette = cx.palette();
        let round = at.h * 0.5;
        scene::fill(
            canvas,
            root,
            Line::Divider.of(palette),
            &scene::rrect(at.x, at.y, at.w, at.h, round),
        );
        let max = crate::dictation::LEVEL_MAX;
        let share = f32::from(self.level.min(max)) / f32::from(max);
        if share > 0.0 {
            // Never narrower than it is tall, so the quietest level is still
            // a round end rather than a sliver.
            let filled = (at.w * share).max(at.h);
            scene::fill(
                canvas,
                root,
                Ink::Primary.of(palette),
                &scene::rrect(at.x, at.y, filled, at.h, round),
            );
        }
    }
}

/// **The level row**: the microphone glyph, then the bar.
fn level_row(level: u8) -> El {
    row(LEVEL_ROW)
        .h(ROW_HEIGHT)
        .pad_x(ROW_INSET)
        .gap(ROW_GAP)
        .items_center()
        .child(
            icon(icons::MICROPHONE, IconSize::S)
                .decorative()
                .ink(Ink::Secondary)
                .named(LEVEL_ICON),
        )
        .child(
            leaf(LEVEL_BAR, LevelBar { level })
                .decorative()
                .fill()
                .named(LEVEL_BAR),
        )
}

/// **The panel as one tree**: the input level, the devices, a divider, and
/// *Hold to record*.
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
        .child(level_row(spec.level))
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
