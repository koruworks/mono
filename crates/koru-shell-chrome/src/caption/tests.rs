// SPDX-License-Identifier: MPL-2.0

//! The caption's furniture never overlaps, and never leaves the strip.

use super::*;
use koru_ui::caption::{Action, controls};

fn caption() -> Rect {
    Rect::new(0.0, 0.0, 1200.0, theme::CAPTION_H)
}

#[test]
fn the_furniture_runs_left_to_right_without_overlapping() {
    for buttons in [false, true] {
        let c = caption();
        let left = sidebar_toggle(c);
        let strip = tab_strip(c, buttons, true);
        let right = context_toggle(c, buttons, true);

        assert_eq!(left.x, c.x, "the sidebar toggle is leftmost");
        assert!(left.x + left.w <= strip.x, "toggle then strip ({buttons})");
        assert!(
            strip.x + strip.w <= right.x,
            "strip then toggle ({buttons})"
        );
        assert!(
            right.x + right.w <= c.x + c.w,
            "nothing leaves the caption ({buttons})"
        );
    }
}

/// The four window controls take the right end, so the context toggle steps
/// inside them rather than sitting underneath.
#[test]
fn the_context_toggle_makes_room_for_the_window_buttons() {
    let c = caption();
    let without = context_toggle(c, false, true);
    let with = context_toggle(c, true, true);
    assert!(with.x < without.x);
    assert_eq!(without.x - with.x, theme::CAPTION_BTN * 4.0);
    for control in koru_ui::caption::controls(c) {
        assert!(
            with.x + with.w <= control.rect.x,
            "no overlap with a window button"
        );
    }
}

#[test]
fn a_host_action_sits_left_of_context() {
    let c = caption();
    let action = host_action_with_context(c, true, true, true);
    let context = context_toggle_with_host_action(c, true, true, true);
    let overflow = controls(c)
        .into_iter()
        .find(|control| control.action == Action::WindowMenu)
        .expect("an overflow button")
        .rect;

    assert_eq!(context.x + context.w, overflow.x);
    assert_eq!(action.x + action.w, context.x);
    assert_eq!(
        tab_strip(c, true, true).w - tab_strip_with_host_action(c, true, true, true).w,
        action.w,
    );
}

#[test]
fn the_window_buttons_are_in_order_and_close_owns_the_corner() {
    let c = caption();
    let b = controls(c);
    assert_eq!(
        b.map(|control| control.action),
        [
            Action::WindowMenu,
            Action::Minimise,
            Action::ToggleMaximise,
            Action::Close,
        ]
    );
    assert!(b.windows(2).all(|w| w[0].rect.x < w[1].rect.x));
    let dr = b[3].rect;
    assert_eq!(dr.x + dr.w, c.x + c.w, "close ends at the window's edge");
    assert_eq!(dr.h, c.h, "and runs the full height, so the corner is live");
}

/// Overflow occupies the former pin slot while the direct window controls keep
/// their familiar positions.
#[test]
fn the_window_menu_replaces_pin_without_replacing_window_controls() {
    let c = caption();
    let b = controls(c);
    assert_eq!(b.len(), 4);
    let first = b[0];
    let r = first.rect;
    assert_eq!(first.action, Action::WindowMenu);
    assert_eq!(r.x, c.x + c.w - theme::CAPTION_BTN * 4.0);
    assert_eq!(r.h, c.h);
    assert_eq!(r.w, theme::CAPTION_BTN);
    assert_eq!(b.last().map(|control| control.action), Some(Action::Close));
}

/// Pin is gone rather than left behind as a zero-width hit target.
#[test]
fn the_replaced_pin_button_is_absent_rather_than_zero_width() {
    let b = controls(caption());
    assert_eq!(b.len(), 4, "pin is not a fifth caption control");
    assert!(b.iter().all(|control| control.rect.w > 0.0));
}

/// Custom window controls reserve exactly four caption-button widths.
#[test]
fn the_window_controls_reserve_four_buttons_of_room() {
    let c = caption();
    let without = tab_strip(c, false, false);
    let with = tab_strip(c, true, false);
    assert_eq!(
        without.w - with.w,
        theme::CAPTION_BTN * 4.0,
        "the strip yields exactly the space the controls occupy"
    );
    assert_eq!(without.x, with.x);
}

/// A window dragged narrow must not produce a strip of negative width, which
/// would make `tabs::layout` return nonsense rather than nothing.
#[test]
fn a_narrow_caption_yields_an_empty_strip_not_a_negative_one() {
    for w in [0.0, 1.0, 40.0, theme::CAPTION_H * 2.0] {
        let c = Rect::new(0.0, 0.0, w, theme::CAPTION_H);
        for buttons in [false, true] {
            let strip = tab_strip(c, buttons, true);
            assert!(strip.w >= 0.0, "w={w} buttons={buttons}");
            assert!(strip.w.is_finite());
            assert!(sidebar_toggle(c).w <= w.max(0.0));
        }
    }
}

/// Every rect here derives from the caption's own `x`, which is `sidebar_w`
/// rather than zero once the pane is open. Nothing in this file may assume the
/// caption starts at the window's left edge.
#[test]
fn the_furniture_is_relative_to_the_caption_wherever_it_starts() {
    let shifted = Rect::new(
        theme::SIDEBAR_W,
        0.0,
        1200.0 - theme::SIDEBAR_W,
        theme::CAPTION_H,
    );
    for buttons in [false, true] {
        let left = sidebar_toggle(shifted);
        let strip = tab_strip(shifted, buttons, true);
        let right = context_toggle(shifted, buttons, true);

        assert_eq!(left.x, theme::SIDEBAR_W, "the logo moved with the caption");
        assert!(left.x + left.w <= strip.x);
        assert!(strip.x + strip.w <= right.x);
        assert!(right.x + right.w <= shifted.x + shifted.w);
        // The window buttons stay pinned to the far edge, which has not
        // moved.
        let controls = koru_ui::caption::controls(shifted);
        for control in controls {
            assert!(control.rect.x + control.rect.w <= shifted.x + shifted.w);
        }
        let last = controls.last().expect("there is always at least one").rect;
        assert_eq!(last.x + last.w, 1200.0, "close still owns the corner");
    }
}

/// Hidden, the toggle is a **zero-width rect** rather than an absence — so the
/// tab strip does the same arithmetic and takes the space back, instead of
/// leaving a 36px hole before the window buttons.
#[test]
fn a_hidden_context_toggle_gives_its_room_to_the_tabs() {
    for buttons in [false, true] {
        let c = caption();
        let shown = context_toggle(c, buttons, true);
        let hidden = context_toggle(c, buttons, false);

        assert!(shown.w > 0.0);
        assert_eq!(hidden.w, 0.0, "hidden is zero-width, not absent");
        assert_eq!(
            hidden.x,
            shown.x + shown.w,
            "and it collapses to its own right edge"
        );

        let wide = tab_strip(c, buttons, false);
        let narrow = tab_strip(c, buttons, true);
        assert_eq!(wide.w - narrow.w, shown.w, "the tabs got exactly that room");
        assert_eq!(
            wide.x + wide.w,
            hidden.x,
            "and the strip now runs to where the toggle was"
        );
        // Whatever else moved, nothing leaves the caption.
        assert!(wide.x + wide.w <= c.x + c.w);
    }
}

/// **The whole trailing chain, in order, with every slot offered at once.**
///
/// `trailing_edge` is the anchor and each rect takes its right edge from its
/// neighbour's `.x`; the point of the test is that adding the banner's Settings
/// slot to the chain moved everything left of it and nothing to its right.
#[test]
fn the_trailing_furniture_is_one_chain_from_the_window_buttons_inward() {
    let c = caption();
    let slots = banner::Slots {
        profile: true,
        workspace: banner::Workspace::Named(90.0),
        settings: true,
    };
    let overflow = controls(c)
        .into_iter()
        .find(|control| control.action == Action::WindowMenu)
        .expect("an overflow button")
        .rect;

    let settings = banner::settings_slot(c, true, slots);
    let context = context_toggle_with_banner(c, true, true, slots);
    let action = host_action_with_banner(c, true, true, true, slots);
    let lane = tab_strip_with_banner(c, true, true, true, slots);

    assert_eq!(trailing_edge(c, true), overflow.x);
    assert_eq!(settings.x + settings.w, overflow.x);
    assert_eq!(context.x + context.w, settings.x);
    assert_eq!(action.x + action.w, context.x);
    assert_eq!(lane.x + lane.w, action.x);
    assert_eq!(lane.x, banner::end(c, slots), "and it begins at the banner");
}

/// A window that offers no banner is untouched by any of it: `Slots::NONE`
/// collapses the chain back onto the window buttons, which is what lets
/// `apps/koru` keep calling the four-argument overloads.
#[test]
fn no_banner_leaves_every_trailing_rect_where_it_was() {
    for buttons in [false, true] {
        for context in [false, true] {
            let c = caption();
            assert_eq!(
                context_toggle_with_banner(c, buttons, context, banner::Slots::NONE),
                context_toggle(c, buttons, context),
            );
            assert_eq!(
                host_action_with_banner(c, buttons, context, true, banner::Slots::NONE),
                host_action_with_context(c, buttons, context, true),
            );
            assert_eq!(
                tab_strip_with_banner(c, buttons, context, true, banner::Slots::NONE),
                tab_strip_with_host_action(c, buttons, context, true),
            );
        }
    }
}
