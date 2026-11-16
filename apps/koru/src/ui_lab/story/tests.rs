// SPDX-License-Identifier: MPL-2.0

use super::*;

fn story() -> ButtonStory {
    ButtonStory::new("Save changes")
}

#[test]
fn a_fresh_story_is_the_registered_default() {
    let story = story();
    assert_eq!(story.label(), "Save changes");
    assert_eq!(story.style(), ButtonRole::default());
    assert_eq!(story.presses(), 0);
    assert!(story.is_pristine());
    // The styles and the inputs on offer are the registration's, not a
    // second list kept beside it.
    assert_eq!(story.styles(), registry::DIALOG_STYLES);
    let (_, recipe) = registry::story(registry::BUTTON_DIALOG_STORY).unwrap();
    let opened = Story::open(registry::BUTTON_DIALOG_STORY).unwrap();
    assert_eq!(opened.controls(), recipe.controls);
    assert_eq!(opened.controls(), [Control::Label, Control::Style]);
}

#[test]
fn the_label_is_whatever_was_typed() {
    let mut story = story();
    assert!(story.set_label("Delete 3 files"));
    assert_eq!(story.label(), "Delete 3 files");
    assert!(
        !story.set_label("Delete 3 files"),
        "the same text is no change"
    );
    // Leading and trailing space, another script, and nothing at all are all
    // labels the production button can be handed.
    for label in ["  spaced  ", "保存", "حفظ التغييرات", ""] {
        assert!(story.set_label(label));
        assert_eq!(story.label(), label);
    }
    assert!(!story.is_pristine());
    assert_eq!(story.presses(), 0, "typing is not pressing");
}

#[test]
fn a_style_is_chosen_from_the_registered_ones_only() {
    let mut story = story();
    for (slot, style) in registry::DIALOG_STYLES.into_iter().enumerate() {
        let changes = style != story.style();
        assert_eq!(story.choose(slot), changes, "{style:?}");
        assert_eq!(story.style(), style);
        assert!(!story.choose(slot), "choosing it again is no change");
    }
    let before = story.clone();
    assert!(!story.choose(registry::DIALOG_STYLES.len()));
    assert!(!story.choose(usize::MAX));
    assert_eq!(story, before, "a slot that does not exist changes nothing");
}

#[test]
fn each_press_is_counted() {
    let mut story = story();
    story.press();
    assert_eq!(story.presses(), 1);
    assert!(!story.is_pristine());
    story.press();
    story.press();
    assert_eq!(story.presses(), 3);
    // Changing how it looks does not forget that it was pressed.
    story.choose(1);
    story.set_label("Again");
    assert_eq!(story.presses(), 3);
}

#[test]
fn reset_restores_the_label_the_style_and_the_click_result() {
    let mut story = story();
    assert!(!story.reset(), "a fresh story has nothing to reset");

    story.set_label("Changed");
    story.choose(2);
    story.press();
    assert!(story.reset());
    assert_eq!(story, self::story());
    assert!(story.is_pristine());
    assert!(!story.reset());

    // Each of the three alone is enough to need one.
    story.press();
    assert!(story.reset());
    story.choose(1);
    assert!(story.reset());
    story.set_label("");
    assert!(story.reset());
    assert_eq!(story.label(), "Save changes");
}

// ------------------------------------------------------- the general story

fn english<T>(body: impl FnOnce() -> T) -> T {
    koru_i18n::with_current(koru_i18n::Lang::En, body)
}

/// Every story the registry says is available opens, is the story it was
/// asked for, starts untouched and offers exactly its recipe's inputs; an ID
/// the registry does not have opens nothing.
#[test]
fn every_registered_story_opens_at_its_defaults() {
    english(|| {
        let mut opened = 0;
        for entry in registry::desktop() {
            for recipe in entry.recipes {
                let registry::Story::Available(id) = recipe.story else {
                    continue;
                };
                let story = Story::open(id)
                    .unwrap_or_else(|| panic!("`{id}` is registered and has no story"));
                assert_eq!(story.id(), id);
                assert!(story.is_pristine(), "{id}");
                assert_eq!(story.controls(), recipe.controls, "{id}");
                assert_eq!(entry.story_id(), Some(id), "{id} is its record's story");
                for control in recipe.controls {
                    // Each input is text or a choice, and answers as one.
                    match control.input() {
                        registry::Input::Text => {
                            assert!(story.text(*control).is_some(), "{id} {control:?}");
                            assert!(story.options(*control).is_empty());
                        }
                        registry::Input::Choice => {
                            let options = story.options(*control);
                            assert!(options.len() > 1, "{id} {control:?}");
                            let chosen = options.iter().filter(|option| option.selected);
                            assert_eq!(chosen.count(), 1, "{id} {control:?}");
                            assert_eq!(story.text(*control), None);
                            let mut parts: Vec<&str> =
                                options.iter().map(|option| option.part).collect();
                            parts.sort_unstable();
                            parts.dedup();
                            assert_eq!(parts.len(), options.len(), "{id} {control:?}");
                        }
                    }
                }
                opened += 1;
            }
        }
        assert_eq!(opened, 9, "the stories the Lab has");
        assert_eq!(Story::open("C-BUTTON/compact"), None);
        assert_eq!(Story::open(""), None);
    });
}

/// A choice steps through its options and stops at either end, and a reset
/// puts every input back.
#[test]
fn a_choice_steps_through_its_options_and_reset_puts_them_back() {
    english(|| {
        let mut story = Story::open(registry::BUTTON_DIALOG_STORY).unwrap();
        let slot = |story: &Story, control| {
            story
                .options(control)
                .iter()
                .position(|option| option.selected)
        };
        assert!(
            !story.step(Control::Style, false),
            "nothing before the first"
        );
        let last = registry::DIALOG_STYLES.len() - 1;
        for at in 1..=last {
            assert!(story.step(Control::Style, true));
            assert_eq!(slot(&story, Control::Style), Some(at));
        }
        assert!(!story.step(Control::Style, true), "nothing after the last");
        assert!(story.step(Control::Style, false));
        assert_eq!(slot(&story, Control::Style), Some(last - 1));
        // An input that is not this story's does nothing.
        assert!(!story.step(Control::Gap, true));
        assert!(!story.choose(Control::Padding, 3));
        assert!(!story.set_text(Control::Placeholder, "x"));
        assert!(story.press());
        assert!(story.reset());
        assert!(story.is_pristine());
        assert!(!story.reset(), "nothing left to put back");
    });
}

/// The text input's story: what is typed, its placeholder and the box it is
/// drawn in, each the production input's own.
#[test]
fn the_text_input_story_holds_what_is_typed_its_hint_and_its_box() {
    english(|| {
        let mut story = Story::open(registry::TEXT_INPUT_STORY).unwrap();
        let Story::Field(field) = &story else {
            panic!("the text input's story is a field");
        };
        assert_eq!(field.placeholder(), "Type something");
        assert_eq!(field.style(), registry::FieldStyle::Form);
        assert_eq!(field.typed(), "");
        let parts: Vec<&str> = story
            .options(Control::Style)
            .iter()
            .map(|option| option.part)
            .collect();
        assert_eq!(parts, ["form", "search", "inline"]);
        assert!(!story.press(), "a field is typed in, not pressed");

        assert!(story.set_typed("hello"));
        assert!(!story.set_typed("hello"));
        assert!(story.set_text(Control::Placeholder, "Your name"));
        assert!(story.choose(Control::Style, 1));
        assert!(!story.choose(Control::Style, 9));
        let Story::Field(field) = &story else {
            unreachable!()
        };
        assert_eq!(
            (field.typed(), field.placeholder(), field.style()),
            ("hello", "Your name", registry::FieldStyle::Search)
        );
        assert!(story.can_change());
        assert!(story.reset());
        assert_eq!(story, Story::open(registry::TEXT_INPUT_STORY).unwrap());
    });
}

/// A layout story's gap and padding are spacing steps: every step of the
/// production scale and no other, the row and the column alike.
#[test]
fn a_layout_storys_gap_and_padding_are_the_spacing_scale() {
    english(|| {
        for (id, axis) in [
            (registry::ROW_STORY, Axis::Horizontal),
            (registry::COLUMN_STORY, Axis::Vertical),
        ] {
            let mut story = Story::open(id).unwrap();
            let Story::Layout(layout) = story else {
                panic!("{id} is a layout");
            };
            assert_eq!(layout.axis(), axis);
            assert_eq!((layout.gap(), layout.padding()), (Space::S2, Space::S4));
            for control in [Control::Gap, Control::Padding] {
                let options = story.options(control);
                let names: Vec<&str> = options.iter().map(|option| option.part).collect();
                assert_eq!(names, Space::ALL.map(Space::name));
                // An option reads as the step's name and what it comes to.
                assert_eq!(options[8].label, OptionLabel::Literal("S4 (16)".to_owned()));
            }
            assert!(story.choose(Control::Gap, 14));
            assert!(story.choose(Control::Padding, 0));
            assert!(!story.choose(Control::Gap, 15), "no sixteenth step");
            let Story::Layout(layout) = story else {
                unreachable!()
            };
            assert_eq!((layout.gap(), layout.padding()), (Space::S12, Space::S0));
            assert!(!story.is_pristine());
            assert!(story.reset());
            assert_eq!(story, Story::open(id).unwrap());
        }
    });
}

/// A token table's rows are the production table's, in its order, each with
/// what it resolves to; a table has nothing to change.
#[test]
fn a_token_tables_rows_are_the_production_tables() {
    english(|| {
        let rows = |table: TokenTable| -> Vec<(&'static str, String)> {
            table
                .rows()
                .into_iter()
                .map(|row| (row.name, row.value))
                .collect()
        };
        let space = rows(TokenTable::Space);
        assert_eq!(space.len(), Space::ALL.len());
        for (row, step) in space.iter().zip(Space::ALL) {
            assert_eq!(row.0, step.name());
            let value = step.px(Profile::Desktop);
            assert_eq!(row.1.parse::<f32>().ok(), Some(value), "{}", row.0);
        }
        assert_eq!(space[8], ("S4", "16".to_owned()));
        assert_eq!(space[1], ("S0_5", "2".to_owned()));

        assert_eq!(
            rows(TokenTable::Surface),
            Surface::ALL
                .iter()
                .map(|role| (role.name(), role.field().to_owned()))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            rows(TokenTable::Surface)[0],
            ("Surface::Raised", "panel_bg".to_owned())
        );
        assert_eq!(rows(TokenTable::Ink).len(), Ink::ALL.len());
        assert_eq!(rows(TokenTable::Line).len(), Line::ALL.len());
        let roles_rows = rows(TokenTable::TextRoles);
        assert_eq!(roles_rows.len(), roles::ALL.len());
        for (row, role) in roles_rows.iter().zip(roles::ALL) {
            assert_eq!(row.0, role.name);
            let metrics = role.style().metrics;
            assert!(row.1.contains(&format!("{}", metrics.size)), "{row:?}");
            assert!(row.1.contains(" / "), "{row:?}");
        }
        for id in [
            registry::SPACE_STORY,
            registry::SURFACE_STORY,
            registry::INK_STORY,
            registry::LINE_STORY,
            registry::TEXT_ROLES_STORY,
        ] {
            let mut story = Story::open(id).unwrap();
            assert!(matches!(story, Story::Tokens(_)), "{id}");
            assert!(story.controls().is_empty());
            assert!(!story.can_change());
            assert!(!story.reset() && !story.press());
        }
    });
}
