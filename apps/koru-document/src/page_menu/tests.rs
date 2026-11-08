// SPDX-License-Identifier: MPL-2.0

//! The page menu's rows, and what each one does.
//!
//! The rows are conditional — the page-color row exists only in Page mode — so
//! every index after it moves with the mode. That is why `rows` returns each
//! row *with* its meaning rather than leaving a second function to count.

use super::*;

/// The pre-language arity, shadowing the module's: these tests exercise the
/// row set, which none of the three arguments changes.
fn rows(mode: ViewMode, canvas: usize) -> Vec<(Row, Option<Choice>)> {
    super::rows(mode, canvas, None, koru_i18n::Lang::En, true)
}

fn labels(mode: ViewMode) -> Vec<String> {
    rows(mode, 0)
        .into_iter()
        .filter_map(|(r, _)| match r {
            Row::Item { label, .. } => Some(label.into_owned()),
            _ => None,
        })
        .collect()
}

fn ticked(mode: ViewMode) -> Vec<String> {
    rows(mode, 0)
        .into_iter()
        .filter_map(|(r, _)| match r {
            Row::Item {
                label,
                trailing: Trailing::Check,
                ..
            } => Some(label.into_owned()),
            _ => None,
        })
        .collect()
}

/// Every width row means the mode it is labelled with, whatever the mode is —
/// which is the assertion that catches an index shifting under the color row.
#[test]
fn every_width_row_means_its_own_mode() {
    for mode in [ViewMode::Page, ViewMode::Narrow, ViewMode::Full] {
        let found: Vec<_> = rows(mode, 0)
            .into_iter()
            .filter_map(|(r, c)| match (r, c) {
                (Row::Item { label, .. }, Some(Choice::Mode(m))) => Some((label.into_owned(), m)),
                _ => None,
            })
            .collect();
        assert_eq!(
            found,
            vec![
                ("Page".to_string(), ViewMode::Page),
                ("Narrow".to_string(), ViewMode::Narrow),
                ("Full width".to_string(), ViewMode::Full),
            ],
            "in {mode:?}"
        );
    }
}

/// Exactly one width row is ticked, and it is the mode the document is in.
#[test]
fn the_current_mode_is_the_one_that_is_ticked() {
    for (mode, expected) in [
        (ViewMode::Page, "Page"),
        (ViewMode::Narrow, "Narrow"),
        (ViewMode::Full, "Full width"),
    ] {
        assert_eq!(ticked(mode), vec![expected], "in {mode:?}");
    }
}

/// The color row sits **under Page**, is always listed, and is **dead off
/// Page**. A setting of the row above it rather than a fourth width beside
/// them; and the canvas is a Page concept, so elsewhere there is nothing for it
/// to tint (ADR-44-page). Listed-and-dimmed rather than appearing, because a
/// row that comes and goes moves everything below it.
#[test]
fn the_color_row_sits_under_page_and_is_dead_off_it() {
    for mode in [ViewMode::Page, ViewMode::Narrow, ViewMode::Full] {
        assert_eq!(
            labels(mode),
            vec![
                "Page",
                "Color",
                "Narrow",
                "Full width",
                "Body",
                "Headings",
                "Code",
                "Language",
                "Focus mode",
            ],
            "the same rows in the same places, in {mode:?}"
        );
    }
    assert!((0..rows(ViewMode::Page, 0).len()).any(|i| is_color_row(ViewMode::Page, i)));
    for mode in [ViewMode::Narrow, ViewMode::Full] {
        assert!(
            !(0..rows(mode, 0).len()).any(|i| is_color_row(mode, i)),
            "and it does nothing in {mode:?}"
        );
        let color = rows(mode, 0)
            .into_iter()
            .find(|(r, _)| matches!(r, Row::Item { label, .. } if label.as_ref() == "Color"))
            .expect("still listed");
        assert!(!color.0.selectable(), "and cannot be pressed");
    }
}

/// Every width carries the chord that picks it, and the chords round-trip
/// through the letter the key map matches on.
#[test]
fn every_width_carries_a_chord_that_round_trips() {
    for (mode, _, id) in WIDTHS {
        // The chord comes from the registry now, so this asserts the *shape*
        // that matters — shift, and not Alt: `Ctrl+Alt+letter` is AltGr on
        // Windows and never arrives as the letter. See `WIDTHS`.
        let chord = koru_keys::registry::get(id).chord;
        assert!(chord.mods.shift_key(), "{id:?}");
        assert!(!chord.mods.alt_key(), "{id:?} must not be an AltGr chord");
        let koru_keys::On::Letter(letter) = chord.on else {
            panic!("{id:?} is not on a letter");
        };
        assert_eq!(mode_for_key(letter), Some(mode), "{id:?} selects {mode:?}");
        assert_eq!(key_for(mode), letter);
    }
    assert_eq!(mode_for_key('z'), None);
}

/// Focus mode is present, dead, and says so. It is the one row that advertises
/// something not built, which is only honest while it cannot be pressed.
#[test]
fn focus_mode_is_offered_but_disabled() {
    for mode in [ViewMode::Page, ViewMode::Narrow, ViewMode::Full] {
        let rows = rows(mode, 0);
        let (row, choice) = rows.last().expect("focus mode is always last");
        assert!(matches!(
            row,
            Row::Item { label, .. } if label.as_ref() == "Focus mode"
        ));
        assert!(!row.selectable(), "focus mode cannot be chosen yet");
        assert_eq!(*choice, None);
    }
}

/// There is a swatch color per preset, so the row's circles and the presets it
/// picks from cannot get out of step.
#[test]
fn there_is_one_swatch_color_per_preset() {
    assert_eq!(
        swatch_colors().len(),
        crate::docview::canvas_bg_presets().len()
    );
}

/// **The heading carries the settings icon at its right**, named *Global
/// settings* for a screen reader, and choosing it is
/// the one row that asks the host to leave the document: the Settings tab, on
/// its Editor page. The keyboard can reach it, because it is selectable.
#[test]
fn the_heading_links_to_the_global_settings() {
    for mode in [ViewMode::Page, ViewMode::Narrow, ViewMode::Full] {
        let rows = rows(mode, 0);
        let (row, choice) = &rows[0];
        assert_eq!(
            *row,
            Row::HeadingLink {
                heading: "PAGE SETTINGS",
                link: "Global settings",
            }
        );
        assert!(row.selectable(), "a click or Enter can land on the link");
        assert_eq!(*choice, Some(Choice::OpenGlobalSettings));
        assert_eq!(choice_at(mode, 0), Some(Choice::OpenGlobalSettings));
    }
}

/// A document in a window of its own has no Settings tab to open, so its
/// heading is plain and nothing on it can be chosen.
#[test]
fn a_document_without_a_settings_tab_gets_the_plain_heading() {
    let rows = super::rows(ViewMode::Page, 0, None, koru_i18n::Lang::En, false);
    assert_eq!(rows[0], (Row::Heading("PAGE SETTINGS"), None));
    assert!(!rows[0].0.selectable());
    assert_eq!(
        rows.len(),
        self::rows(ViewMode::Page, 0).len(),
        "the link moves no other row"
    );
}
