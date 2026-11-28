// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn hex_color_accepts_six_and_three_digit_forms() {
    assert_eq!(parse_hex_color("#0C0C0C").unwrap(), [12, 12, 12]);
    assert_eq!(parse_hex_color("0c0c0c").unwrap(), [12, 12, 12]);
    assert_eq!(parse_hex_color(" #1F2329 ").unwrap(), [31, 35, 41]);
    assert_eq!(parse_hex_color("#000").unwrap(), [0, 0, 0]);
    assert_eq!(parse_hex_color("36c").unwrap(), [0x33, 0x66, 0xCC]);
    assert_eq!(format_hex_color([12, 12, 12]), "#0C0C0C");
}

#[test]
fn invalid_hex_colors_are_rejected() {
    for bad in ["", "#", "#12", "#1234", "#12345", "#1234567", "#GGGGGG", "black"] {
        assert!(parse_hex_color(bad).is_err(), "{bad}");
    }
}

#[test]
fn large_paste_with_multibyte_chars_splits_safely_without_accessibility_panic() {
    let fonts = parley::FontContext::new();
    let mut page = Page::new(&fonts);

    let mut large = "a".repeat(4579);
    large.push('·');
    large.push_str(&"b".repeat(70_000));
    large.push_str("\r\nsecond · paragraph\r\nthird");
    page.paste_text(&large);

    assert!(page.blocks.iter().all(|b| b.editor.raw_text_len() <= MAX_BLOCK_BYTES));
    let mut update = accesskit::TreeUpdate {
        nodes: vec![],
        tree: None,
        tree_id: accesskit::TreeId::ROOT,
        focus: accesskit::NodeId(0),
    };
    let mut root = accesskit::Node::new(accesskit::Role::Document);
    page.accessibility(&mut update, &mut root);
    assert!(!update.nodes.is_empty());
}

#[test]
fn paste_hex_color_in_edit_mode_previews_and_commits() {
    let fonts = parley::FontContext::new();
    let mut page = Page::new(&fonts);
    page.begin_color_edit();
    page.paste_into_color_edit(" #0c0c0c \r\n");
    assert_eq!(page.text_color, [12, 12, 12]);
    assert!(page.commit_color_edit());
    assert!(!page.editing_color());
    assert_eq!(page.text_color, [12, 12, 12]);
}
