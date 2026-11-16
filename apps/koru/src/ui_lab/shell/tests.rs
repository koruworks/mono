// SPDX-License-Identifier: MPL-2.0

//! The window driven the way the runner drives it — a draw, then pointer and
//! key events against that frame — with a real text engine and a scene, and
//! no window, GPU or event loop.
//!
//! The display language is set per thread (`koru_i18n::with_current`), never
//! on the process, so these run beside every other test in the binary.

use koru_ui::button::ButtonRole;

use crate::ui_lab::story::ButtonStory;
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

/// The button's story, which is the one on show.
pub(super) fn story(lab: &Lab) -> &ButtonStory {
    match lab.story.as_ref() {
        Some(Story::Button(story)) => story,
        other => panic!("the story on show is not the button's: {other:?}"),
    }
}

/// The line under the component on show.
pub(super) fn result(lab: &Lab) -> String {
    lab.result_line()
        .expect("the story on show says what using it did")
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
/// eight languages, at the default size and at the smallest one accepted,
/// where the properties wait to be asked for.
#[test]
fn the_regions_compose_in_every_language_and_at_the_smallest_size() {
    for lang in koru_i18n::Lang::ALL {
        koru_i18n::with_current(lang, || {
            for size in [SIZE, SMALLEST] {
                let (lab, _) = drawn(size);
                let context = format!("{lang:?} at {size:?}");
                assert_sound(&lab, &context);
                let frame = frame(&lab);
                let navigator = frame.rect_of("Navigator").unwrap();
                let preview = frame.rect_of("Preview").unwrap();
                let toolbar = frame.rect_of("Toolbar").unwrap();
                assert_eq!(navigator.x, 0.0, "{context}");
                assert!(navigator.x + navigator.w <= preview.x, "{context}");
                assert!(
                    preview.w >= PREVIEW_MIN_WIDTH.value,
                    "{context}: {preview:?}"
                );
                let mut regions = vec![navigator, preview];
                match frame.rect_of("Properties") {
                    Some(properties) => {
                        assert_eq!(size, SIZE, "{context}");
                        assert!(preview.x + preview.w <= properties.x, "{context}");
                        assert_eq!(properties.x + properties.w, size.0, "{context}");
                        // The style control, in the language that spells it
                        // longest, still fits the properties column.
                        let choice = lab.rect(style()).unwrap();
                        assert!(
                            choice.x + choice.w <= properties.x + properties.w,
                            "{context}: {choice:?} leaves {properties:?}"
                        );
                        regions.push(properties);
                    }
                    None => {
                        assert_eq!(size, SMALLEST, "{context}");
                        assert_eq!(preview.x + preview.w, size.0, "{context}");
                    }
                }
                // Every region runs to the bottom of the window. The side
                // columns start under the caption; the page starts under the
                // centre's own toolbar, which is exactly as wide as it.
                let caption = lab.caption_rect().unwrap();
                for region in &regions {
                    assert!(region.y >= caption.y + caption.h, "{context}");
                    assert_eq!(region.y + region.h, size.1, "{context}");
                }
                assert_eq!(navigator.y, toolbar.y, "{context}");
                assert!(preview.y >= toolbar.y + toolbar.h, "{context}");
                assert_eq!((toolbar.x, toolbar.w), (preview.x, preview.w), "{context}");
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

/// The keyboard's stops follow what is seen: the caption's two toggles; the
/// navigator's search and one row of its list; the centre's toolbar (Reset
/// beside the record's name, then the settings on their own row at this
/// width) and its page; then the story's own controls. Nothing stands in for a control
/// the Lab does not have yet.
#[test]
fn the_keyboard_stops_are_the_controls_that_exist() {
    english(|| {
        let (lab, _) = drawn(SIZE);
        assert_eq!(
            lab.laid.as_ref().unwrap().stops(),
            [
                Side::Navigator.toggle(),
                Side::Properties.toggle(),
                SEARCH,
                entry_key(registry::BUTTON),
                RESET,
                Setting::Theme.key(),
                Setting::Width.key(),
                Setting::Language.key(),
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
        assert_eq!(result(&lab), "Not pressed yet");
        assert!(press(&mut lab, &mut engine, SUBJECT));
        assert_eq!(story(&lab).presses(), 1);
        assert_eq!(result(&lab), "Presses: 1");
        assert_eq!(lab.focus, Some(SUBJECT), "a press takes the focus");

        // Two quick presses are two presses: the runner reports the second as
        // a double click, and it still counts.
        let (x, y) = centre(&lab, SUBJECT);
        assert!(lab.click(&mut engine, x, y, false));
        assert!(lab.double_click(&mut engine, x, y));
        assert_eq!(story(&lab).presses(), 3);
        assert_eq!(result(&lab), "Presses: 3");

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
            let line = result(&lab);
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
        assert_eq!(result(&lab), "Not pressed yet");
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
        assert_eq!(result(&lab), "Not pressed yet");
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

// ------------------------------------------------------- the side columns

const NAVIGATOR_TOGGLE: Key = Side::Navigator.toggle();
const PROPERTIES_TOGGLE: Key = Side::Properties.toggle();

/// Which of the three regions a frame holds.
fn regions(lab: &Lab) -> (bool, bool, bool) {
    let frame = frame(lab);
    (
        frame.rect_of("Navigator").is_some(),
        frame.rect_of("Preview").is_some(),
        frame.rect_of("Properties").is_some(),
    )
}

/// The preview is always there. A window with room shows both columns
/// beside it; one without shows the navigator and leaves the properties
/// until they are asked for.
#[test]
fn a_narrow_window_keeps_the_preview_and_gives_up_a_column() {
    english(|| {
        let least = PREVIEW_MIN_WIDTH.value;
        let (lab, _) = drawn(SIZE);
        assert_eq!(regions(&lab), (true, true, true));
        assert!(frame(&lab).rect_of("Preview").unwrap().w >= least);
        let (lab, _) = drawn(SMALLEST);
        assert_eq!(regions(&lab), (true, true, false));
        assert!(frame(&lab).rect_of("Preview").unwrap().w >= least);
        // The width at which the properties first have room.
        let both =
            NAVIGATOR_WIDTH.value + PROPERTIES_WIDTH.value + 2.0 * theme::BORDER_W as f32 + least;
        let (lab, _) = drawn((both, 700.0));
        assert_eq!(regions(&lab), (true, true, true));
        assert_eq!(frame(&lab).rect_of("Preview").unwrap().w, least);
        let (lab, _) = drawn((both - 1.0, 700.0));
        assert_eq!(regions(&lab), (true, true, false));
        // Both toggles are in the caption whatever shows, and say which is on.
        for size in [SIZE, SMALLEST] {
            let (lab, _) = drawn(size);
            let frame = frame(&lab);
            let caption = lab.caption_rect().unwrap();
            for side in Side::ALL {
                let toggle = frame.node(side.toggle()).unwrap();
                assert!(toggle.rect.y + toggle.rect.h <= caption.y + caption.h);
                assert!(toggle.rect.x + toggle.rect.w <= size.0);
                assert_eq!(
                    toggle.selected,
                    Some(lab.columns(size.0).shows(side)),
                    "{side:?} at {size:?}"
                );
            }
        }
    });
}

/// A toggle hides and shows its column, and the preview takes the room.
#[test]
fn a_toggle_hides_and_shows_its_column() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        let preview = |lab: &Lab| frame(lab).rect_of("Preview").unwrap();
        let with_both = preview(&lab);

        assert!(press(&mut lab, &mut engine, PROPERTIES_TOGGLE));
        assert_eq!(regions(&lab), (true, true, false));
        assert_eq!(lab.focus, Some(PROPERTIES_TOGGLE));
        assert_eq!(
            preview(&lab).w,
            with_both.w + PROPERTIES_WIDTH.value + theme::BORDER_W as f32
        );
        assert!(!frame(&lab).contains(field()), "its controls went with it");
        assert_sound(&lab, "no properties");
        // The story is the same story: hiding its inputs changes nothing.
        press(&mut lab, &mut engine, SUBJECT);
        assert_eq!(story(&lab).presses(), 1);

        assert!(press(&mut lab, &mut engine, NAVIGATOR_TOGGLE));
        assert_eq!(regions(&lab), (false, true, false));
        assert_eq!(preview(&lab).x, 0.0);
        assert_eq!(preview(&lab).w, SIZE.0);
        assert_sound(&lab, "the preview alone");
        // The record on show and the search are kept while the list is away.
        assert_eq!(lab.entry.catalog_id, registry::BUTTON);

        assert!(press(&mut lab, &mut engine, PROPERTIES_TOGGLE));
        assert!(press(&mut lab, &mut engine, NAVIGATOR_TOGGLE));
        assert_eq!(regions(&lab), (true, true, true));
        assert_eq!(preview(&lab), with_both);
        assert_eq!(story(&lab).presses(), 1);
        // The list comes back with the record on show in view.
        let row = frame(&lab).node(entry_key(registry::BUTTON)).unwrap();
        assert_eq!(row.selected, Some(true));
    });
}

/// In a window with no room for both, opening one column closes the other,
/// so the preview keeps its least width; with room again, both asked-for
/// columns show.
#[test]
fn in_a_narrow_window_opening_one_column_closes_the_other() {
    english(|| {
        let (mut lab, mut engine) = drawn(SMALLEST);
        let size = SMALLEST;
        let toggle = |lab: &mut Lab, engine: &mut TextEngine, key: Key| {
            let (x, y) = centre(lab, key);
            let changed = lab.click(engine, x, y, false);
            lab.mouse_up();
            draw(lab, engine, size);
            changed
        };
        assert!(toggle(&mut lab, &mut engine, PROPERTIES_TOGGLE));
        assert_eq!(regions(&lab), (false, true, true));
        let preview = frame(&lab).rect_of("Preview").unwrap();
        assert!(preview.w >= PREVIEW_MIN_WIDTH.value, "{preview:?}");
        assert!(frame(&lab).contains(field()));
        assert_sound(&lab, "the properties in a narrow window");

        assert!(toggle(&mut lab, &mut engine, NAVIGATOR_TOGGLE));
        assert_eq!(regions(&lab), (true, true, false));
        assert_sound(&lab, "the navigator in a narrow window");
        // Both were asked for: a window with room shows both.
        lab.resized(PhysicalSize::new(1120, 720), 1.0);
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(regions(&lab), (true, true, true));
        // And narrow again, the one opened last is the one that stays.
        lab.resized(PhysicalSize::new(720, 600), 1.0);
        draw(&mut lab, &mut engine, SMALLEST);
        assert_eq!(regions(&lab), (true, true, false));

        // Closing the navigator makes room for the properties that were
        // asked for; closing those leaves the preview the whole window.
        assert!(toggle(&mut lab, &mut engine, NAVIGATOR_TOGGLE));
        assert_eq!(regions(&lab), (false, true, true));
        assert!(toggle(&mut lab, &mut engine, PROPERTIES_TOGGLE));
        assert_eq!(regions(&lab), (false, true, false));
        assert_eq!(frame(&lab).rect_of("Preview").unwrap().w, SMALLEST.0);
        assert_sound(&lab, "the preview alone in the smallest window");
    });
}

/// When a column closes with the keyboard inside it, the keyboard goes to
/// the toggle that opens it again, and that toggle is the one drawn focused.
#[test]
fn a_closing_column_hands_the_keyboard_to_its_toggle() {
    english(|| {
        // The window is made too narrow for the properties.
        let (mut lab, mut engine) = drawn(SIZE);
        press(&mut lab, &mut engine, field());
        assert_eq!(lab.focus, Some(field()));
        lab.resized(PhysicalSize::new(720, 600), 1.0);
        draw(&mut lab, &mut engine, SMALLEST);
        assert_eq!(regions(&lab), (true, true, false));
        assert_eq!(lab.focus, Some(PROPERTIES_TOGGLE));
        let focused: Vec<Key> = frame(&lab)
            .nodes
            .iter()
            .filter(|node| node.state.focused)
            .filter_map(|node| node.key)
            .collect();
        assert_eq!(focused, [PROPERTIES_TOGGLE]);
        let ring = frame(&lab)
            .named("Column toggle")
            .find(|node| node.state.focused)
            .expect("the box round the toggle is lit");
        assert!(ring.ring.is_some());
        // Enter on it opens the column again, where the story was left.
        let none = ModifiersState::empty();
        assert!(lab.key(
            &mut engine,
            &KeyboardKey::Named(NamedKey::Enter),
            None,
            none
        ));
        draw(&mut lab, &mut engine, SMALLEST);
        assert_eq!(regions(&lab), (false, true, true));
        assert_eq!(story(&lab).label(), "Save changes");

        // Opening the properties in a narrow window closes the navigator
        // under a keyboard that was in its search field or on one of its
        // rows.
        for inside in [SEARCH, entry_key(registry::BUTTON)] {
            let (mut lab, mut engine) = drawn(SMALLEST);
            let (x, y) = centre(&lab, inside);
            lab.click(&mut engine, x, y, false);
            lab.mouse_up();
            assert_eq!(lab.focus, Some(inside));
            assert!(lab.set_column(Side::Properties, true));
            draw(&mut lab, &mut engine, SMALLEST);
            assert_eq!(regions(&lab), (false, true, true));
            assert_eq!(lab.focus, Some(NAVIGATOR_TOGGLE), "{inside:?}");
        }
        // A control outside the columns keeps the keyboard.
        let (mut lab, mut engine) = drawn(SIZE);
        press(&mut lab, &mut engine, SUBJECT);
        assert!(lab.set_column(Side::Properties, false));
        assert!(lab.set_column(Side::Navigator, false));
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(lab.focus, Some(SUBJECT));
    });
}

/// At the smallest window, in every language, everything can be reached
/// from the keyboard: the toolbar, the list, the preview, and the story's
/// inputs once their column is opened from its toggle.
#[test]
fn the_smallest_window_is_usable_from_the_keyboard_in_every_language() {
    for lang in koru_i18n::Lang::ALL {
        koru_i18n::with_current(lang, || {
            let (mut lab, mut engine) = drawn(SMALLEST);
            let context = format!("{lang:?}");
            assert_sound(&lab, &context);
            let step = |lab: &mut Lab, engine: &mut TextEngine, key: NamedKey| {
                let none = ModifiersState::empty();
                let changed = lab.key(engine, &KeyboardKey::Named(key), None, none);
                draw(lab, engine, SMALLEST);
                changed
            };
            let stops = lab.laid.as_ref().unwrap().stops();
            assert_eq!(stops.first(), Some(&NAVIGATOR_TOGGLE));
            for expected in [PROPERTIES_TOGGLE, SEARCH, PAGE, SUBJECT] {
                assert!(stops.contains(&expected), "{context}: {expected:?}");
            }
            assert!(!stops.contains(&field()), "{context}");
            // Every stop is inside the window.
            for stop in &stops {
                let rect = lab.rect(*stop).unwrap();
                assert!(
                    rect.x >= 0.0 && rect.x + rect.w <= SMALLEST.0,
                    "{context}: {stop:?} at {rect:?}"
                );
            }
            // Press the component, then open the properties and relabel it.
            while lab.focus != Some(SUBJECT) {
                assert!(step(&mut lab, &mut engine, NamedKey::Tab));
            }
            assert!(step(&mut lab, &mut engine, NamedKey::Enter));
            assert_eq!(story(&lab).presses(), 1);
            while lab.focus != Some(PROPERTIES_TOGGLE) {
                assert!(step(&mut lab, &mut engine, NamedKey::Tab));
            }
            assert!(step(&mut lab, &mut engine, NamedKey::Space));
            assert_eq!(regions(&lab), (false, true, true), "{context}");
            assert_sound(&lab, &context);
            while lab.focus != Some(field()) {
                assert!(step(&mut lab, &mut engine, NamedKey::Tab));
            }
            type_text(&mut lab, &mut engine, "Go");
            draw(&mut lab, &mut engine, SMALLEST);
            assert_eq!(story(&lab).label(), "Go", "{context}");
            assert!(step(&mut lab, &mut engine, NamedKey::Tab));
            assert_eq!(lab.focus, Some(style()));
            assert!(step(&mut lab, &mut engine, NamedKey::ArrowRight));
            assert_eq!(story(&lab).style(), ButtonRole::Primary);
            // Back to the list: its toggle, then Tab into it.
            while lab.focus != Some(NAVIGATOR_TOGGLE) {
                assert!(step(&mut lab, &mut engine, NamedKey::Tab));
            }
            assert!(step(&mut lab, &mut engine, NamedKey::Enter));
            assert_eq!(regions(&lab), (true, true, false), "{context}");
            assert_sound(&lab, &context);
            while !lab.focus.is_some_and(|key| lab.is_navigator_key(key)) {
                assert!(step(&mut lab, &mut engine, NamedKey::Tab));
            }
            assert_eq!(lab.focus, Some(entry_key(registry::BUTTON)));
            // The story kept what was done to it while its column was away.
            assert_eq!(story(&lab).label(), "Go");
            assert_eq!(story(&lab).presses(), 1);
        });
    }
}

// ------------------------------------- the caption, the toolbar, the chords

/// A column's chord, as the keyboard delivers it: the letter with the
/// platform's primary modifier.
fn chord(lab: &mut Lab, engine: &mut TextEngine, letter: &str, size: (f32, f32)) -> bool {
    let changed = lab.key(
        engine,
        &KeyboardKey::Character(letter.into()),
        None,
        koru_keys::PRIMARY,
    );
    draw(lab, engine, size);
    changed
}

/// The caption holds the navigator's toggle at its start and the
/// properties' toggle at its end, before the window's own three controls,
/// with the title between them. The toggles are pressed, not dragged; the
/// rest of the caption still drags the window.
#[test]
fn the_caption_holds_the_toggles_and_drags_everywhere_else() {
    for lang in koru_i18n::Lang::ALL {
        koru_i18n::with_current(lang, || {
            for size in [SIZE, SMALLEST] {
                let (mut lab, mut engine) = drawn(size);
                let context = format!("{lang:?} at {size:?}");
                let caption = lab.caption_rect().unwrap();
                let frame = frame(&lab);
                let navigator = frame.node(NAVIGATOR_TOGGLE).unwrap().rect;
                let properties = frame.node(PROPERTIES_TOGGLE).unwrap().rect;
                let title = frame.rect_of("Window title").unwrap();
                let minimise = caption::direct_controls(caption)[0].rect;
                for toggle in [navigator, properties] {
                    assert!(
                        toggle.y >= caption.y && toggle.y + toggle.h <= caption.y + caption.h,
                        "{context}: {toggle:?} leaves {caption:?}"
                    );
                }
                // Left to right: the Koru mark, the title, the navigator's
                // toggle; then, at the end, the properties' toggle and the
                // window's controls.
                let mark = frame.node(MARK).unwrap().rect;
                let gap = CAPTION_GAP.token.px(Profile::Desktop);
                assert_eq!((mark.x, mark.y), (caption.x, caption.y), "{context}");
                assert_eq!((mark.w, mark.h), (theme::CAPTION_H, theme::CAPTION_H));
                assert_eq!(title.x, mark.x + mark.w + gap, "{context}");
                assert_eq!(navigator.x, title.x + title.w + gap, "{context}");
                assert!(navigator.x + navigator.w <= properties.x, "{context}");
                assert_eq!(properties.x + properties.w + gap, minimise.x, "{context}");
                assert!(title.w > 0.0, "{context}: the title has room");
                // The mark is not a button: it cannot be pressed or tabbed
                // to, and it drags the window like the title beside it.
                let (mx, my) = mark.centre();
                assert_eq!(lab.hit_at(mx, my), None, "{context}");
                assert!(lab.is_caption(mx, my), "{context}: the mark drags");
                assert!(!lab.laid.as_ref().unwrap().stops().contains(&MARK));
                // The focus ring's box is inside the caption too.
                for ring in frame.named("Column toggle") {
                    assert!(ring.rect.y >= caption.y);
                    assert!(ring.rect.y + ring.rect.h <= caption.y + caption.h);
                }

                // A press on a toggle is the toggle's; everywhere else in
                // the caption that is not a button drags the window.
                for toggle in [navigator, properties] {
                    let (x, y) = toggle.centre();
                    assert!(!lab.is_caption(x, y), "{context}: a toggle is dragged");
                    assert_eq!(lab.caption_action_at(x, y), None, "{context}");
                }
                let (tx, ty) = title.centre();
                assert!(lab.is_caption(tx, ty), "{context}: the title drags");
                let between = (navigator.x + navigator.w + properties.x) * 0.5;
                assert!(
                    lab.is_caption(between, ty),
                    "{context}: the empty band drags"
                );
                assert!(
                    lab.is_caption(navigator.x + navigator.w + 3.0, ty),
                    "{context}"
                );
                assert!(lab.is_caption(tx, caption.y + 1.0), "{context}");
                for control in caption::direct_controls(caption) {
                    let (x, y) = control.rect.centre();
                    assert!(!lab.is_caption(x, y), "{context}");
                }
                // And the press does what the toggle does.
                let before = regions(&lab);
                let (x, y) = navigator.centre();
                assert!(lab.click(&mut engine, x, y, false));
                lab.mouse_up();
                draw(&mut lab, &mut engine, size);
                assert_eq!(regions(&lab).0, !before.0, "{context}");
                assert_eq!(lab.focus, Some(NAVIGATOR_TOGGLE));
                assert_sound(&lab, &context);
            }
        });
    }
}

/// The side columns run from the caption to the bottom of the window; the
/// toolbar is the centre's own, exactly as wide as the page under it.
#[test]
fn the_columns_reach_the_caption_and_the_toolbar_is_the_centres() {
    english(|| {
        let (lab, _) = drawn(SIZE);
        let frame = frame(&lab);
        let caption = lab.caption_rect().unwrap();
        let under_caption = caption.y + caption.h + theme::BORDER_W as f32;
        let navigator = frame.rect_of("Navigator").unwrap();
        let properties = frame.rect_of("Properties").unwrap();
        let toolbar = frame.rect_of("Toolbar").unwrap();
        let preview = frame.rect_of("Preview").unwrap();
        for column in [navigator, properties] {
            assert_eq!(column.y, under_caption);
            assert_eq!(column.y + column.h, SIZE.1);
        }
        assert_eq!(toolbar.y, under_caption);
        assert_eq!((toolbar.x, toolbar.w), (preview.x, preview.w));
        assert!(toolbar.x >= navigator.x + navigator.w);
        assert!(toolbar.x + toolbar.w <= properties.x);
        assert_eq!(preview.y, toolbar.y + toolbar.h + theme::BORDER_W as f32);
        assert_eq!(preview.y + preview.h, SIZE.1);
        // Everything the toolbar holds is over the page and nowhere else.
        for key in [
            Setting::Theme.key(),
            Setting::Width.key(),
            Setting::Language.key(),
            RESET,
        ] {
            let rect = lab.rect(key).unwrap();
            assert!(rect.x >= toolbar.x && rect.x + rect.w <= toolbar.x + toolbar.w);
            assert!(rect.y >= toolbar.y && rect.y + rect.h <= toolbar.y + toolbar.h);
        }
        let title = frame.rect_of("Story title").unwrap();
        assert!(title.x >= toolbar.x && title.x + title.w <= toolbar.x + toolbar.w);
    });
}

/// The toolbar never leaves the centre. With room, the record's name, the
/// settings and Reset share one row; with less, the settings take a row of
/// their own; with less still they share that row's width evenly. Checked
/// at every arrangement of columns the smallest window allows, at the
/// narrowest centre there is, and in every language.
#[test]
fn the_toolbar_fits_the_narrowest_centre_in_every_language() {
    let narrowest = NAVIGATOR_WIDTH.value
        + PROPERTIES_WIDTH.value
        + 2.0 * theme::BORDER_W as f32
        + PREVIEW_MIN_WIDTH.value;
    for lang in koru_i18n::Lang::ALL {
        koru_i18n::with_current(lang, || {
            // (window, properties asked for, navigator asked for)
            let cases = [
                ((1900.0, 800.0), None, None),
                (SIZE, None, None),
                (SMALLEST, None, None),
                (SMALLEST, Some(true), None),
                (SMALLEST, Some(false), Some(false)),
                ((narrowest, 700.0), None, None),
            ];
            for (size, properties, navigator) in cases {
                let (mut lab, mut engine) = drawn(size);
                if let Some(open) = properties {
                    lab.set_column(Side::Properties, open);
                }
                if let Some(open) = navigator {
                    lab.set_column(Side::Navigator, open);
                }
                draw(&mut lab, &mut engine, size);
                let context = format!("{lang:?} at {size:?} {properties:?} {navigator:?}");
                assert_sound(&lab, &context);
                let frame = frame(&lab);
                let toolbar = frame.rect_of("Toolbar").unwrap();
                let preview = frame.rect_of("Preview").unwrap();
                assert_eq!((toolbar.x, toolbar.w), (preview.x, preview.w), "{context}");
                let controls: Vec<Rect> = [
                    Setting::Theme.key(),
                    Setting::Width.key(),
                    Setting::Language.key(),
                    RESET,
                ]
                .into_iter()
                .map(|key| lab.rect(key).unwrap())
                .collect();
                for rect in &controls {
                    assert!(
                        rect.x >= toolbar.x && rect.x + rect.w <= toolbar.x + toolbar.w,
                        "{context}: {rect:?} leaves {toolbar:?}"
                    );
                }
                // A setting's field is never squeezed to nothing: there is
                // always room for its chevron and the start of its value.
                for rect in &controls[..3] {
                    assert!(rect.w >= 64.0, "{context}: {rect:?} is too narrow");
                }
                // No two controls on one row overlap.
                for (at, a) in controls.iter().enumerate() {
                    for b in &controls[at + 1..] {
                        let same_row = a.y < b.y + b.h && b.y < a.y + a.h;
                        let apart = a.x + a.w <= b.x || b.x + b.w <= a.x;
                        assert!(!same_row || apart, "{context}: {a:?} over {b:?}");
                    }
                }
                let title = frame.rect_of("Story title").unwrap();
                assert!(title.w > 0.0, "{context}: the name is not squeezed out");
                assert!(title.x + title.w <= lab.rect(RESET).unwrap().x, "{context}");
                // Never wider than its widest option needs.
                for (setting, rect) in Setting::ALL.into_iter().zip(&controls) {
                    let full = lab.dropdown_width(&mut engine, setting.key());
                    assert!(rect.w <= full + 0.5, "{context}: {setting:?}");
                }
                let two_rows = frame.rect_of("Toolbar settings").is_some();
                if size.0 >= 1900.0 {
                    assert!(!two_rows, "{context}: a wide centre has one row");
                }
                if toolbar.w <= 480.0 {
                    assert!(two_rows, "{context}: a narrow centre has two rows");
                }
            }
        });
    }
    english(|| {
        // The narrowest centre: the three settings share their row evenly,
        // and a narrowed field still opens its list under itself.
        let size = (narrowest, 700.0);
        let (mut lab, mut engine) = drawn(size);
        assert_eq!(regions(&lab), (true, true, true));
        assert_eq!(
            frame(&lab).rect_of("Preview").unwrap().w,
            PREVIEW_MIN_WIDTH.value
        );
        let widths: Vec<f32> = Setting::ALL
            .into_iter()
            .map(|setting| lab.rect(setting.key()).unwrap().w)
            .collect();
        assert!((widths[0] - widths[1]).abs() < 0.5 && (widths[1] - widths[2]).abs() < 0.5);
        let full = lab.dropdown_width(&mut engine, Setting::Language.key());
        assert!(widths[2] < full, "the language field gave up width");
        let (x, y) = centre(&lab, Setting::Language.key());
        assert!(lab.click(&mut engine, x, y, false));
        let open = lab
            .menu
            .as_ref()
            .expect("the narrowed field opens its list");
        let panel = open.menu.panel(size);
        assert!(panel.x >= 0.0 && panel.x + panel.w <= size.0);
        // Tab follows what is seen: the name's row, then the settings' row.
        let stops = lab.laid.as_ref().unwrap().stops();
        let at = |key: Key| stops.iter().position(|stop| *stop == key).unwrap();
        assert!(at(RESET) < at(Setting::Theme.key()));
        assert!(at(Setting::Language.key()) < at(PAGE));
        assert!(
            at(SEARCH) < at(RESET),
            "the navigator is read before the centre"
        );
    });
    english(|| {
        // One row: the name, the settings, then Reset, as they are seen.
        let (lab, _) = drawn((1900.0, 800.0));
        let stops = lab.laid.as_ref().unwrap().stops();
        let at = |key: Key| stops.iter().position(|stop| *stop == key).unwrap();
        assert!(at(Setting::Language.key()) < at(RESET));
        let y = lab.rect(RESET).unwrap().centre().1;
        let theme = lab.rect(Setting::Theme.key()).unwrap();
        assert!(theme.y < y && y < theme.y + theme.h, "one row");
    });
}

/// The primary modifier with B shows and hides the navigator, and with J
/// the properties, wherever the keyboard is. In a field the chord is not
/// typed and not taken by the field; a plain letter still is.
#[test]
fn the_column_chords_work_from_anywhere_and_type_nothing() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        // With the keyboard nowhere.
        assert!(chord(&mut lab, &mut engine, "b", SIZE));
        assert_eq!(regions(&lab), (false, true, true));
        assert_eq!(lab.focus, None, "nothing was focused and nothing is");
        assert!(chord(&mut lab, &mut engine, "b", SIZE));
        assert!(chord(&mut lab, &mut engine, "j", SIZE));
        assert_eq!(regions(&lab), (true, true, false));
        assert!(chord(&mut lab, &mut engine, "j", SIZE));
        assert_eq!(regions(&lab), (true, true, true));

        // In the search field: not typed, and the field keeps the keyboard
        // when it is the other column that moves.
        press(&mut lab, &mut engine, SEARCH);
        type_text(&mut lab, &mut engine, "butt");
        assert!(chord(&mut lab, &mut engine, "j", SIZE));
        assert_eq!(lab.query(), "butt");
        assert_eq!(lab.focus, Some(SEARCH));
        assert_eq!(regions(&lab), (true, true, false));
        // A plain letter is still typed.
        type_text(&mut lab, &mut engine, "bj");
        assert_eq!(lab.query(), "buttbj");
        // Windows delivers the chord as a control character: 0x02 and 0x0A.
        for (code, shows) in [
            ("\u{2}", (false, true, false)),
            ("\u{a}", (false, true, true)),
        ] {
            assert!(lab.key(
                &mut engine,
                &KeyboardKey::Character(code.into()),
                Some(code),
                koru_keys::PRIMARY,
            ));
            draw(&mut lab, &mut engine, SIZE);
            assert_eq!(regions(&lab), shows);
            assert_eq!(lab.query(), "buttbj", "a control character is not typed");
        }
        // The navigator closed under the keyboard: its toggle has it now.
        assert_eq!(lab.focus, Some(NAVIGATOR_TOGGLE));
        assert!(chord(&mut lab, &mut engine, "b", SIZE));

        // In a story's own field.
        press(&mut lab, &mut engine, field());
        assert!(chord(&mut lab, &mut engine, "b", SIZE));
        assert_eq!(story(&lab).label(), "Save changes");
        assert_eq!(lab.focus, Some(field()));
        assert!(chord(&mut lab, &mut engine, "j", SIZE));
        assert_eq!(lab.focus, Some(PROPERTIES_TOGGLE), "its column closed");
        assert_eq!(story(&lab).label(), "Save changes");
        assert!(chord(&mut lab, &mut engine, "b", SIZE));
        assert!(chord(&mut lab, &mut engine, "j", SIZE));
        assert_eq!(regions(&lab), (true, true, true));

        // Only the chord itself: not with Shift or Alt, not without the
        // modifier, and not the other platform's primary.
        let before = regions(&lab);
        let b = KeyboardKey::Character("b".into());
        let foreign = if koru_keys::PRIMARY == ModifiersState::CONTROL {
            ModifiersState::SUPER
        } else {
            ModifiersState::CONTROL
        };
        lab.set_focus(&mut engine, None);
        for mods in [
            koru_keys::PRIMARY | ModifiersState::SHIFT,
            koru_keys::PRIMARY | ModifiersState::ALT,
            ModifiersState::empty(),
            foreign,
        ] {
            assert!(!lab.key(&mut engine, &b, None, mods), "{mods:?}");
        }
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(regions(&lab), before);
    });
}

/// A held chord toggles once, a chord during a composition is left to the
/// composition, and a chord with a list open closes the list first.
#[test]
fn a_column_chord_is_not_repeated_and_waits_for_a_composition() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        let b = KeyboardKey::Character("b".into());
        let event = |repeat| RichKeyEvent {
            logical_key: &b,
            physical_key: PhysicalKey::Code(KeyCode::KeyB),
            location: KeyLocation::Standard,
            text: None,
            state: KeyState::Pressed,
            repeat,
            modifiers: koru_keys::PRIMARY,
        };
        assert!(lab.rich_key(&mut engine, event(false)));
        for _ in 0..4 {
            assert!(!lab.rich_key(&mut engine, event(true)));
        }
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(regions(&lab), (false, true, true), "toggled once");
        assert!(chord(&mut lab, &mut engine, "b", SIZE));

        // Composing in the search field: the chord is not ours to take.
        press(&mut lab, &mut engine, SEARCH);
        lab.ime(&mut engine, ImeEvent::Enabled);
        assert!(lab.ime(
            &mut engine,
            ImeEvent::Preedit {
                text: "ぼ",
                cursor: Some((3, 3)),
            },
        ));
        assert!(!chord(&mut lab, &mut engine, "b", SIZE));
        assert!(!chord(&mut lab, &mut engine, "j", SIZE));
        assert_eq!(regions(&lab), (true, true, true));
        assert!(lab.field(SEARCH).unwrap().is_composing(), "still composing");
        assert!(lab.ime(&mut engine, ImeEvent::Commit { text: "ぼ" }));
        assert!(chord(&mut lab, &mut engine, "j", SIZE));
        assert_eq!(regions(&lab), (true, true, false));
        assert_eq!(lab.query(), "ぼ");

        // With a list open: the list closes and the column still toggles.
        lab.set_field(&mut engine, SEARCH, "");
        assert!(press(&mut lab, &mut engine, Setting::Theme.key()));
        assert!(lab.menu.is_some());
        assert!(chord(&mut lab, &mut engine, "j", SIZE));
        assert!(lab.menu.is_none());
        assert_eq!(regions(&lab), (true, true, true));
    });
}

/// In a window with no room for both, a chord follows the same rule as a
/// press: opening one column closes the other, and a column that closes
/// under the keyboard hands it to its toggle.
#[test]
fn a_column_chord_in_a_narrow_window_keeps_the_preview() {
    english(|| {
        let (mut lab, mut engine) = drawn(SMALLEST);
        assert_eq!(regions(&lab), (true, true, false));
        let (x, y) = centre(&lab, SEARCH);
        lab.click(&mut engine, x, y, false);
        lab.mouse_up();
        assert!(chord(&mut lab, &mut engine, "j", SMALLEST));
        assert_eq!(regions(&lab), (false, true, true));
        assert_eq!(lab.focus, Some(NAVIGATOR_TOGGLE));
        assert!(frame(&lab).rect_of("Preview").unwrap().w >= PREVIEW_MIN_WIDTH.value);
        assert!(chord(&mut lab, &mut engine, "b", SMALLEST));
        assert_eq!(regions(&lab), (true, true, false));
        assert_sound(&lab, "after the chords");
    });
}

/// A column's toggle says its column shows by its glyph's filled form and by
/// nothing else: at rest it has no plate and its glyph has the caption's
/// ordinary ink, whether the column shows or not. Under the pointer it has
/// the ordinary hover plate. The state still reaches a screen reader, which
/// is tested with the tree.
#[test]
fn a_column_toggle_is_lit_by_nothing_but_its_glyph() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        // The toggle's own look, and its glyph's: the first child.
        let looks = |lab: &Lab, key: Key| {
            let frame = frame(lab);
            let at = frame
                .nodes
                .iter()
                .position(|node| node.key == Some(key))
                .unwrap();
            (frame.nodes[at].look, frame.nodes[at + 1].look)
        };
        let shown = looks(&lab, PROPERTIES_TOGGLE);
        assert_eq!(shown, (Look::NONE, Look::NONE), "no plate, no tint");
        assert_eq!(
            frame(&lab).node(PROPERTIES_TOGGLE).unwrap().selected,
            Some(true)
        );
        // Hidden: exactly the same paint. Only the glyph's form differs.
        lab.set_column(Side::Properties, false);
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(looks(&lab, PROPERTIES_TOGGLE), shown);
        assert_eq!(
            frame(&lab).node(PROPERTIES_TOGGLE).unwrap().selected,
            Some(false)
        );
        // Under the pointer: the ordinary hover plate and the text ink, on
        // or off, and never the accent.
        for open in [false, true] {
            lab.set_column(Side::Properties, open);
            draw(&mut lab, &mut engine, SIZE);
            let (x, y) = centre(&lab, PROPERTIES_TOGGLE);
            lab.pointer_moved(x, y);
            draw(&mut lab, &mut engine, SIZE);
            let (plate, glyph) = looks(&lab, PROPERTIES_TOGGLE);
            assert_eq!(plate.bg, Some(Surface::Hover), "open: {open}");
            assert_eq!(glyph.ink, Some(Ink::Primary), "open: {open}");
            assert_eq!(glyph.bg, None);
            lab.pointer_left();
        }
        // The keyboard's mark is the ring, on the toggle itself.
        lab.set_focus(&mut engine, Some(NAVIGATOR_TOGGLE));
        draw(&mut lab, &mut engine, SIZE);
        let toggle = frame(&lab).node(NAVIGATOR_TOGGLE).unwrap();
        assert!(toggle.state.focused && toggle.ring.is_some());
        assert_eq!(toggle.look, Look::NONE);
        assert_eq!(toggle.rect.w, TOGGLE_SIZE.value);
        assert!(toggle.rect.h <= theme::CAPTION_H);
    });
}
