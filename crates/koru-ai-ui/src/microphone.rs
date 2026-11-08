// SPDX-License-Identifier: MPL-2.0

//! **Choosing the microphone from the composer** (koru-ai D103).
//!
//! ```text
//!   idle, hovered   … [model]   ⌄ 🎤  ↑
//!                               └ opens, above the composer:
//!                                   🎤 ━━━━━━━━──────────────────
//!                                   Default – Microphone (USB)   ✓
//!                                   Microphone (USB)
//!                                   Headset (Bluetooth)
//!                                   ─────────────────────────
//!                                   Hold to record        [on]
//! ```
//!
//! Hovering the microphone shows a chevron just left of it; the chevron
//! opens this panel. Its slot is kept whether or not the chevron shows, so
//! nothing in the toolbar moves under the pointer. While the composer records
//! there is no chevron: the footer is the recording row.
//!
//! ## One list and one value, the Settings page's
//!
//! **Nothing here enumerates or stores anything.** The devices are what the
//! host's voice worker last said it could see, and the choice is the person's
//! *Voice → Microphone* preference (`dictation_input`), the same value the
//! Settings page and the Agent's tray show. The surface is handed both
//! ([`ChatSurface::set_microphones`]) and says what was asked
//! ([`ChatSurface::take_microphone_asks`]): read the list again when the
//! panel opens — off the UI thread, by the host — choose a device, or turn
//! *Hold to record* on or off. *Hold to record* is the hold-versus-toggle
//! gesture (`dictation_gesture`): on is push-to-talk.
//!
//! **A remembered device that is not plugged in** is not a row: the ✓ is on
//! *Default*, which is what dictation records from meanwhile
//! (`koru_voice::input::resolve`). The choice is left as it was, so the
//! device is used again when it comes back.
//!
//! **Choosing a device keeps the panel open**, so the level bar at its top
//! can be watched moving to the new microphone; Escape, the chevron or a
//! press elsewhere closes it.
//!
//! ## The input level, and who opens the microphone for it
//!
//! The top row is the microphone glyph and a thin bar filled to the input
//! level. **Nothing here opens a microphone either.** While the panel is open
//! the surface says so ([`ChatSurface::microphone_meter_wanted`]); the host
//! opens the voice worker's level meter — which recognises nothing and loads
//! no model — on the chosen device, closes it when the panel closes or a
//! dictation starts, and pushes each level it hears
//! ([`ChatSurface::set_microphone_level`]). The bar is redrawn when a pushed
//! level moves, at most the worker's ten a second, and never while the panel
//! is shut. A worker too old to meter pushes nothing, and the bar stays
//! empty.

use koru_draw::el::{Frame, FrameCx, Interaction};
use koru_draw::geometry::Rect;
use koru_draw::theme::{Profile, Space};
use koru_draw::{icons, scene};
use koru_i18n::{Key as Words, fill, t};
use koru_ui::icon_button::{self, Metrics, State as IconButtonState};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::ChatSurface;

mod elements;
#[cfg(test)]
mod tests;
mod tokens;

/// The panel's corners, as the composer's other panels have them.
const PANEL_RADIUS: f32 = 8.0;
/// Room enough for any machine's microphones when the panel is measured.
const MEASURE_H: f32 = 10_000.0;

/// **One capture device**, as the host's voice worker named it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicrophoneDevice {
    /// What the operating system calls it, and what a choice stores.
    pub name: String,
    /// The device the system opens when nobody chose.
    pub default: bool,
}

impl MicrophoneDevice {
    pub fn new(name: impl Into<String>, default: bool) -> Self {
        Self {
            name: name.into(),
            default,
        }
    }
}

/// **What the host knows about microphones**: the devices its voice worker
/// last listed, the person's choice, and the gesture.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Microphones {
    /// In the worker's order. Empty until it has answered.
    pub devices: Vec<MicrophoneDevice>,
    /// The device chosen, by name; `None` follows the system default.
    pub chosen: Option<String>,
    /// *Hold to record*: push-to-talk. Off, a press starts and another stops.
    pub hold: bool,
}

impl Microphones {
    /// The listed device the choice names — exactly, then ignoring case, as
    /// `koru_voice::input::resolve` matches it. `None` for the system
    /// default, and for a chosen device that is not plugged in, which
    /// dictation then records in place of.
    fn recorded_from(&self) -> Option<&str> {
        let wanted = self.chosen.as_deref()?;
        self.devices
            .iter()
            .find(|device| device.name == wanted)
            .or_else(|| {
                self.devices
                    .iter()
                    .find(|device| device.name.eq_ignore_ascii_case(wanted))
            })
            .map(|device| device.name.as_str())
    }
}

/// **What the panel asked the host for.**
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MicrophoneAsk {
    /// The panel opened: ask the voice worker which devices there are. It
    /// blocks on a socket, so never on the UI thread.
    List,
    /// Record from this device from now on; `None` follows the system
    /// default.
    Choose(Option<String>),
    /// *Hold to record* on or off.
    Hold(bool),
}

/// The panel's state, kept on the composer's dictation.
#[derive(Default)]
pub(crate) struct Panel {
    /// The host can read the devices and keep a choice: the chevron shows.
    pub(crate) offered: bool,
    open: bool,
    /// The row the keyboard is on.
    focus: Option<usize>,
    known: Microphones,
    asks: Vec<MicrophoneAsk>,
    /// The input level the host last pushed, `0..=`
    /// [`LEVEL_MAX`](crate::dictation::LEVEL_MAX). Empty whenever the panel
    /// opens or shuts, so a bar never shows a level from before.
    level: u8,
}

/// What a row of the panel does.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    /// Record from this device, or the system default.
    Device(Option<String>),
    Hold,
}

impl ChatSurface {
    /// **Offer the microphone chooser**: the host can list the devices off
    /// its UI thread and keep the person's choice. Without it the microphone
    /// has no chevron.
    pub fn offer_microphone_choice(&mut self, offered: bool) {
        self.dictation.microphone.offered = offered;
        if !offered {
            self.close_microphones();
        }
    }

    /// **What the host knows about microphones**, pushed whenever it may have
    /// moved. Answers whether anything drawn changed.
    pub fn set_microphones(&mut self, microphones: Microphones) -> bool {
        let panel = &mut self.dictation.microphone;
        if panel.known == microphones {
            return false;
        }
        panel.known = microphones;
        panel.focus = None;
        panel.open
    }

    /// What the panel asked for since the host last took it.
    pub fn take_microphone_asks(&mut self) -> Vec<MicrophoneAsk> {
        std::mem::take(&mut self.dictation.microphone.asks)
    }

    /// Whether the microphone panel is open.
    pub fn microphone_panel_open(&self) -> bool {
        self.dictation.microphone.open
    }

    /// **Whether the host should hold a level meter open for this panel**:
    /// while it is open, and never while the composer records — a dictation
    /// has the microphone. The host meters the chosen device, so a choice
    /// moves the meter with it.
    pub fn microphone_meter_wanted(&self) -> bool {
        self.dictation.microphone.open && !self.dictating()
    }

    /// **The input level the host's meter heard**, `0..=8` as the worker
    /// buckets it; louder is clamped. Answers whether the open panel's bar
    /// moved, so the host redraws only then.
    pub fn set_microphone_level(&mut self, level: u8) -> bool {
        let level = level.min(crate::dictation::LEVEL_MAX);
        let shown = self.microphone_meter_wanted();
        let panel = &mut self.dictation.microphone;
        if panel.level == level {
            return false;
        }
        panel.level = level;
        shown
    }

    /// The chevron's slot, just left of the microphone, while nothing
    /// records. The toolbar ends before it whether or not it shows.
    pub(crate) fn mic_chooser_slot(&self) -> Option<Rect> {
        if !self.dictation.microphone.offered {
            return None;
        }
        if let Some(shell) = &self.composer_shell {
            return shell.microphone_choice();
        }
        let mic = self.mic_button()?;
        let width = Metrics::COMPACT.control_width();
        let gap = Space::S0_5.px(Profile::Desktop);
        Some(Rect::new(mic.x - gap - width, mic.y, width, mic.h))
    }

    /// **The chevron**, where it shows: while the pointer is on it or on the
    /// microphone, or its panel is open — never while recording.
    pub(crate) fn mic_chooser_button(&self) -> Option<Rect> {
        if self.dictating() {
            return None;
        }
        let slot = self.mic_chooser_slot()?;
        let shown = self.dictation.microphone.open
            || self
                .mic_chooser_zone()
                .is_some_and(|zone| zone.contains(self.pointer.0, self.pointer.1));
        shown.then_some(slot)
    }

    /// Where the pointer shows the chevron: on it, on the microphone, or
    /// between them.
    pub(crate) fn mic_chooser_zone(&self) -> Option<Rect> {
        let slot = self.mic_chooser_slot()?;
        let mic = self.mic_button()?;
        Some(Rect::new(slot.x, mic.y, mic.x + mic.w - slot.x, mic.h))
    }

    /// The panel's rows, in order: *Default*, a row a device, *Hold to
    /// record*.
    fn microphone_rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::Device(None)];
        rows.extend(
            self.dictation
                .microphone
                .known
                .devices
                .iter()
                .map(|device| Row::Device(Some(device.name.clone()))),
        );
        rows.push(Row::Hold);
        rows
    }

    /// What the open panel's tree declares: *Default – …* first, a row a
    /// device, the ✓ on the one recorded from, and *Hold to record*.
    fn microphone_spec(&self) -> elements::Spec {
        let lang = self.lang;
        let known = &self.dictation.microphone.known;
        let focus = self.dictation.microphone.focus;
        let recorded = known.recorded_from();
        let default = match known.devices.iter().find(|device| device.default) {
            Some(device) => fill(
                t(lang, Words::AiMicrophoneDefaultNamed),
                &[device.name.as_str()],
            ),
            None => t(lang, Words::AiMicrophoneDefault).to_owned(),
        };
        let mut devices = vec![elements::DeviceSpec {
            key: elements::DEFAULT_KEY,
            label: default,
            chosen: recorded.is_none(),
            lit: focus == Some(0),
        }];
        devices.extend(known.devices.iter().enumerate().map(|(index, device)| {
            elements::DeviceSpec {
                key: elements::device_key(&device.name),
                label: device.name.clone(),
                chosen: recorded == Some(device.name.as_str()),
                lit: focus == Some(index + 1),
            }
        }));
        elements::Spec {
            level: self.dictation.microphone.level,
            hold_lit: focus == Some(devices.len()),
            devices,
            hold_label: t(lang, Words::AiMicrophoneHoldToRecord).to_owned(),
            hold: known.hold,
        }
    }

    /// The open panel: above the composer, its right edge on the
    /// microphone's, as tall as its rows.
    pub(crate) fn microphone_panel(&self) -> Option<Rect> {
        if !self.dictation.microphone.open || self.dictating() {
            return None;
        }
        let mic = self.mic_button()?;
        let width = self
            .composer_hit_rect
            .w
            .min(elements::PANEL_WIDTH.value)
            .max(1.0);
        let tree = elements::tree(&self.microphone_spec());
        let measured = elements::layout(
            &tree,
            Rect::new(0.0, 0.0, width, MEASURE_H),
            FrameCx::root(),
        );
        let hold = measured.rect_of(elements::HOLD_ROW)?;
        let top = measured.rect_of(elements::LEVEL_ROW)?.y;
        let height = hold.y + hold.h + top;
        Some(self.above_composer(mic.x + mic.w - width, width, height))
    }

    /// **The open panel's frame**: where a press lands and which row it is
    /// — what is drawn, laid out without the pointer.
    fn microphone_frame(&self) -> Option<(Rect, elements::Spec, Frame)> {
        let panel = self.microphone_panel()?;
        let spec = self.microphone_spec();
        let frame = elements::layout(&elements::tree(&spec), panel, FrameCx::root());
        Some((panel, spec, frame))
    }

    /// The row a press or the pointer at `x`, `y` is on.
    pub(crate) fn microphone_row_at(&self, x: f32, y: f32) -> Option<usize> {
        let (_, spec, frame) = self.microphone_frame()?;
        let key = frame.hit_at(x, y)?;
        if key == elements::HOLD_KEY {
            return Some(spec.devices.len());
        }
        spec.devices.iter().position(|device| device.key == key)
    }

    fn open_microphones(&mut self) -> bool {
        self.tool_access_open = false;
        self.tool_mode_open = false;
        self.close_approval_modes();
        if self.chat_model_open {
            self.close_picker();
        }
        let panel = &mut self.dictation.microphone;
        panel.open = true;
        panel.focus = None;
        panel.level = 0;
        panel.asks.push(MicrophoneAsk::List);
        true
    }

    pub(crate) fn close_microphones(&mut self) -> bool {
        let panel = &mut self.dictation.microphone;
        let was = panel.open;
        panel.open = false;
        panel.focus = None;
        panel.level = 0;
        was
    }

    /// **Act on a row**: a device is recorded from from now on, *Hold to
    /// record* flips, and the panel stays open either way — after a choice,
    /// so the level bar shows the new device hearing.
    fn press_microphone_row(&mut self, index: usize) -> bool {
        let Some(row) = self.microphone_rows().into_iter().nth(index) else {
            return false;
        };
        let panel = &mut self.dictation.microphone;
        match row {
            Row::Device(name) => {
                // Shown at once; the host's next push confirms it. The bar
                // starts empty: the level it held was the last device's.
                if panel.known.chosen != name {
                    panel.level = 0;
                }
                panel.known.chosen = name.clone();
                panel.asks.push(MicrophoneAsk::Choose(name));
            }
            Row::Hold => {
                panel.known.hold = !panel.known.hold;
                panel.asks.push(MicrophoneAsk::Hold(panel.known.hold));
            }
        }
        panel.focus = Some(index);
        true
    }

    /// **A press**: the chevron opens and shuts the panel, a row acts, and
    /// a press anywhere else shuts the panel and goes on (`None`).
    pub(crate) fn microphone_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        if let Some(index) = self.microphone_row_at(x, y) {
            return Some(self.press_microphone_row(index));
        }
        if self
            .mic_chooser_button()
            .is_some_and(|chevron| chevron.contains(x, y))
        {
            self.dictation.tooltip_clear();
            return Some(if self.dictation.microphone.open {
                self.close_microphones()
            } else {
                self.open_microphones()
            });
        }
        if self.dictation.microphone.open {
            // A press inside the panel but on no row is the panel's, and
            // leaves it open.
            if self
                .microphone_panel()
                .is_some_and(|panel| panel.contains(x, y))
            {
                return Some(false);
            }
            self.close_microphones();
        }
        None
    }

    /// The open panel's keys: Up and Down move, Enter acts, Escape closes —
    /// the approval-mode list's. `None` when the panel is shut or the key is
    /// not one of them.
    pub(crate) fn microphone_key(&mut self, key: &Key, mods: ModifiersState) -> Option<bool> {
        if !self.dictation.microphone.open || !mods.is_empty() {
            return None;
        }
        let count = self.microphone_rows().len();
        let chosen = self.microphone_spec().devices.iter().position(|d| d.chosen);
        let focused = self.dictation.microphone.focus.or(chosen).unwrap_or(0);
        match key {
            Key::Named(NamedKey::Escape) => Some(self.close_microphones()),
            Key::Named(NamedKey::ArrowDown) => {
                self.dictation.microphone.focus = Some((focused + 1) % count);
                Some(true)
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.dictation.microphone.focus = Some((focused + count - 1) % count);
                Some(true)
            }
            Key::Named(NamedKey::Enter) => Some(self.press_microphone_row(focused)),
            _ => None,
        }
    }

    /// The hover part for redraws: each row of the open panel.
    pub(crate) fn microphone_hover_at(&self, x: f32, y: f32) -> Option<(u8, usize)> {
        self.microphone_row_at(x, y).map(|index| (19, index))
    }

    /// The chevron, beside the microphone, while it shows.
    pub(crate) fn draw_mic_chooser(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(chevron) = self.mic_chooser_button() else {
            return;
        };
        icon_button::draw_with(
            canvas,
            root,
            &mut self.engine,
            chevron,
            icons::CARET_DOWN,
            IconButtonState {
                hovered: chevron.contains(self.pointer.0, self.pointer.1),
                active: self.dictation.microphone.open,
                disabled: false,
            },
            Metrics::COMPACT,
        );
    }

    /// **The open panel**: its element tree ([`elements`]) laid out with the
    /// pointer and the keyboard on it and painted while the panel's
    /// `FloatingPanel` record is open, so every row nests under the panel in
    /// the inspector.
    pub(crate) fn draw_microphone_panel(&mut self, canvas: &mut Scene, root: Affine) {
        let Some((panel, spec, targets)) = self.microphone_frame() else {
            return;
        };
        let hot = targets.hit_at(self.pointer.0, self.pointer.1);
        let tree = elements::tree(&spec);
        let frame = elements::layout(
            &tree,
            panel,
            FrameCx::root().with_interaction(Interaction {
                hot,
                ..Interaction::default()
            }),
        );
        let _panel = scene::floating_panel_scope(
            canvas,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(PANEL_RADIUS),
        );
        frame.paint(&tree, canvas, root, &mut self.engine);
    }
}
