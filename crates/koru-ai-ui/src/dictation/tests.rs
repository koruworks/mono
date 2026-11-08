// SPDX-License-Identifier: MPL-2.0

//! The composer's voice input (D103), driven as a host drives it: the
//! microphone asks, the host's controller is played by hand through the
//! composer's own `DictationTarget` and [`ChatSurface::show_dictation`], and
//! no microphone, model or worker is anywhere near.

use std::time::{Duration, Instant};

use koru_ai_core::InferenceAttemptId;
use koru_ui::dictation::DictationTarget;
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::*;

fn surface() -> ChatSurface {
    let mut surface = ChatSurface::new();
    surface.validated_ready = true;
    surface.offer_dictation(true);
    surface.composer_rect();
    surface
}

fn listening(level: u8) -> Option<DictationShown> {
    Some(DictationShown {
        level,
        listening: true,
        finishing: false,
    })
}

fn centre(rect: Rect) -> (f32, f32) {
    (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5)
}

fn node(surface: &ChatSurface, key: ElKey) -> Rect {
    let (_, frame) = surface
        .dictation_frame(Instant::now())
        .expect("the row is shown");
    frame.node(key).expect("the button is in the row").rect
}

/// **The microphone sits just before send**, the send button's size, and a
/// press on it asks the host — once — with the keyboard in the composer.
#[test]
fn the_microphone_is_in_the_footer_just_before_send() {
    let mut bare = ChatSurface::new();
    bare.composer_rect();
    assert!(bare.mic_button().is_none(), "no microphone unless offered");

    let mut surface = surface();
    let send = surface.send_button();
    let mic = surface.mic_button().expect("offered, so shown");
    assert_eq!((mic.w, mic.h, mic.y), (send.w, send.h, send.y));
    assert!(mic.x + mic.w < send.x, "left of send: {mic:?} {send:?}");
    assert!(
        surface.composer_toolbar_rect().x + surface.composer_toolbar_rect().w <= mic.x,
        "the toolbar's controls give way to it"
    );
    assert!(surface.composer.is_blank(), "shown with no text as well");

    let (x, y) = centre(mic);
    assert!(surface.mouse_down(x, y));
    assert!(surface.take_wants_dictation(), "the host is asked");
    assert!(!surface.take_wants_dictation(), "once");
    assert_eq!(surface.focus, Focus::Composer);
}

/// **Recording, the footer is the recording row**: the waveform filling the
/// free width, the timer, ✕ and ■ — ■ where send was — and the microphone
/// and the toolbar's controls are gone. The text area is unchanged above.
#[test]
fn recording_turns_the_footer_into_waveform_timer_cancel_and_stop() {
    let mut surface = surface();
    let text_before = surface.composer_text_rect();
    let send = surface.send_button();
    let now = Instant::now();
    assert!(surface.show_dictation(listening(4), now));
    assert!(surface.dictating());

    let (_, frame) = surface.dictation_frame(now).expect("the row is shown");
    let waveform = frame.rect_of(WAVEFORM).expect("a waveform");
    let timer = frame.rect_of(TIMER).expect("a timer");
    let cancel = frame.node(CANCEL).expect("✕").rect;
    let stop = frame.node(STOP).expect("■").rect;
    assert!(
        waveform.x < timer.x && timer.x < cancel.x && cancel.x < stop.x,
        "in reading order"
    );
    assert!(
        waveform.w > timer.w + cancel.w + stop.w,
        "the waveform fills what is left: {waveform:?}"
    );
    assert!((stop.x - send.x).abs() < 0.5 && (stop.w - send.w).abs() < 0.5);
    assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
    assert_eq!(surface.composer_text_rect(), text_before, "the text stays");

    let mut scene = Scene::new();
    surface.draw(&mut scene, Affine::IDENTITY);
    assert!(
        surface.show_dictation(None, now),
        "the session's end is a frame"
    );
    assert!(!surface.dictating());
    assert!(
        surface.dictation_frame(now).is_none(),
        "and the row is gone"
    );
}

/// The timer reads minutes and seconds, from the moment the microphone
/// opened, and holds still once the recording is stopped.
#[test]
fn the_timer_counts_from_the_open_microphone_and_holds_at_the_stop() {
    assert_eq!(clock_text(Duration::from_secs(7)), "0:07");
    assert_eq!(clock_text(Duration::from_secs(83)), "1:23");
    assert_eq!(clock_text(Duration::from_secs(725)), "12:05");

    let start = Instant::now();
    let mut recording = Recording::new(
        DictationShown {
            listening: false,
            ..DictationShown::default()
        },
        start,
    );
    assert_eq!(
        recording.clock(start + Duration::from_secs(3)),
        Duration::ZERO
    );
    let open = start + Duration::from_secs(2);
    recording.update(listening(1).unwrap(), open);
    assert_eq!(
        recording.clock(open + Duration::from_secs(7)),
        Duration::from_secs(7)
    );
    let stop = open + Duration::from_secs(9);
    recording.update(
        DictationShown {
            finishing: true,
            ..listening(0).unwrap()
        },
        stop,
    );
    assert_eq!(
        recording.clock(stop + Duration::from_secs(30)),
        Duration::from_secs(9)
    );
}

/// **Partial words stream in provisional, then settle.** The composition is
/// the editor's own: open, it is drawn dim and underlined and is not the
/// message; committed, it is ordinary text after what was typed.
#[test]
fn partial_then_final_text_lands_at_the_caret() {
    let mut surface = surface();
    surface.composer.set_text("Hello ");
    let composer = surface.dictation_composer();
    assert!(composer.begin_composition(()));
    composer.update_composition((), "wor");
    composer.update_composition((), "world, how");
    assert!(composer.composing(), "drawn as provisional words");
    assert_eq!(composer.text(), "Hello ", "not the message yet");

    composer.commit_composition((), "world, how are you");
    assert!(!composer.composing(), "settled");
    assert_eq!(composer.text(), "Hello world, how are you");
}

/// **✕ throws away only what this recording put in**: the text typed before
/// it stays, byte for byte.
#[test]
fn cancel_removes_only_the_dictated_words() {
    let mut surface = surface();
    surface.composer.set_text("Typed first. ");
    let composer = surface.dictation_composer();
    assert!(composer.begin_composition(()));
    composer.update_composition((), "and then some spoken words");
    composer.cancel_composition(());
    assert!(!composer.composing());
    assert_eq!(composer.text(), "Typed first. ");
}

/// **What a press means while recording**: ✕ cancels; ■ — and any other
/// press on the composer — stops and keeps; a press elsewhere is ordinary.
#[test]
fn a_press_while_recording_is_cancel_on_the_cross_and_stop_on_the_composer() {
    let mut surface = surface();
    assert_eq!(
        surface.dictation_press_at(10.0, 10.0),
        None,
        "nothing is recording"
    );
    surface.show_dictation(listening(3), Instant::now());

    let (x, y) = centre(node(&surface, CANCEL));
    assert_eq!(
        surface.dictation_press_at(x, y),
        Some(DictationPress::Cancel)
    );
    let (x, y) = centre(node(&surface, STOP));
    assert_eq!(surface.dictation_press_at(x, y), Some(DictationPress::Stop));
    let (x, y) = centre(surface.composer_text_rect());
    assert_eq!(
        surface.dictation_press_at(x, y),
        Some(DictationPress::Stop),
        "a press in the text is spent on the stop"
    );
    let composer = surface.composer_hit_rect;
    assert_eq!(
        surface.dictation_press_at(composer.x + 4.0, composer.y - 40.0),
        None,
        "above the composer is the conversation's"
    );
    // A press that did reach the surface on the composer is spent there.
    assert_eq!(surface.dictation_mouse_down(x, y), Some(true));
}

/// **Escape while recording never arms the stop of a running reply** — the
/// host stops the recording with it — and once nothing records, Escape
/// arms it as before.
#[test]
fn escape_while_dictating_does_not_arm_the_reply_stop() {
    let mut surface = surface();
    surface.attempt = Some(InferenceAttemptId::new());
    surface.show_dictation(listening(2), Instant::now());
    let escape = Key::Named(NamedKey::Escape);

    assert!(surface.key(&escape, None, ModifiersState::empty()));
    assert!(surface.stop_armed_at.is_none(), "nothing is armed");
    assert!(
        !surface.key(&Key::Named(NamedKey::Shift), None, ModifiersState::SHIFT),
        "a modifier alone is not a key that stops"
    );

    surface.show_dictation(None, Instant::now());
    assert!(surface.key(&escape, None, ModifiersState::empty()));
    assert!(
        surface.stop_armed_at.is_some(),
        "not dictating: Escape arms"
    );
}

/// **Frames only while the microphone is open.** Nothing is due while
/// idle, while a model loads, once the recording is stopped, or after it
/// ends; while it listens, a frame is due every `FRAME` and each one moves
/// the waveform on — and none of it is `animating`, the host's fast wake.
#[test]
fn the_waveform_asks_for_frames_only_while_recording() {
    let mut surface = surface();
    let start = Instant::now();
    assert!(surface.dictation_deadline().is_none(), "idle");

    surface.show_dictation(
        Some(DictationShown {
            listening: false,
            ..DictationShown::default()
        }),
        start,
    );
    assert!(
        surface.dictation_deadline().is_none(),
        "the model is loading: nothing moves"
    );

    surface.show_dictation(listening(6), start);
    let due = surface
        .dictation_deadline()
        .expect("listening: a frame is due");
    assert!(due <= start + FRAME);
    assert!(!surface.animating(), "a deadline, never the 8 ms wake");
    assert!(!surface.maintain_dictation(start), "not before it is due");
    let later = start + BAR_EVERY * 3;
    assert!(surface.maintain_dictation(later), "a frame");
    let bars = surface.dictation.live.as_ref().unwrap().levels.clone();
    assert_eq!(bars, [6, 6, 6], "a bar a sample, at the level heard");
    assert!(!surface.maintain_dictation(later), "once per deadline");

    surface.show_dictation(
        Some(DictationShown {
            finishing: true,
            ..listening(0).unwrap()
        }),
        later,
    );
    assert!(
        surface.dictation_deadline().is_none(),
        "finishing: the line holds still"
    );
    surface.show_dictation(None, later);
    assert!(surface.dictation_deadline().is_none(), "ended");
    assert!(!surface.maintain_dictation(later + FRAME * 10));
}

/// A stall does not make the waveform loop through every sample it missed.
#[test]
fn a_stall_catches_up_a_few_bars_and_starts_again() {
    let start = Instant::now();
    let mut recording = Recording::new(listening(2).unwrap(), start);
    recording.sample(start + Duration::from_secs(60));
    assert_eq!(recording.levels.len(), CATCH_UP as usize);
    assert!(recording.phase(start + Duration::from_secs(60)) < 0.01);
}

/// **■ is spent once the recording is stopped**, and ✕ still cancels while
/// the last words are being finished.
#[test]
fn stop_is_disabled_while_finishing_and_cancel_still_works() {
    let mut surface = surface();
    let now = Instant::now();
    surface.show_dictation(listening(5), now);
    surface.show_dictation(
        Some(DictationShown {
            finishing: true,
            ..listening(0).unwrap()
        }),
        now,
    );
    let (_, frame) = surface.dictation_frame(now).unwrap();
    assert!(frame.node(STOP).unwrap().state.disabled, "■ is spent");
    let (x, y) = centre(frame.node(CANCEL).unwrap().rect);
    assert_eq!(
        surface.dictation_press_at(x, y),
        Some(DictationPress::Cancel)
    );
}

/// The two buttons are named in every language the app speaks, and the
/// microphone's tooltip teaches the shortcut documents use.
#[test]
fn the_controls_say_what_they_do() {
    for lang in koru_i18n::Lang::ALL {
        for key in [
            koru_i18n::Key::AiDictate,
            koru_i18n::Key::AiDictationCancel,
            koru_i18n::Key::AiDictationStop,
        ] {
            assert!(!koru_i18n::t(lang, key).is_empty(), "{lang:?} {key:?}");
        }
    }
    let mut surface = surface();
    let mic = surface.mic_button().unwrap();
    let (x, y) = centre(mic);
    assert!(surface.dictation_hover((0.0, 0.0), x, y));
    assert_eq!(
        surface.dictation.tooltip.label(),
        Some(koru_i18n::t(surface.lang, koru_i18n::Key::AiDictate))
    );
    if koru_keys::hint::shown(koru_keys::Section::Tooltip) {
        assert_eq!(
            surface.dictation.tooltip.shortcut(),
            Some(koru_keys::label_of(koru_keys::ShortcutId::Dictate).as_str())
        );
    }

    surface.show_dictation(listening(1), Instant::now());
    let (x, y) = centre(node(&surface, STOP));
    surface.dictation_hover((0.0, 0.0), x, y);
    assert_eq!(
        surface.dictation.tooltip.label(),
        Some(koru_i18n::t(surface.lang, koru_i18n::Key::AiDictationStop))
    );
}
