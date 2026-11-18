// SPDX-License-Identifier: MPL-2.0

//! What the inline field commits, and what it refuses.
//!
//! Everything else here is `TextField`, which has its own tests one crate down.

use super::*;

fn field(color: &str) -> (NewList, TextEngine) {
    let mut e = TextEngine::new();
    let f = NewList::new(&mut e, color, None);
    (f, e)
}

fn typed(f: &mut NewList, e: &mut TextEngine, text: &str) {
    for ch in text.chars() {
        let k = Key::Character(ch.to_string().into());
        f.key(e, &k, Some(&ch.to_string()), ModifiersState::empty(), &[]);
    }
}

fn press(f: &mut NewList, e: &mut TextEngine, key: &Key, taken: &[String]) -> Outcome {
    f.key(e, key, None, ModifiersState::empty(), taken)
}

const ENTER: Key = Key::Named(NamedKey::Enter);
const ESC: Key = Key::Named(NamedKey::Escape);

/// The whole gesture: type a name, press Enter, keep it. One field, one
/// colour, no dialog.
#[test]
fn enter_keeps_the_list_with_the_colour_it_was_offered() {
    let (mut f, mut e) = field("teal");
    typed(&mut f, &mut e, "Bike Project");
    assert_eq!(f.color(), "teal");
    match press(&mut f, &mut e, &ENTER, &[]) {
        Outcome::Commit(name) => assert_eq!(name, "Bike Project"),
        other => panic!("expected a commit, got {other:?}"),
    }
}

#[test]
fn escape_cancels_without_making_anything() {
    let (mut f, mut e) = field("red");
    typed(&mut f, &mut e, "Wedding");
    assert_eq!(press(&mut f, &mut e, &ESC, &[]), Outcome::Cancel);
}

/// Nothing typed is nothing meant. Enter on an empty field closes it rather
/// than complaining about a box the person has already decided against —
/// which is where this parts company with the rename field, whose empty case
/// is a name being *taken away*.
#[test]
fn enter_on_an_empty_field_just_closes_it() {
    let (mut f, mut e) = field("red");
    assert_eq!(press(&mut f, &mut e, &ENTER, &[]), Outcome::Cancel);
    assert!(f.hint().is_none(), "and says nothing about it");
}

/// A duplicate stays open with the text intact and a line of red beside it.
/// Case-insensitively: two rows reading `Wedding` and `wedding` are one row
/// twice in the only place either name is read.
#[test]
fn a_name_another_list_has_is_refused_inline() {
    let (mut f, mut e) = field("red");
    typed(&mut f, &mut e, "wedding");
    let taken = vec!["Wedding".to_owned()];
    assert_eq!(press(&mut f, &mut e, &ENTER, &taken), Outcome::Keep);
    assert!(f.hint().is_some(), "and it says why");
    // The next keystroke clears the complaint: you are already fixing it.
    typed(&mut f, &mut e, "s");
    assert!(f.hint().is_none());
    match press(&mut f, &mut e, &ENTER, &taken) {
        Outcome::Commit(name) => assert_eq!(name, "weddings"),
        other => panic!("expected a commit, got {other:?}"),
    }
}

/// Clicking away is done, not discarded — except where committing would be a
/// way around what Enter refuses.
#[test]
fn a_press_outside_commits_unless_enter_would_have_refused() {
    let row = Rect::new(0.0, 0.0, 180.0, 24.0);
    let (mut f, mut e) = field("red");
    typed(&mut f, &mut e, "Reading");
    match f.mouse_down(&mut e, row, 400.0, 400.0, &[]) {
        Outcome::Commit(name) => assert_eq!(name, "Reading"),
        other => panic!("expected a commit, got {other:?}"),
    }

    let (mut f, mut e) = field("red");
    typed(&mut f, &mut e, "Reading");
    let taken = vec!["reading".to_owned()];
    assert_eq!(
        f.mouse_down(&mut e, row, 400.0, 400.0, &taken),
        Outcome::Cancel
    );

    // Inside the field is the caret's, not the panel's.
    let (mut f, mut e) = field("red");
    typed(&mut f, &mut e, "Reading");
    assert_eq!(f.mouse_down(&mut e, row, 20.0, 12.0, &[]), Outcome::Keep);
    f.mouse_up();
}
