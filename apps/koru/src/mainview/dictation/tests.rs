// SPDX-License-Identifier: MPL-2.0

//! **The listening surface's own suite.**
//!
//! Three things are being proved, and they are the three the plan says M5
//! inherits unchanged: the geometry has one copy and every element hit-tests
//! back to itself, the chip is wholly on screen wherever the caret is, and the
//! whole thing stops asking for frames the moment a session ends.
//!
//! Nothing here starts a worker. `koru-dictation`'s `no-engine` feature is on
//! in this crate's `[dev-dependencies]`, so a real `Dictation` mints ids and
//! generations and dials nothing — which is what lets the container be driven
//! in the order `App` drives it.

use super::*;
use crate::mainview::harness::*;
use koru_dictation::{Dictation, ErrorCode, Interruption};
use koru_workspace_host::View;

const VIEWPORT: (f32, f32) = (1200.0, 700.0);

/// Plausible measured widths. The numbers do not matter — what matters is that
/// drawing and hit-testing are handed the *same* ones, which is the whole
/// reason [`Metrics`] is a value rather than something measured twice.
fn metrics(hint: bool) -> Metrics {
    Metrics {
        icon_w: 15.0,
        icon_h: 18.0,
        state_w: 62.0,
        state_h: 18.0,
        hint_w: hint.then_some(74.0),
        hint_h: 14.0,
        level_h: 16.0,
    }
}

/// A caret at the middle and at every edge and corner of the window, plus two
/// that are already outside it — a scrolled row whose rect is above the
/// viewport is not hypothetical.
fn anchors() -> Vec<(&'static str, Rect)> {
    vec![
        ("middle", Rect::new(500.0, 300.0, 40.0, 20.0)),
        ("top-left", Rect::new(0.0, 0.0, 2.0, 20.0)),
        ("top-right", Rect::new(VIEWPORT.0, 0.0, 2.0, 20.0)),
        ("bottom-left", Rect::new(0.0, VIEWPORT.1 - 20.0, 2.0, 20.0)),
        (
            "bottom-right",
            Rect::new(VIEWPORT.0, VIEWPORT.1 - 20.0, 2.0, 20.0),
        ),
        ("above-the-viewport", Rect::new(400.0, -80.0, 2.0, 20.0)),
        (
            "below-the-viewport",
            Rect::new(400.0, VIEWPORT.1 + 80.0, 2.0, 20.0),
        ),
        ("left-of-the-viewport", Rect::new(-120.0, 300.0, 2.0, 20.0)),
    ]
}

// ------------------------------------------------------------- the geometry

/// **The rule the whole surface rests on.** A press has to land on the part it
/// is drawn over, and it does because there is only one piece of arithmetic —
/// so every element's own point comes back naming that element.
#[test]
fn every_element_hit_tests_back_to_itself() {
    for hint in [true, false] {
        for (name, anchor) in anchors() {
            let g = geometry(anchor, VIEWPORT, metrics(hint));
            let mut parts: Vec<(Element, Rect)> = vec![
                (Element::Mic, g.mic),
                (Element::State, g.state),
                (Element::Meter, g.meter),
            ];
            if let Some(rect) = g.hint {
                parts.push((Element::Hint, rect));
            }
            for (element, rect) in &parts {
                let (x, y) = rect.centre();
                assert_eq!(
                    g.hit(x, y),
                    Some(*element),
                    "{name} (hint={hint}): {element:?} does not hit-test back to itself"
                );
                assert!(
                    g.panel.contains(x, y),
                    "{name}: {element:?} is drawn outside the panel it sits on"
                );
            }
            // The padding is on the surface and on none of its parts, which is
            // what makes `Surface` a real answer rather than a fall-through.
            assert_eq!(
                g.hit(g.panel.x + 1.0, g.panel.y + 1.0),
                Some(Element::Surface),
                "{name}: the panel's own padding"
            );
            // …and nothing outside it is on it at all.
            assert_eq!(
                g.hit(g.panel.x - 2.0, g.panel.y - 2.0),
                None,
                "{name}: a press beside the chip is an ordinary local event"
            );
            // No two parts share a point, or the assertions above would pass
            // with the elements in the wrong order.
            for (i, (a, ra)) in parts.iter().enumerate() {
                for (b, rb) in parts.iter().skip(i + 1) {
                    assert!(!ra.intersects(*rb), "{name}: {a:?} and {b:?} overlap");
                }
            }
        }
    }
}

/// **The indicator stays inside the viewport when the caret is at every edge.**
#[test]
fn the_chip_is_always_wholly_on_screen() {
    for hint in [true, false] {
        for (name, anchor) in anchors() {
            let g = geometry(anchor, VIEWPORT, metrics(hint));
            let p = g.panel;
            assert!(p.x >= EDGE - 0.01, "{name}: past the left edge ({})", p.x);
            assert!(p.y >= EDGE - 0.01, "{name}: past the top edge ({})", p.y);
            assert!(
                p.x + p.w <= VIEWPORT.0 - EDGE + 0.01,
                "{name}: past the right edge ({})",
                p.x + p.w
            );
            assert!(
                p.y + p.h <= VIEWPORT.1 - EDGE + 0.01,
                "{name}: past the bottom edge ({})",
                p.y + p.h
            );
        }
    }
}

/// It hangs **below** the composition where there is room and **above** it
/// where there is not, rather than being clamped over the words it describes.
#[test]
fn it_flips_above_the_caret_at_the_foot_of_the_window() {
    let g = geometry(Rect::new(500.0, 300.0, 40.0, 20.0), VIEWPORT, metrics(true));
    assert!(g.panel.y > 320.0, "below an anchor with room under it");

    let low = Rect::new(500.0, VIEWPORT.1 - 30.0, 2.0, 20.0);
    let g = geometry(low, VIEWPORT, metrics(true));
    assert!(
        g.panel.y + g.panel.h <= low.y + 0.01,
        "with no room below, it goes above the caret rather than over it"
    );
}

/// A window narrower than the chip is a real window — three columns open on a
/// laptop — and clamping must not panic on an inverted range.
#[test]
fn a_window_smaller_than_the_chip_does_not_panic() {
    let tiny = (60.0, 40.0);
    let g = geometry(Rect::new(10.0, 10.0, 2.0, 20.0), tiny, metrics(true));
    assert_eq!((g.panel.x, g.panel.y), (EDGE, EDGE), "the low edge wins");
}

/// `show_hint` answering `None` means **draw nothing**, not draw an empty
/// column — so the chip is narrower by the whole run, gap included.
#[test]
fn the_chord_column_is_absent_rather_than_empty() {
    let anchor = Rect::new(500.0, 300.0, 40.0, 20.0);
    let with = geometry(anchor, VIEWPORT, metrics(true));
    let without = geometry(anchor, VIEWPORT, metrics(false));
    assert!(with.hint.is_some());
    assert!(without.hint.is_none());
    let m = metrics(true);
    assert!(
        (with.panel.w - without.panel.w - (GAP + m.hint_w.unwrap())).abs() < 0.01,
        "the hidden hint leaves no gap where it used to be"
    );
}

/// Nine cells, one per bucket, and the level is how many are lit.
#[test]
fn the_meter_lights_one_cell_per_bucket() {
    let g = geometry(Rect::new(500.0, 300.0, 40.0, 20.0), VIEWPORT, metrics(true));
    for step in 0..=LevelBucket::MAX {
        let lit = cells(g.meter, LevelBucket::new(step))
            .filter(|(_, lit)| *lit)
            .count();
        assert_eq!(lit as u8, step, "bucket {step}");
    }
    assert_eq!(cells(g.meter, LevelBucket::SILENT).count(), CELLS);
    // Every cell is inside the track the geometry gave it.
    for (cell, _) in cells(g.meter, LevelBucket::new(4)) {
        assert!(cell.x >= g.meter.x - 0.01);
        assert!(cell.x + cell.w <= g.meter.x + g.meter.w + 0.01);
    }
}

// ------------------------------------------------------------- what it says

fn status(stage: Stage, worker: State, level: u8) -> Status {
    Status {
        generation: 1,
        stage,
        worker,
        level: LevelBucket::new(level),
    }
}

/// The word is a pure function of the status, and the *gesture* wins: once the
/// key is up the surface says **Finishing** whatever the worker last said.
#[test]
fn the_state_word_is_a_pure_function_of_the_status() {
    use koru_i18n::Key::*;
    let cases = [
        (Stage::Live, State::Idle, VoiceStarting),
        (Stage::Live, State::Loading, VoiceStarting),
        (Stage::Live, State::Listening, VoiceListening),
        (Stage::Live, State::Finalising, VoiceFinishing),
        (Stage::Finalising, State::Listening, VoiceFinishing),
        (Stage::Finalising, State::Finalising, VoiceFinishing),
    ];
    for (stage, worker, expected) in cases {
        assert_eq!(
            state_key(status(stage, worker, 0)),
            expected,
            "{stage:?} / {worker:?}"
        );
    }
}

/// **Every string resolves in all eight languages**, and the surface refusal
/// wins over whichever code the controller borrowed to carry it.
#[test]
fn every_voice_string_resolves_in_all_eight_languages() {
    let mut keys = vec![
        koru_i18n::Key::VoiceStarting,
        koru_i18n::Key::VoiceListening,
        koru_i18n::Key::VoiceFinishing,
    ];
    for code in ErrorCode::ALL {
        keys.push(refusal_key(Refusal {
            code,
            surface: false,
        }));
        assert_eq!(
            refusal_key(Refusal {
                code,
                surface: true
            }),
            koru_i18n::Key::VoiceErrorSurface,
            "a surface refusal is the host's answer, whatever code carried it"
        );
    }
    for key in keys {
        for lang in koru_i18n::Lang::ALL {
            let s = koru_i18n::t(lang, key);
            assert!(!s.trim().is_empty(), "{key:?} in {lang:?} is empty");
        }
    }
}

/// The four the plan names, each with its own sentence rather than one shrug.
#[test]
fn the_four_named_failures_each_say_something_different() {
    let of = |code| {
        koru_i18n::t(
            koru_i18n::Lang::En,
            refusal_key(Refusal {
                code,
                surface: false,
            }),
        )
    };
    let said = [
        of(ErrorCode::CaptureFailed),
        of(ErrorCode::ModelMissing),
        of(ErrorCode::Busy),
        of(ErrorCode::WorkerUnavailable),
    ];
    for (i, a) in said.iter().enumerate() {
        for b in said.iter().skip(i + 1) {
            assert_ne!(a, b, "two failures a person can act on read the same");
        }
    }
}

// ------------------------------------------------------ through a container

/// Open a dictation into the document's body, the way the shell does.
fn dictating() -> (MainView, koru_workspace::Workspace, Dictation) {
    let (mut v, mut w) = on_document();
    let doc = v.document_root().expect("a document tab");
    w.ws.insert(
        koru_core::schema::Parent::Node(doc),
        koru_core::node::Node::paragraph("row".to_owned()).unwrap(),
    )
    .expect("insert");
    View::adopt(&mut v, &w.ws);
    View::set_viewport(&mut v, VIEWPORT.0, VIEWPORT.1);

    let mut controller = crate::mainview::harness::consenting_dictation();
    let action = controller
        .press(Instant::now())
        .expect("a press opens a session");
    let accepted = View::dictate(&mut v, &mut w.ws, &action);
    controller.began(accepted);
    assert!(accepted, "a plain document takes a dictation");
    assert!(View::dictation_status(&mut v, controller.status()));
    (v, w, controller)
}

/// **The anchor is the leaf's own rectangle, translated by the content
/// origin** — compared against the leaf's answer rather than against the
/// arithmetic, which is the discipline
/// `a_click_lands_in_the_same_place_through_the_container_as_without_it` sets.
#[test]
fn the_anchor_is_the_leafs_own_rect_translated_by_the_content_origin() {
    let (v, _w, _c) = dictating();
    let c = v.content();
    assert!(c.y > 0.0, "the caption row gives the content a real origin");
    let leaf = match &v.tabs().active_tab().view {
        TabView::Leaf(leaf) => leaf.dictation_anchor(),
        _ => None,
    }
    .expect("the leaf is composing and has a place");
    let anchor = v.dictation_anchor().expect("and the container converts it");
    assert!((anchor.x - (leaf.x + c.x)).abs() < 0.01, "x");
    assert!((anchor.y - (leaf.y + c.y)).abs() < 0.01, "y");
    assert_eq!((anchor.w, anchor.h), (leaf.w, leaf.h));
}

/// **`animating` is `false` with no session and one frame after one ends**, and
/// the meter runs on a deadline rather than on the shell's 8 ms wake.
#[test]
fn the_meter_never_asks_for_an_animation_frame() {
    let (v, _w) = on_document();
    assert!(!View::animating(&v), "nothing is running");
    assert!(v.dictation_deadline().is_none(), "and nothing is due");
    drop(v);

    let (mut v, mut w, mut controller) = dictating();
    assert!(v.listening.is_some(), "the surface is up");
    assert!(
        !View::animating(&v),
        "a level meter must not hold the shell at 8 ms frames"
    );
    let base = Instant::now();
    assert!(v.dictation_deadline().is_some(), "it is due on a clock");
    assert!(!v.tick_dictation(base), "not before the deadline");
    assert!(v.tick_dictation(base + METER_FRAME), "and then once");
    assert!(
        !v.tick_dictation(base + METER_FRAME),
        "once per deadline, not once per call"
    );

    // The session ends the way every interruption does.
    controller.interrupt(Interruption::FocusLost);
    for action in controller.pump(base) {
        View::dictate(&mut v, &mut w.ws, &action);
    }
    assert!(View::dictation_status(&mut v, controller.status()));
    assert!(v.listening.is_none(), "the surface went with the session");
    assert!(!View::animating(&v), "one frame after: still not animating");
    assert!(
        v.dictation_deadline().is_none(),
        "one frame after: nothing is due"
    );
    assert!(!v.tick_dictation(base + METER_FRAME * 10));
    assert!(v.dictation_geometry().is_none(), "and nothing is drawn");
}

/// A level bucket is a redraw and nothing more: the widths are measured when
/// the *word* moves, not nine times a second.
#[test]
fn a_louder_bucket_changes_the_picture_and_not_the_measurements() {
    let (mut v, _w, _c) = dictating();
    let before = v.listening.as_ref().expect("listening").metrics;
    let word = v.listening.as_ref().expect("listening").word;
    let loud = Status {
        level: LevelBucket::new(7),
        ..v.listening.as_ref().unwrap().status
    };
    assert!(View::dictation_status(&mut v, Some(loud)), "it changed");
    let after = v.listening.as_ref().expect("listening");
    assert_eq!(after.metrics, before, "no re-measure for a meter");
    assert_eq!(after.word, word);
    assert_eq!(after.status.level.step(), 7);
    assert!(
        !View::dictation_status(&mut v, Some(loud)),
        "and the same status again asks for no frame"
    );
}

/// **The one click that does not discard.** A press on the chip is the
/// microphone button; a press anywhere else is an ordinary local event.
#[test]
fn a_press_on_the_chip_is_the_one_click_that_does_not_discard() {
    let (v, _w, _c) = dictating();
    let g = v.dictation_geometry().expect("a chip on screen");
    let (x, y) = g.mic.centre();
    assert!(v.dictation_pressed(x, y), "the microphone");
    let (x, y) = g.panel.centre();
    assert!(v.dictation_pressed(x, y), "and the rest of the chip");
    assert!(
        !v.dictation_pressed(g.panel.x - 20.0, g.panel.y - 20.0),
        "a click beside it is an ordinary local event"
    );
}

/// A refusal is said where this app says everything that has already happened.
#[test]
fn a_refusal_is_said_in_the_toast_and_never_as_a_code() {
    let (mut v, _w) = on_document();
    assert!(v.toast.is_none());
    assert!(View::dictation_refused(
        &mut v,
        Refusal {
            code: ErrorCode::CaptureFailed,
            surface: false,
        }
    ));
    let (toast, _) = v.toast.as_ref().expect("a toast");
    assert_eq!(
        toast.message(),
        koru_i18n::t(crate::format::lang(), koru_i18n::Key::VoiceErrorMicrophone)
    );
    assert!(
        !toast.message().contains("capture_failed"),
        "a person never reads a wire code"
    );
}

/// **No `rgb()` and no new `Palette` field.** Every colour on this surface is
/// one the app already had, which is the standing precedent DESIGN sets and the
/// thing a later edit is most likely to break without noticing.
#[test]
fn the_surface_adds_no_colour_of_its_own() {
    let source = include_str!("../dictation.rs");
    for forbidden in ["rgb(", "from_rgb", "Rgba", "Color::new"] {
        assert!(
            !source.contains(forbidden),
            "`{forbidden}` belongs in theme.rs and nowhere else"
        );
    }
    for used in ["p.accent", "p.text_dim", "p.text", "p.scrollbar_thumb"] {
        assert!(
            source.contains(used),
            "{used} is what the chip is painted in"
        );
    }
}

/// **Light and dark are both painted.** The chip has no colour of its own, so
/// the only way it can be wrong in one theme is by not being drawn at all —
/// which is what this catches, along with a palette that stopped moving.
///
/// Serialised on the app's own theme lock: `theme::p()` is a process global and
/// cargo runs this binary's tests concurrently.
#[test]
fn the_chip_is_painted_in_light_and_in_dark() {
    let _guard = crate::tests::lock_theme();
    let was = theme::is_dark();
    let (mut v, _w, _c) = dictating();
    let mut palettes = Vec::new();
    for dark in [false, true] {
        theme::set_dark(dark);
        v.remeasure_dictation();
        let p = theme::p();
        palettes.push((p.panel_bg, p.text, p.text_dim, p.accent, p.scrollbar_thumb));
        let mut scene = Scene::new();
        v.draw_dictation(&mut scene, Affine::IDENTITY);
        assert!(
            !scene.encoding().is_empty(),
            "dark={dark}: the chip painted nothing"
        );
    }
    assert_ne!(
        palettes[0], palettes[1],
        "the two themes must not resolve to one palette"
    );
    theme::set_dark(was);
}

/// …and it paints nothing at all when nothing is being said, which is the same
/// claim as `animating` and worth asserting where the scene can see it.
#[test]
fn nothing_is_painted_when_nobody_is_speaking() {
    let (mut v, _w) = on_document();
    let mut scene = Scene::new();
    v.draw_dictation(&mut scene, Affine::IDENTITY);
    assert!(scene.encoding().is_empty());
}

// ------------------------------------------------------- the consent step

/// The offer a first-time person meets, with a figure that is easy to find in
/// a sentence.
fn offer() -> koru_workspace_host::voice_install::VoiceOffer {
    koru_workspace_host::voice_install::VoiceOffer {
        terms: vec![
            koru_voice_consent::Terms {
                model: "parakeet-tdt-v3".into(),
                version: "1.2.0".into(),
                licence: "Apache-2.0".into(),
                attribution: "k2-fsa".into(),
                // Exactly 300 MB, so the sentence carries a number this test can
                // look for without asserting a rounding rule that lives elsewhere.
                download_bytes: 300 * 1024 * 1024,
            },
            koru_voice_consent::Terms {
                model: "whisper-base".into(),
                version: "q5_1".into(),
                licence: "MIT".into(),
                attribution: "whisper.cpp".into(),
                download_bytes: 57 * 1024 * 1024,
            },
        ],
    }
}

/// **The consent step names each download's size and licence**, which is the
/// whole of what the plan asks it to say. Both come off the *signed*
/// descriptor, so the sentence and the record cannot disagree.
#[test]
fn the_consent_step_names_the_size_and_the_licence() {
    let (mut v, _w) = container();

    assert!(View::show_voice_consent(&mut v, offer()));

    let (dialog, _) = v.confirm.as_ref().expect("the consent step is open");
    // One line per model, each with its own size, rather than one total in a
    // sentence.
    assert!(dialog.body().contains("300 MB"), "{}", dialog.body());
    assert!(dialog.body().contains("57 MB"), "{}", dialog.body());
    assert!(
        dialog
            .body()
            .lines()
            .filter(|l| l.starts_with('\u{2022}'))
            .count()
            == 2,
        "{}",
        dialog.body()
    );
    assert!(dialog.body().contains("Apache-2.0"), "{}", dialog.body());
    assert!(dialog.body().contains("MIT"), "{}", dialog.body());
    assert!(dialog.body().contains("k2-fsa"), "{}", dialog.body());
    assert!(dialog.body().contains("whisper.cpp"), "{}", dialog.body());
    assert!(!dialog.title().is_empty());
}

/// **Enter is Cancel**, inherited from `Confirm` and load-bearing here: a modal
/// that appears under somebody's hands must not read the Enter they had already
/// begun as *yes, spend three hundred megabytes*.
#[test]
fn enter_does_not_agree_to_a_download() {
    let (mut v, mut w) = container();
    View::show_voice_consent(&mut v, offer());

    let handled = View::key(
        &mut v,
        &mut w.ws,
        &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        NONE,
    );

    assert!(handled, "a modal takes every key");
    assert!(v.confirm.is_none(), "the dialog closed");
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::VoiceConsent(false)),
        "Enter answered no"
    );
}

/// **Both answers are answers.** The shell holds a catalog entry until the step
/// is settled, so a Cancel that only closed the dialog would leave it holding
/// one forever and ignore the next press as *already offering*.
#[test]
fn both_answers_reach_the_shell() {
    for agreed in [true, false] {
        let (mut v, _w) = container();
        View::show_voice_consent(&mut v, offer());

        v.settle_confirm(agreed);

        assert_eq!(
            View::take_pane_request(&mut v),
            Some(koru_workspace_host::PaneRequest::VoiceConsent(agreed))
        );
        assert!(v.confirm.is_none());
    }
}

/// A question already on the screen is the one somebody is reading. A second
/// offer does not replace it — and says so, so the shell settles rather than
/// waiting for an answer nobody will give.
#[test]
fn a_second_offer_does_not_replace_the_open_one() {
    let (mut v, _w) = container();
    assert!(View::show_voice_consent(&mut v, offer()));
    assert!(!View::show_voice_consent(&mut v, offer()));
}

// ---------------------------------------------------- the progress toast

/// **Stop requests cancellation and leaves the toast up**, which is DESIGN's
/// contract: the stage becomes *Canceling*, the action goes so it cannot be
/// asked twice, and the shell closes the chip three seconds later.
#[test]
fn stop_asks_for_cancellation_and_keeps_the_toast() {
    let (mut v, w) = container();
    View::show_voice_download(&mut v, "Downloading\u{2026} 40%".into(), Some(0.4));
    let content = v.regions.content;
    let floor = v.toast_floor();
    let action = v
        .toast
        .as_ref()
        .expect("a progress toast")
        .0
        .action_rect(&mut v.engine, content, floor)
        .expect("Stop is on the chip");

    let hit = v.toast_mouse_down(&w.ws, action.x + action.w * 0.5, action.y + action.h * 0.5);

    assert!(hit);
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::CancelVoiceDownload)
    );
    assert!(v.toast.is_some(), "Stop does not close the toast");
}

/// **The close button only hides progress.** It never changes the operation —
/// no cancellation is asked for, which is the other half of the same contract.
#[test]
fn closing_the_progress_toast_cancels_nothing() {
    let (mut v, w) = container();
    View::show_voice_download(&mut v, "Downloading\u{2026} 40%".into(), Some(0.4));
    let content = v.regions.content;
    let floor = v.toast_floor();
    let close = v
        .toast
        .as_ref()
        .expect("a progress toast")
        .0
        .close_rect(&mut v.engine, content, floor)
        .expect("the chip has a close box");

    let hit = v.toast_mouse_down(&w.ws, close.x + close.w * 0.5, close.y + close.h * 0.5);

    assert!(hit);
    assert!(v.toast.is_none(), "the chip is hidden");
    assert_eq!(
        View::take_pane_request(&mut v),
        None,
        "hiding progress is not stopping the download"
    );
}

/// The accepted Stop takes the action off the chip in place, so it cannot be
/// asked for twice while the three seconds run out.
#[test]
fn an_accepted_stop_takes_its_own_action_away() {
    let (mut v, _w) = container();
    View::show_voice_download(&mut v, "Downloading\u{2026} 40%".into(), Some(0.4));
    let content = v.regions.content;
    let floor = v.toast_floor();

    assert!(View::cancel_voice_download(
        &mut v,
        "Downloading\u{2026} Canceling".into()
    ));

    let (toast, _) = v.toast.as_ref().expect("the chip is still up");
    assert!(toast.message().contains("Canceling"));
    assert!(
        toast.action_rect(&mut v.engine, content, floor).is_none(),
        "Stop can only be asked once"
    );
}

/// A stage arriving after somebody closed the chip is consumed rather than
/// recreating it — the container owns this feedback, and a surface that came
/// back would make the × mean nothing.
#[test]
fn a_stage_after_the_chip_was_closed_does_not_bring_it_back() {
    let (mut v, _w) = container();
    View::show_voice_download(&mut v, "Downloading\u{2026} 40%".into(), Some(0.4));
    v.toast = None;

    assert!(View::update_voice_download(
        &mut v,
        "Downloading\u{2026} 50%".into(),
        Some(0.5)
    ));
    assert!(v.toast.is_none());
}

/// Clearing a model download leaves a different toast alone. Two things can
/// have happened, and the other one's chip is not this one's to take away.
#[test]
fn clearing_the_download_does_not_take_another_toast() {
    let (mut v, _w) = container();
    View::show_notice(&mut v, "Moved to Trash".into());

    assert!(!View::clear_voice_download(&mut v));
    assert!(v.toast.is_some());
}
