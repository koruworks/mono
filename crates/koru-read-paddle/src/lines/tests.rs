// SPDX-License-Identifier: MPL-2.0

use super::*;

fn region(left: f32, top: f32, right: f32, bottom: f32, text: &str, score: f32) -> Region {
    Region {
        left,
        top,
        right,
        bottom,
        text: text.into(),
        confidence: Some(score),
    }
}

/// The detector's order is not the reading order: lines come out top to
/// bottom, boxes on one line left to right.
#[test]
fn lines_are_read_top_to_bottom_and_left_to_right() {
    let recognized = assemble(vec![
        region(10.0, 60.0, 200.0, 80.0, "second line", 0.9),
        region(220.0, 12.0, 300.0, 30.0, "right", 0.9),
        region(10.0, 10.0, 200.0, 30.0, "first line", 0.9),
        region(10.0, 110.0, 200.0, 130.0, "third line", 0.9),
    ]);
    assert_eq!(
        recognized.text.as_str(),
        "first line right\nsecond line\nthird line"
    );
}

/// A box a little higher than its neighbour (a rotated photograph, a larger
/// word) is still on the same line; one a full line lower is not.
#[test]
fn a_box_within_half_a_line_height_joins_the_line() {
    let recognized = assemble(vec![
        region(10.0, 10.0, 100.0, 30.0, "Total", 0.9),
        region(110.0, 16.0, 200.0, 36.0, "1,284.50", 0.9),
        region(10.0, 31.0, 100.0, 51.0, "Tax", 0.9),
    ]);
    assert_eq!(recognized.text.as_str(), "Total 1,284.50\nTax");
}

#[test]
fn empty_lines_are_dropped_and_inner_white_space_is_one_space() {
    let recognized = assemble(vec![
        region(10.0, 10.0, 100.0, 30.0, "  ", 0.2),
        region(10.0, 40.0, 100.0, 60.0, "two\twords\n here", 0.8),
    ]);
    assert_eq!(recognized.text.as_str(), "two words here");
    assert_eq!(recognized.confidence, 80, "the blank box is not a line");
}

/// **Confidence is the mean over recognized lines**, as a whole percentage.
#[test]
fn confidence_is_the_mean_over_the_lines_rounded() {
    let recognized = assemble(vec![
        region(0.0, 0.0, 10.0, 10.0, "a", 0.90),
        region(0.0, 40.0, 10.0, 50.0, "b", 0.70),
        region(0.0, 80.0, 10.0, 90.0, "c", 0.99),
    ]);
    // (0.90 + 0.70 + 0.99) / 3 = 0.8633 → 86
    assert_eq!(recognized.confidence, 86);
    let mut unscored = region(0.0, 0.0, 10.0, 10.0, "d", 0.0);
    unscored.confidence = None;
    assert_eq!(mean_confidence(&[unscored]), 0);
    assert_eq!(
        mean_confidence(&[region(0.0, 0.0, 1.0, 1.0, "e", 1.7)]),
        100
    );
}

#[test]
fn no_lines_is_no_words_and_no_confidence() {
    let recognized = assemble(Vec::new());
    assert!(recognized.text.is_empty());
    assert_eq!(recognized.confidence, 0);
}

#[test]
fn a_box_with_a_coordinate_that_is_not_a_number_is_ignored() {
    let recognized = assemble(vec![
        region(f32::NAN, 0.0, 10.0, 10.0, "bad", 0.1),
        region(0.0, 0.0, 10.0, 10.0, "good", 0.9),
    ]);
    assert_eq!(recognized.text.as_str(), "good");
    assert_eq!(recognized.confidence, 90);
}
