// SPDX-License-Identifier: MPL-2.0

use super::*;

/// **The air is the same on both sides** of a regular chip with a mark: the
/// label's left edge sits the padding in from the left border and the mark's
/// box ends the padding in from the right one — the asymmetry the owner saw
/// on the Tasks page's filters (round nine, finding 45), held here for every
/// chip now.
#[test]
fn a_regular_chip_has_the_same_air_at_both_ends() {
    let mut engine = TextEngine::new();
    for trail in [Trail::Caret, Trail::Clear] {
        let chip = LabelChip::new("Any date").size(Size::Regular).trail(trail);
        let rect = Rect::new(20.0, 8.0, chip.width(&mut engine), chip.height());
        let mark = chip.trail_rect(rect).expect("a mark takes room");
        let right = (rect.x + rect.w) - (mark.x + mark.w);
        assert_eq!(right, REGULAR_PAD_X, "{trail:?}: one inset, both ends");
        assert!(
            mark.x > rect.x + REGULAR_PAD_X,
            "the mark is past the label"
        );
    }
}

#[test]
fn a_regular_context_chips_remove_mark_has_matching_edge_padding() {
    let mut engine = TextEngine::new();
    let chip = LabelChip::new("headshot-reddit.jpg")
        .size(Size::Regular)
        .lead(Lead::Glyph(icons::IMAGE))
        .trail(Trail::Remove);
    let plain = chip.trail(Trail::None);
    let rect = Rect::new(20.0, 8.0, chip.width(&mut engine), chip.height());
    let lane = chip.trail_rect(rect).expect("remove target");
    let x = chip.remove_mark_x(rect, lane);

    assert_eq!(
        chip.width(&mut engine),
        plain.width(&mut engine) + CHIP_CLOSE_W
    );
    assert_eq!(rect.x + rect.w - (x + X_ARM), REGULAR_PAD_X);
    assert!(x - X_ARM >= lane.x, "all of the × is clickable");
    assert!(x + X_ARM <= lane.x + lane.w, "all of the × is clickable");
    assert_eq!(lane.h, rect.h);
}

/// **A chip that cannot be removed carries no remove lane** — the Tasks
/// page's row chips read 6 px on the left and 21 px on the right while every
/// chip reserved one. Its width is the label and its padding, and nothing
/// else.
#[test]
fn a_compact_chip_without_a_remove_lane_is_symmetric() {
    let mut engine = TextEngine::new();
    let label = engine.measure_attr_chip(ChipKind::Tag, "Next Milestones", None, None, false);
    let plain = LabelChip::new("Next Milestones");
    assert_eq!(
        plain.width(&mut engine),
        label,
        "label + padding both sides"
    );
    assert!(
        plain
            .trail_rect(Rect::new(0.0, 0.0, label, CHIP_H))
            .is_none()
    );

    let removable = plain.trail(Trail::Remove);
    let w = removable.width(&mut engine);
    assert_eq!(w, label + CHIP_CLOSE_W, "the lane, where it can be removed");
    let lane = removable
        .trail_rect(Rect::new(10.0, 0.0, w, CHIP_H))
        .expect("a removable chip has its lane");
    assert_eq!(lane.x + lane.w, 10.0 + w, "it ends where the chip ends");
    assert_eq!(lane.h, CHIP_H, "the whole height is the target");
}

/// The two always-inked marks cost the same, so switching a selector's caret
/// for a filter's `×` does not shuffle the chips beside it.
#[test]
fn a_mark_costs_width_and_both_marks_cost_the_same() {
    let mut engine = TextEngine::new();
    let chip = LabelChip::new("Open").size(Size::Regular);
    let plain = chip.width(&mut engine);
    let caret = chip.trail(Trail::Caret).width(&mut engine);
    let clear = chip.trail(Trail::Clear).width(&mut engine);
    assert_eq!(caret, clear);
    assert_eq!(caret - plain, REGULAR_GAP + MARK);
}

/// A lead is width: the status dot's and a glyph's lanes are inside the
/// lozenge, so a chip is pressable exactly where it is drawn.
#[test]
fn a_lead_is_inside_the_width() {
    let mut engine = TextEngine::new();
    let chip = LabelChip::new("Updating").size(Size::Regular);
    let plain = chip.width(&mut engine);
    assert_eq!(
        chip.lead(Lead::Dot).width(&mut engine) - plain,
        DOT_D + REGULAR_GAP
    );
    let compact = LabelChip::new("Wed");
    let led = compact.lead(Lead::Glyph(icons::CLOCK)).width(&mut engine);
    assert!(led > compact.width(&mut engine));
}

/// A swatch is a dot, and its lane follows the same rule as a lozenge's.
#[test]
fn a_swatch_is_a_dot_with_a_lane_only_where_it_can_be_removed() {
    let mut engine = TextEngine::new();
    let dot = LabelChip::swatch(Some("red"));
    assert_eq!(dot.width(&mut engine), SWATCH_D);
    assert_eq!(
        dot.trail(Trail::Remove).width(&mut engine),
        SWATCH_D + CHIP_CLOSE_W
    );
    assert_eq!(
        LabelChip::attribute(ChipKind::Swatch, "", Some("red")),
        dot,
        "the attribute constructor answers the same swatch"
    );
}

/// The regular chip's height is its role's line plus the air above and
/// below, and the mark is centred in it.
#[test]
fn a_regular_chip_is_its_line_plus_air_and_its_mark_is_centred() {
    let chip = LabelChip::new("Open")
        .size(Size::Regular)
        .trail(Trail::Caret);
    assert_eq!(
        chip.height(),
        theme::role::hint().metrics.line_height + REGULAR_PAD_Y * 2.0
    );
    let rect = Rect::new(0.0, 30.0, 120.0, chip.height());
    let mark = chip.trail_rect(rect).expect("a mark");
    let above = mark.y - rect.y;
    let below = (rect.y + rect.h) - (mark.y + mark.h);
    assert!((above - below).abs() < 0.01);
}

#[test]
fn a_context_chips_second_line_changes_geometry_without_moving_its_remove_target() {
    let mut engine = TextEngine::new();
    let plain = LabelChip::new("Budget")
        .size(Size::Regular)
        .trail(Trail::Remove);
    let context = plain.secondary("Planning workspace");
    assert!(context.height() > plain.height());
    assert!(context.width(&mut engine) > plain.width(&mut engine));
    let rect = Rect::new(0.0, 0.0, context.width(&mut engine), context.height());
    let lane = context.trail_rect(rect).expect("remove lane");
    assert_eq!(lane.x + lane.w, rect.w);
    assert_eq!(lane.h, rect.h);
}

#[test]
fn the_tones_are_the_neutral_the_accent_and_the_inverted_pairs() {
    let p = theme::p();
    assert_eq!(Tone::Neutral.colors(&p), (p.chip_bg, p.text_dim));
    assert_eq!(Tone::Inverted.colors(&p), (p.bubble_bg, p.bubble_fg));
    assert_eq!(Tone::Accent("red").colors(&p), chip_colors(Some("red")));
    assert_eq!(
        Tone::Accent("not-a-token").colors(&p),
        (p.chip_bg, p.text_dim),
        "an unknown token reads as neutral"
    );
}
