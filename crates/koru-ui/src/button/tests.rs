// SPDX-License-Identifier: MPL-2.0

//! Geometry only. What a button *looks* like needs eyes; where it *is* does not.

use super::*;

#[test]
fn a_row_is_laid_out_from_the_right() {
    let rects = row_from_right(400.0, 100.0, &[90.0, 120.0]);
    assert_eq!(rects.len(), 2);
    // The last one owns the corner, whatever the others turned out to be.
    assert_eq!(rects[1].x + rects[1].w, 400.0);
    assert_eq!(rects[1].w, 120.0);
    // And the first sits a gap to its left.
    assert_eq!(rects[0].x + rects[0].w + GAP, rects[1].x);
    assert_eq!(rects[0].w, 90.0);
    for r in &rects {
        assert_eq!(r.y, 100.0);
        assert_eq!(r.h, H);
    }
}

/// Widening one button moves the *others* left and leaves the corner where it
/// was. That is the whole point of laying out from the right: a long label in
/// another language must not push the primary answer off the panel.
#[test]
fn a_longer_label_grows_leftwards() {
    let narrow = row_from_right(400.0, 0.0, &[90.0, 100.0]);
    let wide = row_from_right(400.0, 0.0, &[90.0, 200.0]);
    assert_eq!(
        narrow[1].x + narrow[1].w,
        wide[1].x + wide[1].w,
        "the corner is fixed"
    );
    assert!(wide[0].x < narrow[0].x, "everything else moved left");
}

#[test]
fn one_button_still_owns_the_corner() {
    let rects = row_from_right(300.0, 0.0, &[88.0]);
    assert_eq!(rects.len(), 1);
    assert_eq!(rects[0].x + rects[0].w, 300.0);
}

#[test]
fn no_buttons_is_no_rects_rather_than_a_panic() {
    assert!(row_from_right(300.0, 0.0, &[]).is_empty());
}

/// Secondary is the default because a confirmation dialog's safe answer is the
/// one that should be reachable without thinking about it.
#[test]
fn the_default_role_is_the_safe_one() {
    assert_eq!(ButtonRole::default(), ButtonRole::Secondary);
}

#[test]
fn compact_actions_measure_label_padding_and_optional_icon() {
    let mut engine = TextEngine::new();
    let plain = ActionButton::new("Replace", ActionStyle::Ghost, None).width(&mut engine);
    let icon = ActionButton::new(
        "Replace",
        ActionStyle::Primary,
        Some(koru_draw::icons::ENTER),
    )
    .width(&mut engine);
    assert!(icon > plain + ActionButton::GAP);
    assert_eq!(ActionButton::PAD_X, 6.0);
    assert_eq!(ActionButton::PAD_Y, 4.0);
    assert_eq!(
        ActionButton::H,
        theme::text_metrics::SM.line_height + ActionButton::PAD_Y * 2.0
    );
    assert!((ActionButton::ICON_SIZE - 14.0 * 0.90).abs() < 0.001);
}

#[test]
fn primary_action_hover_is_a_real_darken() {
    let colour = vello::peniko::Color::new([0.8, 0.6, 0.4, 1.0]);
    let hover = darkened(colour, 0.90);
    assert!(hover.components[0] < colour.components[0]);
    assert!(hover.components[1] < colour.components[1]);
    assert!(hover.components[2] < colour.components[2]);
    assert_eq!(hover.components[3], colour.components[3]);
}

#[test]
fn inverted_button_uses_foreground_ink_and_reports_its_token() {
    let mut engine = TextEngine::new();
    let mut canvas = Scene::new();
    let root = Affine::IDENTITY;
    let rect = Rect::new(0.0, 0.0, 200.0, 42.0);
    let capture = koru_draw::inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, 200.0, 42.0));
    draw_button_enabled(
        &mut canvas,
        root,
        &mut engine,
        rect,
        "Start using Koru",
        ButtonRole::Inverted,
        true,
        false,
        true,
    );
    let records = capture.finish().records;
    let button = records
        .iter()
        .find(|r| r.name == "KoruButton")
        .expect("KoruButton record");
    assert!(
        button
            .properties
            .iter()
            .any(|p| p.name == "background token" && p.value == "theme::p().text")
    );
}
