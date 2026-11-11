// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::fixtures;

#[test]
fn utf8_with_or_without_a_mark_decodes_as_it_is() {
    assert_eq!(decode("Grüße, 東京".as_bytes()), "Grüße, 東京");
    assert_eq!(decode(b"\xEF\xBB\xBFmarked"), "marked");
}

#[test]
fn utf16_in_either_order_decodes() {
    let little: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain("Zoë".encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    let big: Vec<u8> = [0xFE, 0xFF]
        .into_iter()
        .chain("Zoë".encode_utf16().flat_map(u16::to_be_bytes))
        .collect();
    assert_eq!(decode(&little), "Zoë");
    assert_eq!(decode(&big), "Zoë");
}

/// A file that is not UTF-8 is almost always Windows-1252 here.
#[test]
fn bytes_that_are_not_utf8_are_windows_1252() {
    assert_eq!(decode(b"caf\xE9 \x93quoted\x94 \x80"), "café “quoted” €");
}

/// The read limit can cut a character in half; the text before it survives.
#[test]
fn a_character_cut_by_the_read_limit_is_dropped_not_misread() {
    let mut bytes = "naïve ü".as_bytes().to_vec();
    bytes.pop();
    assert_eq!(decode(&bytes), "naïve ");
}

#[test]
fn line_endings_become_newlines_and_control_characters_go() {
    assert_eq!(
        decode(b"one\r\ntwo\rthree\n\tfour\x00\x07"),
        "one\ntwo\nthree\n\tfour"
    );
}

/// **HTML without its markup**: no tag, attribute, script, style or comment
/// survives, entities are decoded, and blocks end lines.
#[test]
fn html_reads_as_its_text() {
    let text = strip_html(fixtures::HTML);
    assert_eq!(text, fixtures::HTML_TEXT);
    for hidden in [
        "scriptword",
        "commentword",
        "hrefword",
        "attrword",
        "hidden",
        "class",
        "utf-8",
        "DOCTYPE",
    ] {
        assert!(!text.contains(hidden), "{hidden} leaked into the text");
    }
}

#[test]
fn html_that_is_not_well_formed_is_still_read() {
    assert_eq!(strip_html("a < b and c > d"), "a < b and c > d");
    assert_eq!(strip_html("<p>unclosed <b>bold"), "unclosed bold");
    assert_eq!(
        strip_html("fish &chips; &amp &#xZZ; &#65;"),
        "fish &chips; &amp \u{FFFD} A"
    );
    assert_eq!(strip_html("<SCRIPT>hidden()</SCRIPT >after"), "after");
    assert_eq!(strip_html("<script>never closed"), "");
    assert_eq!(strip_html("before<!-- never closed"), "before");
    assert_eq!(strip_html("<div title=\"a > b\">kept</div>"), "kept");
    assert_eq!(strip_html("<![CDATA[raw <text>]]>"), "raw <text>");
}

/// A `<scripted>` element is not a `<script>`.
#[test]
fn only_the_named_element_is_skipped() {
    assert_eq!(strip_html("<scripted>shown</scripted>"), "shown");
    assert_eq!(
        strip_html("<style>a{}</style-x>still style</style>shown"),
        "shown"
    );
}

#[test]
fn truncation_cuts_at_a_word_and_says_so() {
    let (kept, cut) = truncate("alpha beta gamma".into(), 12);
    assert_eq!((kept.as_str(), cut), ("alpha beta", true));
    let (kept, cut) = truncate("short".into(), 12);
    assert_eq!((kept.as_str(), cut), ("short", false));
    // Never inside a character, even with no space to cut at.
    let (kept, cut) = truncate("ééééé".into(), 5);
    assert_eq!((kept.as_str(), cut), ("éé", true));
}

#[test]
fn the_caps_fit_the_frame() {
    // A page crosses one koru-ipc frame (16 MiB). Text with every byte a
    // quote doubles in JSON; the cap leaves room for that.
    const { assert!(PLAIN_TEXT_MAX_BYTES * 2 < 16 * 1024 * 1024) };
    assert_eq!(PLAIN_FILE_MAX_BYTES, 4 * PLAIN_TEXT_MAX_BYTES as u64);
}
