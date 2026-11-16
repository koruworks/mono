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
    assert_eq!(story.style_slot(), Some(0));
    assert_eq!(story.presses(), 0);
    assert!(story.is_pristine());
    // The styles and the inputs on offer are the registration's, not a
    // second list kept beside it.
    assert_eq!(story.styles(), registry::DIALOG_STYLES);
    let (_, recipe) = registry::story(registry::BUTTON_DIALOG_STORY).unwrap();
    assert_eq!(story.controls(), recipe.controls);
    assert_eq!(story.controls(), [Control::Label, Control::Style]);
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
        assert_eq!(story.style_slot(), Some(slot));
        assert!(!story.choose(slot), "choosing it again is no change");
    }
    let before = story.clone();
    assert!(!story.choose(registry::DIALOG_STYLES.len()));
    assert!(!story.choose(usize::MAX));
    assert_eq!(story, before, "a slot that does not exist changes nothing");
}

#[test]
fn the_arrow_keys_walk_the_styles_and_stop_at_the_ends() {
    let mut story = story();
    assert!(!story.step_style(false), "nothing before the first");
    let last = registry::DIALOG_STYLES.len() - 1;
    for slot in 1..=last {
        assert!(story.step_style(true));
        assert_eq!(story.style_slot(), Some(slot));
    }
    assert!(!story.step_style(true), "nothing after the last");
    assert_eq!(story.style(), registry::DIALOG_STYLES[last]);
    assert!(story.step_style(false));
    assert_eq!(story.style_slot(), Some(last - 1));
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
