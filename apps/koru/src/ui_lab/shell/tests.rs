// SPDX-License-Identifier: MPL-2.0

//! The window driven the way the runner drives it — a draw, then pointer and
//! key events against that frame — with a real text engine and a scene, and
//! no window, GPU or event loop.
//!
//! The display language is set per thread (`koru_i18n::with_current`), never
//! on the process, so these run beside every other test in the binary.

use koru_ui::button::ButtonRole;
use koru_window::KeyState;
use winit::keyboard::{KeyCode, KeyLocation, PhysicalKey};

use super::*;

pub(super) const SIZE: (f32, f32) = (1120.0, 720.0);
/// The smallest window `--window-size` accepts.
pub(super) const SMALLEST: (f32, f32) = koru_workspace_host::settings::MIN_WINDOW_SIZE;

fn button_entry() -> &'static Entry {
    registry::story(registry::BUTTON_DIALOG_STORY)
        .expect("the trial story is registered")
        .0
}

/// Run `body` in English, on this thread only.
pub(super) fn english<T>(body: impl FnOnce() -> T) -> T {
    koru_i18n::with_current(koru_i18n::Lang::En, body)
}

/// A Lab that has drawn one frame at `size`.
pub(super) fn drawn(size: (f32, f32)) -> (Lab, TextEngine) {
    let mut engine = TextEngine::new();
    let mut lab = Lab::new(
        &mut engine,
        Placement::in_memory(Some(size)),
        button_entry(),
    );
    draw(&mut lab, &mut engine, size);
    (lab, engine)
}

pub(super) fn draw(lab: &mut Lab, engine: &mut TextEngine, size: (f32, f32)) {
    let mut scene = Scene::new();
    lab.draw(&mut scene, engine, size.0, size.1);
}

pub(super) fn frame(lab: &Lab) -> &Frame {
    &lab.laid.as_ref().expect("the Lab has drawn").frame
}

/// The story on show.
pub(super) fn story(lab: &Lab) -> &ButtonStory {
    lab.story.as_ref().expect("the record on show has a story")
}

/// The label property's field.
pub(super) fn label(lab: &Lab) -> &TextInput {
    lab.field(field()).expect("the story has a label field")
}

/// A record of the gallery, by its catalog ID.
pub(super) fn record(catalog_id: &str) -> &'static Entry {
    registry::desktop()
        .find(|entry| entry.catalog_id == catalog_id)
        .unwrap_or_else(|| panic!("the gallery has no `{catalog_id}`"))
}

pub(super) fn entry_key(catalog_id: &str) -> Key {
    Row::Entry(record(catalog_id)).key()
}

pub(super) fn group_key(family: Family) -> Key {
    Row::Group(family).key()
}

/// A record the Lab lists and cannot run: the corner radius table waits for
/// its story.
pub(super) const NO_STORY: &str = "T-RADIUS";

pub(super) fn field() -> Key {
    control_key(Control::Label)
}

pub(super) fn style() -> Key {
    control_key(Control::Style)
}

pub(super) fn centre(lab: &Lab, key: Key) -> (f32, f32) {
    lab.rect(key).expect("the frame shows the control").centre()
}

/// Press the control and draw the frame the press asked for.
pub(super) fn press(lab: &mut Lab, engine: &mut TextEngine, key: Key) -> bool {
    let (x, y) = centre(lab, key);
    let changed = lab.click(engine, x, y, false);
    lab.mouse_up();
    draw(lab, engine, SIZE);
    changed
}

pub(super) fn named(
    lab: &mut Lab,
    engine: &mut TextEngine,
    key: NamedKey,
    mods: ModifiersState,
) -> bool {
    let changed = lab.key(engine, &KeyboardKey::Named(key), None, mods);
    draw(lab, engine, SIZE);
    changed
}

pub(super) fn tab(lab: &mut Lab, engine: &mut TextEngine) -> bool {
    named(lab, engine, NamedKey::Tab, ModifiersState::empty())
}

pub(super) fn back_tab(lab: &mut Lab, engine: &mut TextEngine) -> bool {
    named(lab, engine, NamedKey::Tab, ModifiersState::SHIFT)
}

pub(super) fn type_text(lab: &mut Lab, engine: &mut TextEngine, typed: &str) {
    for character in typed.chars() {
        let text = character.to_string();
        let key = if character == ' ' {
            KeyboardKey::Named(NamedKey::Space)
        } else {
            KeyboardKey::Character(text.as_str().into())
        };
        assert!(lab.key(engine, &key, Some(&text), ModifiersState::empty()));
    }
    draw(lab, engine, SIZE);
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

pub(super) fn assert_sound(lab: &Lab, context: &str) {
    let frame = frame(lab);
    assert!(
        frame.diagnostics.is_empty(),
        "{context}: {:?}",
        frame.diagnostics
    );
    let overflowing: Vec<&str> = frame
        .nodes
        .iter()
        .filter(|node| node.overflow)
        .map(|node| node.name)
        .collect();
    assert!(
        overflowing.is_empty(),
        "{context}: {overflowing:?} overflow"
    );
}

// ------------------------------------------------------------ the surface

/// Navigator, preview and properties, left to right under the toolbar, with
/// every element inside the window and nothing overflowing — in each of the
/// eight languages, at the default size and at the smallest one accepted.
#[test]
fn the_three_regions_compose_in_every_language_and_at_the_smallest_size() {
    for lang in koru_i18n::Lang::ALL {
        koru_i18n::with_current(lang, || {
            for size in [SIZE, SMALLEST] {
                let (lab, _) = drawn(size);
                let context = format!("{lang:?} at {size:?}");
                assert_sound(&lab, &context);
                let frame = frame(&lab);
                let navigator = frame.rect_of("Navigator").unwrap();
                let preview = frame.rect_of("Preview").unwrap();
                let properties = frame.rect_of("Properties").unwrap();
                let toolbar = frame.rect_of("Toolbar").unwrap();
                assert_eq!(navigator.x, 0.0, "{context}");
                assert!(navigator.x + navigator.w <= preview.x, "{context}");
                assert!(preview.x + preview.w <= properties.x, "{context}");
                assert_eq!(properties.x + properties.w, size.0, "{context}");
                assert!(preview.w >= button::MIN_W, "{context}: {preview:?}");
                for region in [navigator, preview, properties] {
                    assert!(region.y >= toolbar.y + toolbar.h, "{context}");
                    assert_eq!(region.y + region.h, size.1, "{context}");
                }
                // The style control, in the language that spells it longest,
                // still fits the properties column.
                let choice = lab.rect(style()).unwrap();
                assert!(
                    choice.x + choice.w <= properties.x + properties.w,
                    "{context}: {choice:?} leaves {properties:?}"
                );
                // The component under test stands on its stage, in the
                // preview column.
                let subject = lab.rect(SUBJECT).unwrap();
                let stage = lab.rect(STAGE).unwrap();
                assert!(stage.x >= preview.x && stage.x + stage.w <= preview.x + preview.w);
                assert!(subject.x >= stage.x && subject.x + subject.w <= stage.x + stage.w);
                assert_eq!(subject.h, button::H, "{context}");
                // And the line under it wraps rather than leaving the column.
                let result = frame.rect_of("Click result").unwrap();
                assert!(
                    result.x >= preview.x && result.x + result.w <= preview.x + preview.w,
                    "{context}: {result:?} leaves {preview:?}"
                );
            }
        });
    }
}

/// The Lab opens on the record it was given, with its row in view and
/// marked as the one on show.
#[test]
fn the_lab_opens_with_the_record_on_show_in_view() {
    english(|| {
        for size in [SIZE, SMALLEST] {
            let (lab, _) = drawn(size);
            let frame = frame(&lab);
            let row = frame
                .node(entry_key(registry::BUTTON))
                .expect("the record on show is a row of the first frame");
            assert_eq!(row.selected, Some(true));
            assert_eq!(
                row.label,
                Some(el::Name::Key(koru_i18n::Key::UiLabButtonName))
            );
            let list = frame.node(LIST).unwrap().viewport.unwrap();
            assert!(
                row.rect.y >= list.y && row.rect.y + row.rect.h <= list.y + list.h,
                "{size:?}: {:?} is outside {list:?}",
                row.rect
            );
            let selected = frame
                .named("Navigator entry")
                .filter(|node| node.selected == Some(true))
                .count();
            assert_eq!(selected, 1);
        }
    });
}

/// The keyboard's stops are the toolbar's settings and Reset, the search,
/// one row of the navigator, the page, and the story's own controls. Nothing stands in for a control
/// the Lab does not have yet.
#[test]
fn the_keyboard_stops_are_the_controls_that_exist() {
    english(|| {
        let (lab, _) = drawn(SIZE);
        assert_eq!(
            lab.laid.as_ref().unwrap().stops(),
            [
                Setting::Theme.key(),
                Setting::Width.key(),
                Setting::Language.key(),
                RESET,
                SEARCH,
                entry_key(registry::BUTTON),
                PAGE,
                SUBJECT,
                field(),
                style()
            ]
        );
        // Every pointer target can also be reached by the keyboard: it is a
        // stop, a row of the navigator's one stop, or a scrollbar.
        for hit in frame(&lab).hits() {
            assert!(
                lab.takes_focus(hit.key) || VIEWPORTS.contains(&hit.key),
                "{:?} can be pressed and not reached",
                hit.key
            );
        }
    });
}

// -------------------------------------------------------------- the story

#[test]
fn pressing_the_button_is_counted_and_shown() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        assert_eq!(lab.result_line(), "Not pressed yet");
        assert!(press(&mut lab, &mut engine, SUBJECT));
        assert_eq!(story(&lab).presses(), 1);
        assert_eq!(lab.result_line(), "Presses: 1");
        assert_eq!(lab.focus, Some(SUBJECT), "a press takes the focus");

        // Two quick presses are two presses: the runner reports the second as
        // a double click, and it still counts.
        let (x, y) = centre(&lab, SUBJECT);
        assert!(lab.click(&mut engine, x, y, false));
        assert!(lab.double_click(&mut engine, x, y));
        assert_eq!(story(&lab).presses(), 3);
        assert_eq!(lab.result_line(), "Presses: 3");

        // A press beside it, on the stage, is not a press of it.
        let stage = lab.rect(STAGE).unwrap();
        assert!(lab.click(&mut engine, stage.x + 2.0, stage.y + 2.0, false));
        assert_eq!(story(&lab).presses(), 3);
        assert_eq!(lab.focus, None, "a press on nothing gives the focus up");
    });
}

#[test]
fn the_result_line_counts_in_the_display_languages_own_digits() {
    let (mut lab, mut engine) = english(|| drawn(SIZE));
    english(|| press(&mut lab, &mut engine, SUBJECT));
    for lang in koru_i18n::Lang::ALL {
        koru_i18n::with_current(lang, || {
            let line = lab.result_line();
            assert!(
                line.contains(&koru_i18n::number::decimal(lang, 1)),
                "{lang:?}: {line}"
            );
            assert!(!line.contains("{}"), "{lang:?}: {line}");
        });
    }
}

#[test]
fn typing_in_the_label_field_relabels_the_button() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        let before = lab.rect(SUBJECT).unwrap();
        assert_eq!(story(&lab).label(), "Save changes");

        assert!(press(&mut lab, &mut engine, field()));
        assert_eq!(lab.focus, Some(field()));
        // Select what is there, then type over it.
        assert!(lab.key(
            &mut engine,
            &KeyboardKey::Character("a".into()),
            None,
            ModifiersState::CONTROL,
        ));
        type_text(&mut lab, &mut engine, "Move 12 files to the shared archive");
        assert_eq!(story(&lab).label(), "Move 12 files to the shared archive");
        assert_eq!(label(&lab).raw_text(), story(&lab).label());
        assert_eq!(story(&lab).presses(), 0, "typing a space is not a press");

        // The button is measured from its new label, and is named by it.
        let after = lab.rect(SUBJECT).unwrap();
        assert!(after.w > before.w, "{after:?} is no wider than {before:?}");
        assert_eq!(
            frame(&lab).node(SUBJECT).unwrap().label,
            Some(el::Name::Text(story(&lab).label().to_owned()))
        );
        assert_sound(&lab, "a relabelled button");

        // Backspace edits it; an empty label is a real state, shown as one.
        for _ in 0..story(&lab).label().chars().count() {
            lab.key(
                &mut engine,
                &KeyboardKey::Named(NamedKey::Backspace),
                None,
                ModifiersState::empty(),
            );
        }
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(story(&lab).label(), "");
        assert_eq!(lab.rect(SUBJECT).unwrap().w, button::MIN_W);
        assert_sound(&lab, "an unlabelled button");
    });
}

/// A label longer than the column keeps the button inside its stage and the
/// stage inside the preview: the neighbours are never drawn over.
#[test]
fn a_very_long_label_stays_inside_the_preview() {
    english(|| {
        for size in [SIZE, SMALLEST] {
            let (mut lab, mut engine) = drawn(size);
            let long = "A label that keeps going ".repeat(40);
            assert!(lab.set_field(&mut engine, field(), &long));
            draw(&mut lab, &mut engine, size);
            assert_sound(&lab, "a very long label");
            let preview = frame(&lab).rect_of("Preview").unwrap();
            let stage = lab.rect(STAGE).unwrap();
            let subject = lab.rect(SUBJECT).unwrap();
            assert!(stage.x >= preview.x && stage.x + stage.w <= preview.x + preview.w);
            assert!(subject.x >= stage.x && subject.x + subject.w <= stage.x + stage.w);
            assert!(subject.w >= button::MIN_W);
            assert_eq!(story(&lab).label(), long, "the label itself is not cut");
        }
    });
}

#[test]
fn choosing_a_segment_restyles_the_button() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        assert_eq!(story(&lab).style(), ButtonRole::Secondary);
        let segments = lab.laid.as_ref().unwrap().segments.clone().unwrap();
        assert_eq!(segments.segments.len(), registry::DIALOG_STYLES.len());
        // The segments are drawn inside the control that is pressed for them.
        let choice = lab.rect(style()).unwrap();
        assert!(segments.bounds.x >= choice.x && segments.bounds.y >= choice.y);

        for (slot, expected) in registry::DIALOG_STYLES.into_iter().enumerate().rev() {
            let (x, y) = segments.segments[slot].centre();
            assert!(lab.pointer_moved(x, y) || lab.style_hover == Some(slot));
            assert_eq!(lab.style_hover, Some(slot));
            lab.click(&mut engine, x, y, false);
            draw(&mut lab, &mut engine, SIZE);
            assert_eq!(story(&lab).style(), expected);
            assert_eq!(lab.focus, Some(style()));
        }
        assert_eq!(story(&lab).presses(), 0, "choosing a style is not a press");
        // Leaving the control clears the segment hover.
        assert!(lab.pointer_moved(5.0, SIZE.1 - 5.0));
        assert_eq!(lab.style_hover, None);
    });
}

#[test]
fn reset_restores_the_label_the_style_and_the_click_result() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        // Nothing to restore yet.
        assert!(!lab.activate(&mut engine, RESET));

        press(&mut lab, &mut engine, SUBJECT);
        press(&mut lab, &mut engine, field());
        type_text(&mut lab, &mut engine, "!!");
        let (x, y) = lab
            .laid
            .as_ref()
            .unwrap()
            .segments
            .as_ref()
            .unwrap()
            .segments[2]
            .centre();
        lab.click(&mut engine, x, y, false);
        draw(&mut lab, &mut engine, SIZE);
        assert_ne!(story(&lab).label(), "Save changes");
        assert_eq!(story(&lab).style(), ButtonRole::Destructive);
        assert_eq!(story(&lab).presses(), 1);

        assert!(press(&mut lab, &mut engine, RESET));
        assert!(story(&lab).is_pristine());
        assert_eq!(story(&lab).label(), "Save changes");
        assert_eq!(label(&lab).raw_text(), "Save changes", "the field follows");
        assert_eq!(story(&lab).style(), ButtonRole::Secondary);
        assert_eq!(lab.result_line(), "Not pressed yet");
        assert_eq!(lab.focus, Some(RESET));
        // What Reset leaves alone: the entry on show.
        assert_eq!(lab.entry.catalog_id, registry::BUTTON);
        assert_sound(&lab, "after a reset");
    });
}

// ------------------------------------------------------------ the keyboard

#[test]
fn tab_walks_the_controls_in_reading_order_and_wraps() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        let stops = lab.laid.as_ref().unwrap().stops();
        assert_eq!(lab.focus, None);
        for stop in &stops {
            assert!(tab(&mut lab, &mut engine));
            assert_eq!(lab.focus, Some(*stop));
            // The focused control is the one the frame draws as focused.
            let focused: Vec<Key> = frame(&lab)
                .nodes
                .iter()
                .filter(|node| node.state.focused)
                .filter_map(|node| node.key)
                .collect();
            assert_eq!(focused, [*stop]);
        }
        assert!(tab(&mut lab, &mut engine));
        assert_eq!(lab.focus, Some(stops[0]), "past the last is the first");
        assert!(back_tab(&mut lab, &mut engine));
        assert_eq!(lab.focus, stops.last().copied(), "and back again");
        for stop in stops.iter().rev().skip(1) {
            assert!(back_tab(&mut lab, &mut engine));
            assert_eq!(lab.focus, Some(*stop));
        }

        // Ctrl+I arrives as Tab on Windows, and Ctrl+Tab belongs to nobody
        // here: neither moves the focus.
        let before = lab.focus;
        assert!(!named(
            &mut lab,
            &mut engine,
            NamedKey::Tab,
            ModifiersState::CONTROL
        ));
        assert_eq!(lab.focus, before);
        // Escape gives the focus up.
        assert!(named(
            &mut lab,
            &mut engine,
            NamedKey::Escape,
            ModifiersState::empty()
        ));
        assert_eq!(lab.focus, None);
    });
}

/// The whole trial without the pointer: press the button, relabel it, restyle
/// it, and reset.
#[test]
fn the_trial_can_be_done_from_the_keyboard() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        let none = ModifiersState::empty();
        while lab.focus != Some(SUBJECT) {
            assert!(tab(&mut lab, &mut engine));
        }
        assert!(named(&mut lab, &mut engine, NamedKey::Enter, none));
        assert!(named(&mut lab, &mut engine, NamedKey::Space, none));
        assert_eq!(story(&lab).presses(), 2);

        // Arriving at the field by keyboard selects its text, so typing
        // replaces it.
        assert!(tab(&mut lab, &mut engine));
        assert_eq!(lab.focus, Some(field()));
        type_text(&mut lab, &mut engine, "Send now");
        assert_eq!(story(&lab).label(), "Send now");
        // Enter in the field is not a press of anything.
        assert!(!named(&mut lab, &mut engine, NamedKey::Enter, none));
        assert_eq!(story(&lab).presses(), 2);

        assert!(tab(&mut lab, &mut engine));
        assert_eq!(lab.focus, Some(style()));
        assert!(named(&mut lab, &mut engine, NamedKey::ArrowRight, none));
        assert_eq!(story(&lab).style(), ButtonRole::Primary);
        assert!(named(&mut lab, &mut engine, NamedKey::ArrowRight, none));
        assert_eq!(story(&lab).style(), ButtonRole::Destructive);
        assert!(!named(&mut lab, &mut engine, NamedKey::ArrowRight, none));
        assert!(named(&mut lab, &mut engine, NamedKey::ArrowLeft, none));
        assert_eq!(story(&lab).style(), ButtonRole::Primary);
        // Enter and Space do nothing on a choice.
        assert!(!named(&mut lab, &mut engine, NamedKey::Enter, none));

        while lab.focus != Some(RESET) {
            assert!(back_tab(&mut lab, &mut engine));
        }
        assert!(named(&mut lab, &mut engine, NamedKey::Enter, none));
        assert!(story(&lab).is_pristine());
        assert_eq!(label(&lab).raw_text(), "Save changes");
        assert_sound(&lab, "after a keyboard reset");
    });
}

/// A held key repeats. Typing repeats with it; pressing a button does not,
/// and a release is never a press.
#[test]
fn a_held_activation_key_presses_once() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        press(&mut lab, &mut engine, SUBJECT);
        let enter = KeyboardKey::Named(NamedKey::Enter);
        let event = |state, repeat| RichKeyEvent {
            logical_key: &enter,
            physical_key: PhysicalKey::Code(KeyCode::Enter),
            location: KeyLocation::Standard,
            text: None,
            state,
            repeat,
            modifiers: ModifiersState::empty(),
        };
        assert!(lab.rich_key(&mut engine, event(KeyState::Pressed, false)));
        assert_eq!(story(&lab).presses(), 2);
        for _ in 0..5 {
            assert!(!lab.rich_key(&mut engine, event(KeyState::Pressed, true)));
        }
        assert!(!lab.rich_key(&mut engine, event(KeyState::Released, false)));
        assert_eq!(story(&lab).presses(), 2);
    });
}

#[test]
fn a_key_with_nothing_focused_does_nothing() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        let before = lab.story.clone();
        for key in [NamedKey::Enter, NamedKey::Space, NamedKey::ArrowRight] {
            assert!(!named(&mut lab, &mut engine, key, ModifiersState::empty()));
        }
        assert!(!lab.key(
            &mut engine,
            &KeyboardKey::Character("x".into()),
            Some("x"),
            ModifiersState::empty(),
        ));
        assert_eq!(lab.story, before);
        assert_eq!(label(&lab).raw_text(), "Save changes");
    });
}

// ------------------------------------------------------- composed text

/// Composed text goes to the label field while it has the keyboard, and
/// becomes the label when it is committed — not while it is still a
/// composition.
#[test]
fn composed_text_reaches_the_label_only_when_committed() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        assert!(lab.wants_ime());
        // Nowhere to put it yet.
        assert!(!lab.ime(&mut engine, ImeEvent::Commit { text: "保存" }));
        assert_eq!(story(&lab).label(), "Save changes");

        while lab.focus != Some(field()) {
            tab(&mut lab, &mut engine);
        }
        lab.ime(&mut engine, ImeEvent::Enabled);
        assert!(lab.ime(
            &mut engine,
            ImeEvent::Preedit {
                text: "ほぞん",
                cursor: Some((9, 9)),
            },
        ));
        assert!(label(&lab).is_composing());
        assert_eq!(story(&lab).label(), "Save changes", "not the label yet");

        assert!(lab.ime(&mut engine, ImeEvent::Commit { text: "保存" }));
        assert!(!label(&lab).is_composing());
        assert_eq!(story(&lab).label(), "保存");
        assert_eq!(label(&lab).raw_text(), "保存");
        draw(&mut lab, &mut engine, SIZE);
        assert_sound(&lab, "a composed label");

        // A composition abandoned by moving on leaves the label as it was.
        lab.ime(
            &mut engine,
            ImeEvent::Preedit {
                text: "と",
                cursor: None,
            },
        );
        assert!(tab(&mut lab, &mut engine));
        assert!(!label(&lab).is_composing());
        assert_eq!(story(&lab).label(), "保存");
    });
}

// --------------------------------------------------------------- the window

#[test]
fn the_caption_drags_everywhere_but_on_its_controls() {
    let (lab, _) = english(|| drawn(SIZE));
    let caption = lab.caption_rect().unwrap();
    assert_eq!((caption.x, caption.y), (0.0, 0.0));
    assert_eq!((caption.w, caption.h), (SIZE.0, theme::CAPTION_H));
    assert!(lab.is_caption(200.0, theme::CAPTION_H * 0.5));
    for action in [
        caption::Action::Minimise,
        caption::Action::ToggleMaximise,
        caption::Action::Close,
    ] {
        let (x, y) = control(&lab, action);
        assert!(!lab.is_caption(x, y), "{action:?} is pressed, not dragged");
    }
    let (x, y) = centre(&lab, SUBJECT);
    assert!(!lab.is_caption(x, y));
}

#[test]
fn closing_ends_the_process_and_nothing_else_does() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        assert!(!lab.should_exit());
        // Using the Lab is not closing it.
        press(&mut lab, &mut engine, SUBJECT);
        press(&mut lab, &mut engine, RESET);
        tab(&mut lab, &mut engine);
        assert!(!lab.should_exit());

        let (x, y) = control(&lab, caption::Action::Close);
        assert!(lab.click(&mut engine, x, y, false));
        assert!(lab.should_exit());
        assert_eq!(lab.take_window_request(), None);

        // The system's close — Alt+F4, the taskbar — is the same answer.
        let (mut lab, _) = drawn(SIZE);
        lab.close_requested();
        assert!(lab.should_exit());
    });
}

#[test]
fn minimise_and_maximise_are_asked_of_the_runner_once() {
    english(|| {
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
    });
}

#[test]
fn hover_asks_for_a_frame_only_when_it_changes() {
    english(|| {
        let (mut lab, _) = drawn(SIZE);
        let (x, y) = control(&lab, caption::Action::Close);
        assert!(lab.pointer_moved(x, y));
        assert!(!lab.pointer_moved(x + 1.0, y));
        let (bx, by) = centre(&lab, SUBJECT);
        assert!(lab.pointer_moved(bx, by));
        assert_eq!(lab.hot, Some(SUBJECT));
        assert!(!lab.pointer_moved(bx + 1.0, by));
        // With the button held the runner reports motion as a drag; hover
        // still follows.
        let (rx, ry) = centre(&lab, RESET);
        let mut engine = TextEngine::new();
        assert!(lab.drag(&mut engine, rx, ry));
        assert_eq!(lab.hot, Some(RESET));
        assert!(lab.pointer_left());
        assert!(!lab.pointer_left());
        assert_eq!(lab.hot, None);
    });
}

/// The I-beam over the one place a caret can go; the plain arrow everywhere
/// else, controls included.
#[test]
fn the_cursor_is_an_i_beam_only_over_the_field() {
    let (lab, _) = english(|| drawn(SIZE));
    let (x, y) = centre(&lab, field());
    assert_eq!(lab.cursor_icon(x, y), CursorIcon::Text);
    for key in [SUBJECT, RESET, style()] {
        let (x, y) = centre(&lab, key);
        assert_eq!(lab.cursor_icon(x, y), CursorIcon::Default);
    }
}

/// A resize drops the frame it invalidated, and the next draw lays the window
/// out at its new size: hit testing never answers from a stale layout.
#[test]
fn a_resize_is_answered_from_the_new_frame() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        let (old_x, y) = control(&lab, caption::Action::Close);
        let (bx, by) = centre(&lab, SUBJECT);
        lab.pointer_moved(bx, by);
        lab.resized(PhysicalSize::new(800, 600), 1.0);
        assert_eq!(lab.caption_rect(), None);
        assert_eq!(lab.hot, None);
        assert!(!lab.is_caption(200.0, y));
        assert!(!lab.click(&mut engine, old_x, y, false));
        assert!(!lab.click(&mut engine, bx, by, false));
        assert!(!lab.should_exit());
        assert_eq!(story(&lab).presses(), 0);

        draw(&mut lab, &mut engine, (800.0, 600.0));
        let (new_x, _) = control(&lab, caption::Action::Close);
        assert!(new_x < old_x);
        assert_eq!(lab.caption_rect().unwrap().w, 800.0);
        assert_sound(&lab, "after a resize");
        // And the story the resize left alone still answers there.
        let (x, y) = centre(&lab, SUBJECT);
        assert!(lab.click(&mut engine, x, y, false));
        assert_eq!(story(&lab).presses(), 1);
    });
}

/// A window that is not the one being typed into shows no focus ring and no
/// caret, and gets them back with the keyboard.
#[test]
fn focus_is_drawn_only_while_the_window_has_the_keyboard() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        press(&mut lab, &mut engine, field());
        let focused = |lab: &Lab| frame(lab).node(field()).unwrap().state.focused;
        assert!(focused(&lab));

        assert!(lab.focus_changed(false));
        assert!(!lab.focus_changed(false));
        draw(&mut lab, &mut engine, SIZE);
        assert!(!focused(&lab));
        assert_eq!(lab.focus, Some(field()), "the focus itself is kept");

        assert!(lab.focus_changed(true));
        draw(&mut lab, &mut engine, SIZE);
        assert!(focused(&lab));
    });
}

#[test]
fn the_window_is_titled_in_the_display_language() {
    let mut engine = TextEngine::new();
    let mut titles = Vec::new();
    let mut lab = english(|| Lab::new(&mut engine, Placement::in_memory(None), button_entry()));
    for lang in koru_i18n::Lang::ALL {
        // The window is titled in the language the Lab opened in.
        lab = koru_i18n::with_current(lang, || {
            Lab::new(&mut engine, Placement::in_memory(None), button_entry())
        });
        let title = lab.window().title;
        assert_eq!(title, koru_i18n::t(lang, koru_i18n::Key::UiLabTitle));
        assert!(title.contains("Koru"), "{lang:?}: {title}");
        titles.push(title);
    }
    titles.sort_unstable();
    titles.dedup();
    assert!(titles.len() > 1, "the title is translated");
    assert!(!lab.window().decorations);
}

/// The sample label is made-up content in the person's language, fixed when
/// the Lab opens: it is what Reset goes back to.
#[test]
fn the_sample_label_is_in_the_language_the_lab_opened_in() {
    for lang in koru_i18n::Lang::ALL {
        koru_i18n::with_current(lang, || {
            let (lab, _) = drawn(SIZE);
            assert_eq!(
                story(&lab).label(),
                koru_i18n::t(lang, koru_i18n::Key::UiLabButtonSampleLabel)
            );
            assert_eq!(label(&lab).raw_text(), story(&lab).label());
        });
    }
}

// ------------------------------------------------------- showing a record

/// Showing another record drops the story that was running, with every edit
/// made to it; coming back starts the story again at its defaults.
#[test]
fn showing_another_record_discards_the_storys_edits() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        press(&mut lab, &mut engine, SUBJECT);
        press(&mut lab, &mut engine, field());
        type_text(&mut lab, &mut engine, " now");
        assert_eq!(story(&lab).presses(), 1);
        assert_ne!(story(&lab).label(), "Save changes");

        assert!(lab.show(&mut engine, record(NO_STORY)));
        draw(&mut lab, &mut engine, SIZE);
        assert!(lab.story.is_none());
        assert_eq!(lab.focus, None, "the field it was on is gone");
        assert!(!frame(&lab).contains(SUBJECT));
        assert!(!frame(&lab).contains(field()));
        assert_sound(&lab, "a record with no story");
        // Nothing to reset, and nothing to press.
        assert!(!lab.activate(&mut engine, RESET));
        assert!(!lab.activate(&mut engine, SUBJECT));
        assert!(!lab.show(&mut engine, record(NO_STORY)), "already shown");

        assert!(lab.show(&mut engine, record(registry::BUTTON)));
        draw(&mut lab, &mut engine, SIZE);
        assert!(story(&lab).is_pristine());
        assert_eq!(story(&lab).label(), "Save changes");
        assert_eq!(label(&lab).raw_text(), "Save changes");
        assert_eq!(lab.result_line(), "Not pressed yet");
    });
}

/// Reset puts the story back and leaves the rest of the window as it is:
/// the record on show, what is folded and where the list is scrolled.
#[test]
fn reset_keeps_the_navigator_as_it_is() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        assert!(lab.fold(&mut engine, Family::Compositions, true));
        draw(&mut lab, &mut engine, SIZE);
        let scrolled = lab.nav.scroll;
        assert!(scrolled > 0.0, "the list opened on the Button's row");
        press(&mut lab, &mut engine, SUBJECT);
        assert!(press(&mut lab, &mut engine, RESET));
        assert!(story(&lab).is_pristine());
        assert_eq!(lab.entry.catalog_id, registry::BUTTON);
        assert_eq!(lab.nav.scroll, scrolled);
        assert!(lab.nav.is_folded(Family::Compositions));
    });
}
