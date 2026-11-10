// SPDX-License-Identifier: MPL-2.0

//! The composer's microphone chooser (D103), driven as a host drives it: the
//! host pushes what it knows ([`ChatSurface::set_microphones`]) and takes what
//! was asked ([`ChatSurface::take_microphone_asks`]). No device, worker or
//! settings file is anywhere near.

use std::time::Instant;

use koru_draw::inspect;
use koru_i18n::Lang;
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::*;
use crate::dictation::DictationShown;

/// A composer whose host dictates and offers the chooser, on a machine with
/// a USB microphone (the system's default) and a headset with a long name.
fn surface() -> ChatSurface {
    let mut surface = ChatSurface::new();
    surface.lang = Lang::En;
    surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 700.0));
    surface.validated_ready = true;
    surface.offer_dictation(true);
    surface.offer_microphone_choice(true);
    surface.composer_rect();
    surface.set_microphones(Microphones {
        devices: vec![
            MicrophoneDevice::new("Microphone (USB Audio)", true),
            MicrophoneDevice::new(
                "Headset Microphone (Wireless Stereo Headset with a very long name indeed)",
                false,
            ),
        ],
        chosen: None,
        hold: true,
    });
    surface
}

fn centre(rect: Rect) -> (f32, f32) {
    (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5)
}

fn key(surface: &mut ChatSurface, named: NamedKey) -> bool {
    surface.key(&Key::Named(named), None, ModifiersState::empty())
}

/// Open the panel the way a person does: onto the microphone, then the
/// chevron that shows, then a press on it.
fn open(surface: &mut ChatSurface) {
    let mic = surface.mic_button().unwrap();
    let (x, y) = centre(mic);
    surface.mouse_move(x, y);
    let chevron = surface
        .mic_chooser_button()
        .expect("shown on the microphone");
    let (x, y) = centre(chevron);
    surface.mouse_move(x, y);
    assert!(surface.mouse_down(x, y));
    assert!(surface.microphone_panel_open());
}

fn labels(surface: &ChatSurface) -> Vec<String> {
    surface
        .microphone_spec()
        .devices
        .into_iter()
        .map(|device| device.label)
        .collect()
}

fn checked(surface: &ChatSurface) -> Vec<bool> {
    surface
        .microphone_spec()
        .devices
        .into_iter()
        .map(|device| device.chosen)
        .collect()
}

/// **The chevron expands right of the fixed microphone** on hover, and only
/// there: not before the pointer comes, not while the composer records, and
/// not where the host offers no choice. Its slot is kept either way.
#[test]
fn the_chevron_expands_right_of_the_microphone_on_hover() {
    let mut surface = surface();
    let mic = surface.mic_button().unwrap();
    let slot = surface.mic_chooser_slot().unwrap();
    assert!(mic.x + mic.w < slot.x, "right of the microphone");
    assert_eq!((slot.y, slot.h), (mic.y, mic.h));
    near(
        slot.w,
        koru_ui::icon_button::Metrics::COMPACT.control_width(),
        "the compact icon button's width",
    );
    let toolbar = surface.composer_toolbar_rect();
    assert!(toolbar.x + toolbar.w <= mic.x, "the toolbar gives way");

    assert!(surface.mic_chooser_button().is_none(), "not before a hover");
    let (x, y) = centre(mic);
    assert!(surface.mouse_move(x, y), "a redraw shows it");
    assert_eq!(surface.mic_chooser_button(), Some(slot));
    assert_eq!(surface.mic_button(), Some(mic), "hover keeps the mic fixed");
    let (x, y) = centre(slot);
    surface.mouse_move(x, y);
    assert_eq!(surface.mic_chooser_button(), Some(slot), "still on it");
    assert!(surface.mouse_move(10.0, 10.0), "a redraw takes it away");
    assert!(surface.mic_chooser_button().is_none());

    // Recording: the footer is the recording row, and there is no chevron.
    let (x, y) = centre(mic);
    surface.mouse_move(x, y);
    surface.show_dictation(
        Some(DictationShown {
            level: 0,
            listening: true,
            finishing: false,
        }),
        Instant::now(),
    );
    assert!(
        surface.mic_chooser_button().is_none(),
        "none while recording"
    );

    // A host that keeps no choice: the microphone alone.
    let mut bare = ChatSurface::new();
    bare.offer_dictation(true);
    bare.composer_rect();
    assert!(bare.mic_button().is_some());
    assert!(bare.mic_chooser_slot().is_none());
    let mic = bare.mic_button().unwrap();
    let toolbar = bare.composer_toolbar_rect();
    assert!(toolbar.x + toolbar.w <= mic.x);
}

fn near(a: f32, b: f32, what: &str) {
    assert!((a - b).abs() < 0.01, "{what}: {a}, not {b}");
}

/// **The panel**: *Default – <the system's device>* first and checked, then
/// a row a device, a divider, and *Hold to record* with its switch. It opens
/// next to the chooser, its right edge on the chooser's, and opening it
/// asks the host for the list again.
#[test]
fn the_chevron_opens_the_devices_with_default_first() {
    let mut surface = surface();
    open(&mut surface);
    assert_eq!(surface.take_microphone_asks(), [MicrophoneAsk::List]);
    assert_eq!(
        labels(&surface),
        [
            "Default – Microphone (USB Audio)",
            "Microphone (USB Audio)",
            "Headset Microphone (Wireless Stereo Headset with a very long name indeed)",
        ]
    );
    assert_eq!(checked(&surface), [true, false, false]);

    let panel = surface.microphone_panel().unwrap();
    let chooser = surface.mic_chooser_slot().unwrap();
    near(
        panel.x + panel.w,
        chooser.x + chooser.w,
        "right edge on the chooser's",
    );
    let gap = koru_draw::theme::overlay::popover().anchor_gap;
    assert!(
        (panel.y - chooser.y - chooser.h - gap).abs() < 0.5
            || (chooser.y - panel.y - panel.h - gap).abs() < 0.5,
        "panel stays beside the chooser on the side with room"
    );

    let (_, _, frame) = surface.microphone_frame().unwrap();
    let rows: Vec<Rect> = frame
        .named(elements::DEVICE_ROW)
        .map(|node| node.rect)
        .collect();
    assert_eq!(rows.len(), 3);
    let level = frame.rect_of(elements::LEVEL_ROW).unwrap();
    let divider = frame.rect_of(elements::DIVIDER).unwrap();
    let hold = frame.rect_of(elements::HOLD_ROW).unwrap();
    assert!(level.y + level.h <= rows[0].y, "the level first");
    assert!(rows[2].y + rows[2].h <= divider.y, "the divider after them");
    assert!(divider.y + divider.h <= hold.y, "Hold to record last");
    near(
        hold.y + hold.h + (level.y - panel.y),
        panel.y + panel.h,
        "the panel ends with it",
    );
    let switch = frame.rect_of(elements::HOLD_SWITCH).unwrap();
    near(
        switch.w,
        koru_ui::toggle::Toggle::TRACK_W,
        "the shared switch",
    );
    assert!(switch.x > hold.x + hold.w * 0.5, "the switch on the right");
    // A long name is elided before the ✓'s lane rather than run under it.
    let name = frame.named(elements::DEVICE_NAME).nth(2).unwrap().rect;
    let lane = frame.named(elements::CHECK_LANE).nth(2).unwrap().rect;
    assert!(name.x + name.w <= lane.x + 0.01, "{name:?} before {lane:?}");
    assert_eq!(frame.named(elements::CHECK).count(), 1, "one ✓");
}

/// **A device chosen is the Voice preference**: the host is asked to keep
/// it, the ✓ moves, and the panel stays open so its level bar shows the new
/// device. *Default* goes back to the system.
#[test]
fn choosing_a_device_asks_the_host_and_moves_the_check() {
    let mut surface = surface();
    open(&mut surface);
    surface.take_microphone_asks();
    let (_, _, frame) = surface.microphone_frame().unwrap();
    let headset = frame.named(elements::DEVICE_ROW).nth(2).unwrap().rect;
    let (x, y) = centre(headset);
    assert!(surface.mouse_down(x, y));
    let name = "Headset Microphone (Wireless Stereo Headset with a very long name indeed)";
    assert_eq!(
        surface.take_microphone_asks(),
        [MicrophoneAsk::Choose(Some(name.into()))]
    );
    assert!(
        surface.microphone_panel_open(),
        "a choice leaves the panel open"
    );
    assert_eq!(checked(&surface), [false, false, true]);

    let (_, _, frame) = surface.microphone_frame().unwrap();
    let default = frame.named(elements::DEVICE_ROW).next().unwrap().rect;
    let (x, y) = centre(default);
    surface.take_microphone_asks();
    surface.mouse_down(x, y);
    assert_eq!(
        surface.take_microphone_asks(),
        [MicrophoneAsk::Choose(None)]
    );
}

/// **An unplugged device falls back to Default**: the ✓ is on *Default*,
/// which dictation records from meanwhile, and the remembered choice is left
/// alone — nothing is asked of the host.
#[test]
fn a_missing_device_checks_default() {
    let mut surface = surface();
    surface.set_microphones(Microphones {
        devices: vec![MicrophoneDevice::new("Microphone (USB Audio)", true)],
        chosen: Some("Headset Microphone (gone)".into()),
        hold: true,
    });
    open(&mut surface);
    assert_eq!(surface.take_microphone_asks(), [MicrophoneAsk::List]);
    assert_eq!(checked(&surface), [true, false]);
    // A host re-casing the name is still the same device.
    surface.set_microphones(Microphones {
        devices: vec![MicrophoneDevice::new("Microphone (USB Audio)", true)],
        chosen: Some("microphone (usb audio)".into()),
        hold: true,
    });
    assert_eq!(checked(&surface), [false, true]);
    // No device known yet: *Default*, without a name.
    surface.set_microphones(Microphones::default());
    assert_eq!(labels(&surface), ["Default"]);
}

/// ***Hold to record* is the gesture**: the switch flips, the host is asked,
/// and the panel stays open.
#[test]
fn hold_to_record_flips_and_stays_open() {
    let mut surface = surface();
    open(&mut surface);
    surface.take_microphone_asks();
    assert!(surface.microphone_spec().hold);
    let (_, _, frame) = surface.microphone_frame().unwrap();
    let (x, y) = centre(frame.rect_of(elements::HOLD_ROW).unwrap());
    assert!(surface.mouse_down(x, y));
    assert_eq!(surface.take_microphone_asks(), [MicrophoneAsk::Hold(false)]);
    assert!(surface.microphone_panel_open());
    assert!(!surface.microphone_spec().hold);
}

/// **The keys are the approval-mode list's**: Down and Up move from the
/// device recorded from and wrap, Enter acts on the row lit and leaves the
/// panel open, Escape closes.
#[test]
fn arrows_enter_and_escape_work_the_panel() {
    let mut surface = surface();
    open(&mut surface);
    surface.take_microphone_asks();
    assert!(key(&mut surface, NamedKey::ArrowDown));
    assert_eq!(surface.dictation.microphone.focus, Some(1));
    assert!(surface.microphone_spec().devices[1].lit);
    assert!(key(&mut surface, NamedKey::ArrowDown));
    assert!(key(&mut surface, NamedKey::ArrowDown));
    assert!(surface.microphone_spec().hold_lit, "Hold to record last");
    assert!(key(&mut surface, NamedKey::Enter));
    assert_eq!(surface.take_microphone_asks(), [MicrophoneAsk::Hold(false)]);
    assert!(key(&mut surface, NamedKey::ArrowDown));
    assert_eq!(surface.dictation.microphone.focus, Some(0), "wraps");
    assert!(key(&mut surface, NamedKey::ArrowUp));
    assert!(key(&mut surface, NamedKey::ArrowUp));
    assert!(key(&mut surface, NamedKey::Enter));
    let name = "Headset Microphone (Wireless Stereo Headset with a very long name indeed)";
    assert_eq!(
        surface.take_microphone_asks(),
        [MicrophoneAsk::Choose(Some(name.into()))]
    );
    assert!(surface.microphone_panel_open());
    assert_eq!(
        surface.dictation.microphone.focus,
        Some(2),
        "the keyboard stays on the row it chose"
    );

    assert!(key(&mut surface, NamedKey::Escape));
    assert!(!surface.microphone_panel_open());
}

/// A press anywhere else closes the panel and still lands; a recording
/// beginning closes it too.
#[test]
fn a_press_elsewhere_or_a_recording_closes_the_panel() {
    let mut surface = surface();
    open(&mut surface);
    surface.mouse_down(5.0, 5.0);
    assert!(!surface.microphone_panel_open());
    open(&mut surface);
    surface.show_dictation(
        Some(DictationShown {
            level: 0,
            listening: false,
            finishing: false,
        }),
        Instant::now(),
    );
    assert!(!surface.microphone_panel_open());
    assert!(surface.microphone_panel().is_none());
}

/// **The words are translated**: *Default – …*, *Hold to record* and the
/// chevron's tooltip come from the catalog in every language.
#[test]
fn the_panels_words_come_from_the_catalog() {
    let mut surface = surface();
    surface.lang = Lang::De;
    let spec = surface.microphone_spec();
    assert_eq!(spec.devices[0].label, "Standard – Microphone (USB Audio)");
    assert_eq!(spec.hold_label, "Zum Aufnehmen halten");
    for lang in Lang::ALL {
        for key in [
            koru_i18n::Key::AiMicrophoneChoose,
            koru_i18n::Key::AiMicrophoneDefault,
            koru_i18n::Key::AiMicrophoneDefaultNamed,
            koru_i18n::Key::AiMicrophoneHoldToRecord,
        ] {
            assert!(!koru_i18n::t(lang, key).is_empty(), "{lang:?} {key:?}");
        }
        assert!(koru_i18n::t(lang, koru_i18n::Key::AiMicrophoneDefaultNamed).contains("{}"));
    }
}

/// **Everything drawn inside the panel nests under its `FloatingPanel`**,
/// as the composer's other panels do: each row, its name and ✓, the
/// divider, and the switch's `KoruToggle`.
#[test]
fn everything_drawn_inside_the_panel_nests_under_the_floating_panel() {
    let mut surface = surface();
    open(&mut surface);
    let capture = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        vello::kurbo::Rect::new(0.0, 0.0, 900.0, 700.0),
    );
    let mut scene = Scene::new();
    surface.draw_microphone_panel(&mut scene, Affine::IDENTITY);
    let snapshot = capture.finish();
    let records = &snapshot.records;
    let panel = records
        .iter()
        .position(|r| r.name == "FloatingPanel")
        .expect("a FloatingPanel");
    let under = |mut index: usize| loop {
        match records[index].parent {
            Some(parent) if parent == panel => return true,
            Some(parent) => index = parent,
            None => return false,
        }
    };
    for (index, record) in records.iter().enumerate() {
        if index != panel {
            assert!(
                under(index),
                "{} lies outside the FloatingPanel",
                record.name
            );
        }
    }
    for name in [
        elements::PANEL,
        elements::LEVEL_ROW,
        elements::DEVICE_ROW,
        elements::HOLD_ROW,
        "KoruToggle",
    ] {
        assert!(records.iter().any(|r| r.name == name), "{name} is drawn");
    }
}

// ------------------------------------------------------------ the input level

/// **The level row heads the panel**: the microphone glyph, then a thin
/// rounded bar the rest of the row's width, at its token thickness; the bar
/// shows what the host pushed, clamped, and a push that moves nothing asks
/// for no redraw.
#[test]
fn the_level_bar_heads_the_panel_and_shows_the_pushed_level() {
    let mut surface = surface();
    open(&mut surface);
    assert_eq!(surface.microphone_spec().level, 0, "empty when it opens");

    let (_, _, frame) = surface.microphone_frame().unwrap();
    let row = frame.rect_of(elements::LEVEL_ROW).unwrap();
    let glyph = frame.rect_of(elements::LEVEL_ICON).unwrap();
    let bar = frame.rect_of(elements::LEVEL_BAR).unwrap();
    let first = frame.named(elements::DEVICE_ROW).next().unwrap().rect;
    near(row.h, elements::ROW_HEIGHT.value, "a row's height");
    assert!(row.y + row.h <= first.y, "above the devices");
    assert!(glyph.x + glyph.w <= bar.x, "the glyph, then the bar");
    near(
        bar.h,
        elements::LEVEL_BAR_HEIGHT.value,
        "the bar's thickness",
    );
    near(
        bar.x + bar.w,
        first.x + first.w - elements::ROW_INSET.token.px(Profile::Desktop),
        "to the row's end inset",
    );
    assert!(
        frame.hit_at(bar.x + 1.0, bar.y + 1.0).is_none(),
        "the bar is not a row to press"
    );

    assert!(surface.set_microphone_level(4), "it moved: redraw");
    assert_eq!(surface.microphone_spec().level, 4);
    assert!(!surface.set_microphone_level(4), "nothing moved");
    surface.set_microphone_level(200);
    assert_eq!(
        surface.microphone_spec().level,
        crate::dictation::LEVEL_MAX,
        "louder than a bucket is a full bar"
    );
}

/// **The meter is wanted exactly while the panel is open**: not before, not
/// once Escape or a press elsewhere closed it, and not once a recording took
/// the footer. A level pushed while it is shut redraws nothing, and the next
/// opening starts empty.
#[test]
fn the_meter_is_wanted_only_while_the_panel_is_open() {
    let mut surface = surface();
    assert!(!surface.microphone_meter_wanted());
    open(&mut surface);
    assert!(surface.microphone_meter_wanted());
    surface.set_microphone_level(6);

    // Choosing another device keeps it wanted, and empties the bar until
    // that device is heard.
    let (_, _, frame) = surface.microphone_frame().unwrap();
    let (x, y) = centre(frame.named(elements::DEVICE_ROW).nth(1).unwrap().rect);
    surface.mouse_down(x, y);
    assert!(surface.microphone_meter_wanted());
    assert_eq!(surface.microphone_spec().level, 0);

    assert!(key(&mut surface, NamedKey::Escape));
    assert!(!surface.microphone_meter_wanted(), "Escape");
    assert!(!surface.set_microphone_level(3), "no bar to redraw");

    open(&mut surface);
    assert_eq!(surface.microphone_spec().level, 0, "nothing from before");
    surface.mouse_down(5.0, 5.0);
    assert!(!surface.microphone_meter_wanted(), "a press elsewhere");

    open(&mut surface);
    surface.show_dictation(
        Some(DictationShown {
            level: 0,
            listening: false,
            finishing: false,
        }),
        Instant::now(),
    );
    assert!(!surface.microphone_meter_wanted(), "a recording began");
}
