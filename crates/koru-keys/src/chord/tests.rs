// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::modifiers::{NONE, PRIMARY, PRIMARY_SHIFT};

fn ch(s: &str) -> Key {
    Key::Character(s.into())
}

fn key(n: NamedKey) -> Key {
    Key::Named(n)
}

/// Both encodings of Ctrl+letter reach the same chord. This is the trap that
/// made Ctrl+K do nothing for a release.
#[test]
fn a_letter_matches_in_either_encoding() {
    let c = Chord::new(On::Letter('k'), PRIMARY);
    assert!(c.matches(&ch("k"), Some("k"), PRIMARY));
    // 0x0B is Ctrl+K as Windows delivers it.
    assert!(c.matches(&ch("\u{0b}"), Some("\u{0b}"), PRIMARY));
}

/// **Ctrl+I arrives as Tab on Windows**, so italic has to accept a named Tab —
/// while a bare Tab, with no modifier, must still be a bare Tab.
#[test]
fn ctrl_i_survives_arriving_as_tab_without_swallowing_tab() {
    let italic = Chord::new(On::Letter('i'), PRIMARY);
    assert!(italic.matches(&key(NamedKey::Tab), None, PRIMARY));

    let indent = Chord::new(On::Named(NamedKey::Tab), NONE);
    assert!(indent.matches(&key(NamedKey::Tab), None, NONE));
    assert!(!italic.matches(&key(NamedKey::Tab), None, NONE));
}

/// Modifiers compare exactly: a chord written without Shift does not fire with
/// Shift held. Every "why did Ctrl+Shift+E also do Ctrl+E" bug is this.
#[test]
fn modifiers_are_exact() {
    let plain = Chord::new(On::Letter('e'), PRIMARY);
    assert!(plain.matches(&ch("e"), Some("e"), PRIMARY));
    assert!(!plain.matches(&ch("e"), Some("e"), PRIMARY_SHIFT));

    let shifted = Chord::new(On::Letter('e'), PRIMARY_SHIFT);
    assert!(shifted.matches(&ch("e"), Some("e"), PRIMARY_SHIFT));
    assert!(!shifted.matches(&ch("e"), Some("e"), PRIMARY));
}

/// **The foreign primary vetoes a chord.** ⌘+B means nothing on Windows and
/// Ctrl+B means nothing on macOS; without this a resolver testing only its own
/// modifier fires on both.
#[test]
fn the_other_platforms_primary_does_not_fire_a_chord() {
    let bold = Chord::new(On::Letter('b'), PRIMARY);
    let foreign = if cfg!(target_os = "macos") {
        ModifiersState::CONTROL
    } else {
        ModifiersState::SUPER
    };
    assert!(!bold.matches(&ch("b"), Some("b"), foreign));

    // And it vetoes an unmodified chord too — a bare Escape is not Escape with
    // the other platform's command key down.
    let escape = Chord::new(On::Named(NamedKey::Escape), NONE);
    assert!(escape.matches(&key(NamedKey::Escape), None, NONE));
    assert!(!escape.matches(&key(NamedKey::Escape), None, foreign));
}

/// A chord written on the literal Control key still works on macOS, where
/// Control is the *foreign* primary. Tab cycling depends on this: ⌘+Tab is the
/// OS app switcher and is not ours to take.
#[test]
fn a_chord_can_ask_for_the_literal_control_key() {
    let cycle = Chord::new(On::Named(NamedKey::Tab), ModifiersState::CONTROL);
    assert!(cycle.matches(&key(NamedKey::Tab), None, ModifiersState::CONTROL));
}

/// Digits pass through unchanged, which is what lets `Ctrl+1` select a workspace.
#[test]
fn digits_and_punctuation_arrive_verbatim() {
    assert_eq!(letter(&ch("1"), Some("1")), Some('1'));
    assert_eq!(letter(&ch("="), Some("=")), Some('='));
    // 0x1C is Ctrl+backslash.
    assert_eq!(letter(&ch("\u{1c}"), None), Some('\\'));
}

/// Case is normalised, so a chord table never has to list both.
#[test]
fn letters_are_lowercased() {
    assert_eq!(letter(&ch("B"), Some("B")), Some('b'));
}
