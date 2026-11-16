// SPDX-License-Identifier: MPL-2.0

use super::*;

const LAYER: &str = "Quarterly report\nThe invoice is due on the fifteenth of March.";

/// **Recognition reads the typed text too**: none of it is added again,
/// however it was wrapped, spaced or capitalised.
#[test]
fn lines_the_layer_already_holds_are_not_added_twice() {
    let recognized = "QUARTERLY REPORT\nThe invoice is due on the\nfifteenth of March.";
    assert_eq!(combine(LAYER, recognized), LAYER);
}

/// The words only the picture holds are added, after the layer, as lines.
#[test]
fn lines_only_the_pictures_hold_are_added_after_the_layer() {
    let recognized = "Quarterly report\nSettings\nStorage used: 3.2 GB of 15 GB";
    assert_eq!(
        combine(LAYER, recognized),
        format!("{LAYER}\nSettings\nStorage used: 3.2 GB of 15 GB")
    );
}

/// A different word is retained: similarity alone cannot prove a misreading.
#[test]
fn reading_diagnostics_keeps_a_single_different_word() {
    let recognized = "The invoice is due on the fifteenth of Narch.";
    assert_eq!(combine(LAYER, recognized), format!("{LAYER}\n{recognized}"));
}

#[test]
fn reading_diagnostics_preserves_new_names_numbers_and_word_boundaries() {
    for (layer, recognized) in [
        (
            "The invoice is due on day 15",
            "The invoice is due on day 16",
        ),
        (
            "The contact for this request is Alice",
            "The contact for this request is Bob",
        ),
        ("Annual report", "Ann"),
        ("one two three", "three two one"),
    ] {
        assert_eq!(combine(layer, recognized), format!("{layer}\n{recognized}"));
    }
}

/// A line that is mostly new words is added, even if it shares a few.
#[test]
fn a_line_of_mostly_new_words_is_added() {
    let recognized = "The warranty covers repairs for two years";
    assert_eq!(combine(LAYER, recognized), format!("{LAYER}\n{recognized}"));
}

#[test]
fn a_line_recognized_twice_is_added_once_and_blank_lines_are_dropped() {
    let recognized = "Closed on Sundays\n\n  \nclosed on sundays";
    assert_eq!(
        combine(LAYER, recognized),
        format!("{LAYER}\nClosed on Sundays")
    );
}

/// Writing without spaces: the comparison is by the whole line.
#[test]
fn text_without_spaces_compares_by_the_line() {
    let layer = "今天天气很好，我们去公园散步。";
    assert_eq!(combine(layer, "今天天气很好，我们去公园散步。"), layer);
    assert_eq!(
        combine(layer, "会议在下午三点开始。"),
        format!("{layer}\n会议在下午三点开始。")
    );
}

#[test]
fn nothing_recognized_is_the_layer_and_nothing_layered_is_the_recognition() {
    assert_eq!(combine(LAYER, ""), LAYER);
    assert_eq!(combine("", "a picture's words"), "a picture's words");
}
