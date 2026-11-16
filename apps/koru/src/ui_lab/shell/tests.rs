// SPDX-License-Identifier: MPL-2.0

//! The window driven the way the runner drives it — a draw, then pointer and
//! key events against that frame — with a real text engine and a scene, and
//! no window, GPU or event loop.

use super::*;

/// A Lab that has drawn one frame at `size`.
fn drawn(size: (f32, f32)) -> (Lab, TextEngine) {
    let mut engine = TextEngine::new();
    let mut lab = Lab::new(Placement::in_memory(Some(size)));
    draw(&mut lab, &mut engine, size);
    (lab, engine)
}

fn draw(lab: &mut Lab, engine: &mut TextEngine, size: (f32, f32)) {
    let mut scene = Scene::new();
    lab.draw(&mut scene, engine, size.0, size.1);
}

fn control(lab: &Lab, action: caption::Action) -> (f32, f32) {
    let caption = lab.caption_rect().expect("a drawn Lab has a caption");
    caption::direct_controls(caption)
        .into_iter()
        .find(|control| control.action == action)
        .expect("the caption has the three direct controls")
        .rect
        .centre()
}

const SIZE: (f32, f32) = (1120.0, 720.0);

#[test]
fn the_frame_composes_without_a_diagnostic() {
    let (lab, _) = drawn(SIZE);
    let laid = lab.laid.as_ref().unwrap();
    assert!(
        laid.frame.diagnostics.is_empty(),
        "{:?}",
        laid.frame.diagnostics
    );
    let caption = lab.caption_rect().unwrap();
    assert_eq!((caption.x, caption.y), (0.0, 0.0));
    assert_eq!((caption.w, caption.h), (SIZE.0, theme::CAPTION_H));
}

#[test]
fn the_caption_drags_everywhere_but_on_its_controls() {
    let (lab, _) = drawn(SIZE);
    assert!(lab.is_caption(200.0, theme::CAPTION_H * 0.5));
    for action in [
        caption::Action::Minimise,
        caption::Action::ToggleMaximise,
        caption::Action::Close,
    ] {
        let (x, y) = control(&lab, action);
        assert!(!lab.is_caption(x, y), "{action:?} is pressed, not dragged");
    }
    assert!(!lab.is_caption(200.0, theme::CAPTION_H + 40.0));
}

#[test]
fn closing_ends_the_process_and_nothing_else_does() {
    let (mut lab, mut engine) = drawn(SIZE);
    assert!(!lab.should_exit());
    // A press in the body, and a key, are not a close.
    assert!(!lab.click(&mut engine, 300.0, 300.0, false));
    assert!(!lab.should_exit());

    let (x, y) = control(&lab, caption::Action::Close);
    assert!(lab.click(&mut engine, x, y, false));
    assert!(lab.should_exit());
    assert_eq!(lab.take_window_request(), None);

    // The system's close — Alt+F4, the taskbar — is the same answer.
    let (mut lab, _) = drawn(SIZE);
    lab.close_requested();
    assert!(lab.should_exit());
}

#[test]
fn minimise_and_maximise_are_asked_of_the_runner_once() {
    let (mut lab, mut engine) = drawn(SIZE);
    let (x, y) = control(&lab, caption::Action::Minimise);
    assert!(lab.click(&mut engine, x, y, false));
    assert_eq!(lab.take_window_request(), Some(WindowRequest::Minimise));
    assert_eq!(lab.take_window_request(), None);

    let (x, y) = control(&lab, caption::Action::ToggleMaximise);
    assert!(lab.click(&mut engine, x, y, false));
    assert_eq!(
        lab.take_window_request(),
        Some(WindowRequest::ToggleMaximise)
    );
    assert!(!lab.should_exit());

    assert!(lab.set_maximised(true));
    assert!(!lab.set_maximised(true), "an unchanged state is not news");
}

#[test]
fn hover_asks_for_a_frame_only_when_it_changes() {
    let (mut lab, _) = drawn(SIZE);
    let (x, y) = control(&lab, caption::Action::Close);
    assert!(lab.pointer_moved(x, y));
    assert!(!lab.pointer_moved(x + 1.0, y));
    assert!(lab.pointer_moved(300.0, 300.0));
    assert!(!lab.pointer_moved(310.0, 300.0));
    assert!(lab.pointer_moved(x, y));
    assert!(lab.pointer_left());
    assert!(!lab.pointer_left());
}

/// A resize drops the frame it invalidated, and the next draw puts the
/// controls at the new edge: hit testing never answers from a stale layout.
#[test]
fn a_resize_is_answered_from_the_new_frame() {
    let (mut lab, mut engine) = drawn(SIZE);
    let (old_x, y) = control(&lab, caption::Action::Close);
    lab.resized(PhysicalSize::new(800, 600), 1.0);
    assert_eq!(lab.caption_rect(), None);
    assert!(!lab.is_caption(200.0, y));
    assert!(!lab.click(&mut engine, old_x, y, false));
    assert!(!lab.should_exit());

    draw(&mut lab, &mut engine, (800.0, 600.0));
    let (new_x, _) = control(&lab, caption::Action::Close);
    assert!(new_x < old_x);
    assert_eq!(lab.caption_rect().unwrap().w, 800.0);
    assert!(lab.laid.as_ref().unwrap().frame.diagnostics.is_empty());
}

#[test]
fn the_window_is_titled_in_the_display_language() {
    let lab = Lab::new(Placement::in_memory(None));
    let mut titles = Vec::new();
    for lang in koru_i18n::Lang::ALL {
        let title = koru_i18n::with_current(lang, || lab.window().title);
        assert_eq!(title, koru_i18n::t(lang, koru_i18n::Key::UiLabTitle));
        assert!(title.contains("Koru"), "{lang:?}: {title}");
        titles.push(title);
    }
    titles.sort_unstable();
    titles.dedup();
    assert!(titles.len() > 1, "the title is translated");
    assert!(!lab.window().decorations);
}
