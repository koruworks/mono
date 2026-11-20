// SPDX-License-Identifier: MPL-2.0

use koru_draw::el::Name;

use super::*;

const LANG: Lang = Lang::En;

fn area() -> Rect {
    Rect::new(300.0, 40.0, 900.0, 660.0)
}

fn rect(view: &EmptyView, area: Rect, key: Key) -> Rect {
    view.with_frame(area, LANG, |frame| frame.node(key).expect("drawn").rect)
}

/// The empty view is the heading, paragraph, New bot and examples, in
/// that order, no wider than the design's measure, in the middle of the
/// content area; every part an assistive technology hears is named.
#[test]
fn the_view_is_the_pitch_and_one_button_in_the_middle() {
    let view = EmptyView::default();
    let area = area();
    view.with_frame(area, LANG, |frame| {
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        let roots = frame.semantics();
        assert_eq!(roots.len(), 1);
        let said: Vec<Name> = roots[0]
            .children
            .iter()
            .filter_map(|node| node.name.clone())
            .collect();
        assert_eq!(
            said,
            [
                Name::Key(Words::BotsIntroTitle),
                Name::Key(Words::BotsIntroBody),
                Name::Key(Words::BotsNewBot),
                Name::Key(Words::BotsIntroExamples),
            ]
        );
        let button = &roots[0].children[2];
        assert_eq!(button.role, Role::Button);
        assert_eq!(button.actions, [Action::Click, Action::Focus]);
        assert_eq!(roots[0].children[0].role, Role::Heading);
        assert_eq!(
            frame.hits().iter().map(|hit| hit.key).collect::<Vec<_>>(),
            [NEW_BOT],
            "one thing to press"
        );
    });
    let (title, body, button, examples) = (
        rect(&view, area, TITLE),
        rect(&view, area, BODY),
        rect(&view, area, NEW_BOT),
        rect(&view, area, EXAMPLES),
    );
    assert!(title.y < body.y && body.y < button.y && button.y < examples.y);
    let faces = view.with_frame(area, LANG, |frame| {
        frame.rect_of("Bots empty faces").expect("the faces")
    });
    assert!(faces.y + faces.h <= title.y, "the faces come first");
    assert_eq!(faces.h, faces::INTRO_FACE.value);
    assert_eq!(
        faces.w,
        faces::INTRO_FACE.value * 3.0 - faces::INTRO_OVERLAP.value * 2.0,
        "three, each over the one before"
    );
    // No wider than the measure, and centred both ways.
    assert!(body.w <= CONTENT_W.value + 0.5, "{body:?}");
    let middle = area.x + area.w * 0.5;
    assert!((body.x + body.w * 0.5 - middle).abs() < 1.0, "{body:?}");
    let (top, bottom) = (
        faces.y - area.y,
        area.y + area.h - (examples.y + examples.h),
    );
    assert!((top - bottom).abs() < 1.5, "above {top}, below {bottom}");
}

/// A narrow content area wraps the words instead of cutting them: nothing
/// leaves the area.
#[test]
fn a_narrow_area_wraps_the_words() {
    let view = EmptyView::default();
    let narrow = Rect::new(0.0, 0.0, 260.0, 700.0);
    let wide_body = rect(&view, area(), BODY);
    for key in [TITLE, BODY, NEW_BOT, EXAMPLES] {
        let at = rect(&view, narrow, key);
        assert!(
            at.x >= narrow.x && at.x + at.w <= narrow.x + narrow.w + 0.5,
            "{key:?}: {at:?}"
        );
    }
    assert!(rect(&view, narrow, BODY).h > wide_body.h, "more lines");
}

/// **The pointer and the keyboard both reach *New bot***: a press on it,
/// or Tab then Enter or Space. A press anywhere else in the area is taken
/// and does nothing; a chord is not the view's.
#[test]
fn new_bot_is_pressed_by_the_pointer_and_by_the_keyboard() {
    let mut view = EmptyView::default();
    let area = area();
    let button = rect(&view, area, NEW_BOT);
    let (x, y) = (button.x + button.w * 0.5, button.y + button.h * 0.5);
    assert!(view.mouse_move(area, LANG, x, y), "lit under the pointer");
    assert!(view.hot());
    assert_eq!(view.mouse_down(area, LANG, x, y), Outcome::NewBot);
    assert_eq!(
        view.mouse_down(area, LANG, area.x + 4.0, area.y + 4.0),
        Outcome::Consumed,
        "the area is the view's, so the tab under it is never pressed"
    );
    assert!(view.mouse_move(area, LANG, area.x + 4.0, area.y + 4.0));
    assert!(!view.hot());

    let none = ModifiersState::empty();
    assert!(!view.focused(), "no ring until the keyboard is used");
    assert_eq!(
        view.key(&WinitKey::Named(NamedKey::Tab), none),
        Outcome::Consumed
    );
    assert!(view.focused());
    assert_eq!(
        view.key(&WinitKey::Named(NamedKey::Enter), none),
        Outcome::NewBot
    );
    assert_eq!(
        view.key(&WinitKey::Named(NamedKey::Space), none),
        Outcome::NewBot
    );
    assert_eq!(
        view.key(&WinitKey::Character("x".into()), none),
        Outcome::Ignored
    );
    assert_eq!(
        view.key(&WinitKey::Named(NamedKey::Enter), ModifiersState::CONTROL),
        Outcome::Ignored,
        "a chord is the window's"
    );
    // A press takes the ring away again; the dialog closing puts it back.
    view.mouse_down(area, LANG, area.x + 4.0, area.y + 4.0);
    assert!(!view.focused());
    assert!(view.focus());
    assert!(view.focused());
    assert!(view.blur());
    assert!(!view.focused());
}

/// The words are the design's, except where the design says something a
/// Bot does not do.
#[test]
fn the_words_say_only_what_a_bot_does() {
    assert_eq!(t(LANG, Words::BotsNoneYet), "No bots yet");
    assert_eq!(
        t(LANG, Words::BotsIntroTitle),
        "A bot is a chat that works on its own"
    );
    let body = t(LANG, Words::BotsIntroBody);
    assert!(
        body.contains("on this computer while Koru is open"),
        "{body}"
    );
    assert!(!body.to_lowercase().contains("cloud"), "{body}");
    assert!(
        body.ends_with("What it makes lands in your workspace."),
        "{body}"
    );
}
