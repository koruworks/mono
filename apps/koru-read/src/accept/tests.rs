// SPDX-License-Identifier: MPL-2.0

use super::*;

fn layer(chars: usize, unmapped: usize, picture_share: f64) -> Layer {
    Layer {
        text: "x".repeat(chars),
        chars,
        unmapped,
        picture_share,
    }
}

#[test]
fn a_page_of_mapped_text_is_taken() {
    assert_eq!(judge(&layer(500, 0, 0.0)), Verdict::Text);
    // A full-page picture under plenty of real text: the words are the text
    // layer's (a scan someone already ran OCR on, or a photo with a caption).
    assert_eq!(judge(&layer(500, 0, 1.0)), Verdict::Text);
}

#[test]
fn a_mostly_unmapped_layer_goes_to_the_reader() {
    assert_eq!(judge(&layer(20, 80, 0.0)), Verdict::NeedsReader);
    assert_eq!(judge(&layer(0, 40, 0.0)), Verdict::NeedsReader);
}

/// The measured rule: at most ten percent unmapped.
#[test]
fn up_to_a_tenth_unmapped_is_still_text() {
    assert_eq!(judge(&layer(400, 3, 0.0)), Verdict::Text);
    assert_eq!(judge(&layer(90, 10, 0.0)), Verdict::Text, "exactly a tenth");
    assert_eq!(
        judge(&layer(90, 11, 0.0)),
        Verdict::NeedsReader,
        "just over"
    );
}

/// Twenty characters is the line.
#[test]
fn twenty_characters_make_a_text_layer_over_a_picture() {
    assert_eq!(judge(&layer(20, 0, 1.0)), Verdict::Text);
    assert_eq!(judge(&layer(19, 0, 1.0)), Verdict::NeedsReader);
}

#[test]
fn a_scan_with_a_page_number_goes_to_the_reader() {
    assert_eq!(judge(&layer(2, 0, 0.9)), Verdict::NeedsReader);
    assert_eq!(judge(&layer(0, 0, 0.9)), Verdict::NeedsReader);
}

/// A blank page, and a page whose only content is a short line beside a small
/// logo, are what they are: they do not make a text PDF wait for a download.
#[test]
fn a_short_page_without_a_picture_is_taken_as_it_is() {
    assert_eq!(judge(&layer(0, 0, 0.0)), Verdict::Nothing);
    assert_eq!(judge(&layer(3, 0, 0.0)), Verdict::Text);
    assert_eq!(judge(&layer(19, 0, 0.02)), Verdict::Text);
    assert_eq!(
        judge(&layer(1, 2, 0.0)),
        Verdict::NeedsReader,
        "mostly unmapped"
    );
}

/// The thresholds are pinned so a change is a decision someone made, not a
/// drift: the first two are Plan 0.1's measurements.
#[test]
fn the_thresholds_are_pinned() {
    assert_eq!(MIN_TEXT_CHARS, 20);
    assert_eq!(MAX_UNMAPPED_PERMILLE, 100);
    assert!((MIN_PICTURE_SHARE - 0.10).abs() < f64::EPSILON);
}
