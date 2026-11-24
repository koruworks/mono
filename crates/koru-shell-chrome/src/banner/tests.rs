// SPDX-License-Identifier: MPL-2.0

//! The banner promises exactly what the caption module does: drawing and
//! hit-testing share one function per rect, so every slot hit-tests back to
//! itself and a slot that is not offered takes no space and no press.
//!
//! And one promise of its own: with no banner offered, every rect in the caption
//! is what it was before the banner existed. That is the regression the whole
//! change rests on, and `no_banner_leaves_the_lane_exactly_where_it_was` is it.

use super::*;

fn a_caption() -> Rect {
    // The caption as the container cuts it: starting after an open sidebar,
    // because nothing in the banner may assume it owns the window's left edge.
    Rect::new(240.0, 0.0, 960.0, theme::CAPTION_H)
}

/// Everything offered, with a measured name.
fn full(label_w: f32) -> Slots {
    Slots {
        profile: true,
        workspace: Workspace::Named(label_w),
        settings: true,
    }
}

#[test]
fn every_slot_hit_tests_back_to_itself() {
    let caption = a_caption();
    let slots = full(90.0);
    for (slot, expected) in [
        (profile_slot(caption, slots), Hit::Profile),
        (workspace_slot(caption, slots), Hit::Workspace),
        (settings_slot(caption, true, slots), Hit::Settings),
    ] {
        assert!(slot.w > 0.0, "{expected:?} is offered, so it has width");
        let (x, y) = slot.centre();
        assert_eq!(hit(caption, true, slots, x, y), Some(expected));
    }
}

#[test]
fn a_slot_not_offered_takes_no_space_and_no_press() {
    let caption = a_caption();
    // Workspace leads when the profile is hidden…
    let ws = workspace_slot(
        caption,
        Slots {
            profile: false,
            workspace: Workspace::Named(90.0),
            settings: false,
        },
    );
    assert_eq!(
        ws.x,
        caption::sidebar_toggle(caption).x + caption::sidebar_toggle(caption).w,
        "a hidden profile slot cedes its space"
    );
    // …and a hidden slot swallows nothing: the press falls through to whatever
    // the caption says is there (here, nothing of the banner's).
    let (x, y) = profile_slot(caption, full(90.0)).centre();
    assert_eq!(hit(caption, true, Slots::NONE, x, y), None);
}

#[test]
fn the_workspace_name_is_clamped_by_the_same_arithmetic_everywhere() {
    let caption = a_caption();
    let slots = full(10_000.0);
    let slot = workspace_slot(caption, slots);
    assert_eq!(
        slot.w, WORKSPACE_MAX_W,
        "a novel of a name stops at the max"
    );
    // The far edge of the clamped slot still answers Workspace: hit and draw
    // agreed about where the name stops.
    assert_eq!(
        hit(caption, true, slots, slot.x + slot.w - 1.0, slot.centre().1,),
        Some(Hit::Workspace)
    );
}

/// Ladder step 2: the name goes and a square carrying the workspace's own icon
/// stays. It is the same width as the profile slot beside it, because both hold
/// one glyph.
#[test]
fn an_icon_only_workspace_is_a_square_and_still_hits() {
    let caption = a_caption();
    let slots = Slots {
        profile: true,
        workspace: Workspace::IconOnly,
        settings: true,
    };
    let ws = workspace_slot(caption, slots);
    assert_eq!(ws.w, profile_slot(caption, slots).w);
    let (x, y) = ws.centre();
    assert_eq!(hit(caption, true, slots, x, y), Some(Hit::Workspace));
}

#[test]
fn end_is_where_the_leading_slots_stop() {
    let caption = a_caption();
    let slots = full(90.0);
    let ws = workspace_slot(caption, slots);
    assert_eq!(end(caption, slots), ws.x + ws.w);
    // With nothing offered, the banner takes nothing: the strip may begin at
    // the sidebar toggle's far edge.
    let toggle = caption::sidebar_toggle(caption);
    assert_eq!(end(caption, Slots::NONE), toggle.x + toggle.w);
}

/// **The regression the whole change rests on.** Every window that shows no
/// banner — which is every window today — gets the geometry it always had, from
/// the same functions its callers already use.
#[test]
fn no_banner_leaves_the_lane_exactly_where_it_was() {
    for width in [500.0, 720.0, 960.0, 1600.0] {
        for window_buttons in [false, true] {
            for context in [false, true] {
                let caption = Rect::new(0.0, 0.0, width, theme::CAPTION_H);
                let toggle = caption::sidebar_toggle(caption);
                let lane = caption::tab_strip_with_banner(
                    caption,
                    window_buttons,
                    context,
                    false,
                    Slots::NONE,
                );
                assert_eq!(
                    lane.x,
                    toggle.x + toggle.w,
                    "w={width} buttons={window_buttons} context={context}"
                );
                assert_eq!(
                    lane,
                    caption::tab_strip(caption, window_buttons, context),
                    "the overload every existing caller uses is unchanged"
                );
            }
        }
    }
}

/// The give-way order: the lane keeps one minimum tab, its gap and the `+`
/// whatever the banner asks for — and `fits` is the only thing that says so.
#[test]
fn the_lane_reservation_is_what_triggers_a_collapse_step() {
    // The floor both apps share, with the explore pane open: `solve(720, …)`
    // leaves the caption 488 px starting at 232. DESIGN works this case in §4.
    let caption = Rect::new(232.0, 0.0, 488.0, theme::CAPTION_H);
    assert_eq!(
        LANE_RESERVE,
        theme::TAB_MIN_W + theme::TAB_GAP + theme::TAB_NEW_W
    );

    let generous = Slots {
        profile: true,
        workspace: Workspace::Named(160.0),
        settings: true,
    };
    assert!(
        !fits(caption, true, false, generous),
        "a full name at the floor width pushes the lane under its reservation"
    );
    // Step 1 truncates, step 2 drops the name, step 3 merges the two chips.
    //
    // **DESIGN §4 works this exact case and gets step 1 *or* 2**: at the floor
    // with the pane open the lane's right edge is 508 (720 − 176 window controls
    // − 36 settings), the lane may begin no later than 382, and the workspace
    // slot may therefore be at most 78 px — narrower than step 1's 84. So step 1
    // is *not* enough here and step 2 is. Before SP-12 this test asserted the
    // opposite, because the lane's right edge did not subtract the settings slot
    // and every measurement was 36 px too generous.
    let truncated = Slots {
        workspace: Workspace::truncated(),
        ..generous
    };
    assert!(
        !fits(caption, true, false, truncated),
        "step 1 leaves the slot 84 px where DESIGN's arithmetic allows 78"
    );
    let icon_only = Slots {
        workspace: Workspace::IconOnly,
        ..generous
    };
    assert!(fits(caption, true, false, icon_only), "step 2 is enough");

    // And with the pane shut the banner gains 232 px, which is normally two
    // steps back.
    let wide = Rect::new(0.0, 0.0, 720.0, theme::CAPTION_H);
    assert!(fits(wide, true, false, generous));
}

/// The settings slot is the **anchor** of the caption's trailing chain: it takes
/// the square against the window controls, which is exactly where a lone context
/// toggle sits. That is why the two used to be one function.
#[test]
fn settings_sits_where_a_lone_context_toggle_would() {
    let caption = a_caption();
    assert_eq!(
        settings_slot(
            caption,
            true,
            Slots {
                settings: true,
                ..Slots::NONE
            }
        ),
        caption::context_toggle(caption, true, true)
    );
}

/// **SP-12's question, answered in arithmetic.** A window offering both keeps
/// Settings where every other window has it and seats the context toggle to its
/// left; the two are adjacent, neither overlaps, and the lane stops short of
/// both.
#[test]
fn settings_and_the_context_toggle_seat_side_by_side() {
    let caption = a_caption();
    let slots = Slots {
        profile: true,
        workspace: Workspace::Named(90.0),
        settings: true,
    };
    let settings = settings_slot(caption, true, slots);
    let context = caption::context_toggle_with_banner(caption, true, true, slots);
    assert_eq!(
        settings.w,
        theme::CAPTION_CONTROL_W,
        "Settings keeps its width"
    );
    assert_eq!(
        context.w,
        theme::CAPTION_CONTROL_W,
        "and the toggle gets one too"
    );
    assert_eq!(
        context.x + context.w,
        settings.x,
        "adjacent, in that order, with no overlap"
    );
    assert_eq!(
        settings,
        settings_slot(
            caption,
            true,
            Slots {
                settings: true,
                ..Slots::NONE
            }
        ),
        "and offering the column moved Settings not one pixel"
    );

    let lane = caption::tab_strip_with_banner(caption, true, true, false, slots);
    assert_eq!(lane.x + lane.w, context.x, "the lane stops at the toggle");
}

/// **The regression the aliasing hid.** With Settings offered and no context
/// column — every Koru Terminal window before SP-12 — the lane used to run the
/// last 36 px underneath the Settings icon, so a scrolled tab passed behind it.
#[test]
fn the_lane_never_runs_under_the_settings_slot() {
    let caption = a_caption();
    let slots = Slots {
        profile: true,
        workspace: Workspace::Named(90.0),
        settings: true,
    };
    let settings = settings_slot(caption, true, slots);
    let lane = caption::tab_strip_with_banner(caption, true, false, false, slots);
    assert_eq!(lane.x + lane.w, settings.x);
}

// ------------------------------------------------------- the pane header band
//
// The identity's other home (owner, 2026-09-02). The promises are the caption's,
// asserted again in the band that now carries them: one function per rect, a
// slot that is not offered takes no space and no press, and the clamp is the
// same clamp.

/// The pane header band as the shell cuts it: the explore column's own width,
/// `CAPTION_H` tall, at the window's left edge.
fn a_band() -> Rect {
    Rect::new(0.0, 0.0, theme::SIDEBAR_W, theme::CAPTION_H)
}

/// **The band has no sidebar toggle in front of it**, so the avatar leads the
/// band the way it led the pane header before the banner ever moved out.
#[test]
fn the_pane_headers_identity_starts_at_the_bands_row_inset() {
    let band = a_band();
    let slots = full(90.0);
    let profile = pane_profile_slot(band, slots);
    // NAV_PAD, not the bare edge: the tree rows below are inset by it, and an
    // avatar one pixel off their edge reads as a mistake (owner follow-up,
    // 2026-09-02).
    assert_eq!(profile.x, band.x + theme::NAV_PAD, "aligned with the rows");
    assert_eq!(
        profile.w,
        theme::CAPTION_CONTROL_W,
        "the caption control width"
    );
    let workspace = pane_workspace_slot(band, slots);
    assert_eq!(
        workspace.x,
        profile.x + profile.w,
        "and the name follows it with no gap of its own"
    );
    assert_eq!(pane_end(band, slots), workspace.x + workspace.w);
}

/// **The named chip fills the band out to its trailing inset** (owner,
/// 2026-09-02) — the pane header is the column's header, not a short pill
/// floating in a wide strip. Koru's `header_rects` does the same: its row is the
/// band inset by `NAV_PAD` on both sides, and the name half runs to that row's
/// far edge.
///
/// The caption's chip is deliberately *not* asserted here: it stays measured to
/// its label, because a chip that filled the caption would eat the tab lane.
#[test]
fn a_pane_header_name_fills_the_band_out_to_its_trailing_inset() {
    let band = a_band();
    let slots = full(90.0);
    let workspace = pane_workspace_slot(band, slots);
    assert_eq!(
        workspace.x + workspace.w,
        band.x + band.w - theme::NAV_PAD,
        "symmetrical with the inset the avatar starts at: {workspace:?}"
    );
    assert!(
        workspace.w > 90.0 + 26.0,
        "wider than the label asked for: {workspace:?}"
    );
    // The chevron the painter pins at the trailing edge, and the hover box the
    // chip wears, both come off this one rect — so widening it widens both.
    let box_ = named_chip_box(workspace);
    assert_eq!(box_.x + box_.w, workspace.x + workspace.w - 2.0);

    // And the same rect answers the press, all the way out to that edge.
    let far = (workspace.x + workspace.w - 1.0, band.y + band.h * 0.5);
    assert_eq!(pane_hit(band, slots, far.0, far.1), Some(Hit::Workspace));

    // A square chip is not stretched: it carries a tile, and a 180 px rounded
    // rectangle around a 22 px mark is not the same control.
    let icon_only = Slots {
        workspace: Workspace::IconOnly,
        ..slots
    };
    assert_eq!(
        pane_workspace_slot(band, icon_only).w,
        theme::CAPTION_CONTROL_W,
        "retains the icon control width"
    );
}

#[test]
fn every_pane_header_slot_hit_tests_back_to_itself() {
    let band = a_band();
    let slots = full(90.0);
    for (slot, expected) in [
        (pane_profile_slot(band, slots), Hit::Profile),
        (pane_workspace_slot(band, slots), Hit::Workspace),
    ] {
        assert!(slot.w > 0.0, "{expected:?} is offered, so it has width");
        let (x, y) = slot.centre();
        assert_eq!(pane_hit(band, slots, x, y), Some(expected));
    }
    // Settings never moved: it is caption furniture, and the band does not
    // answer for it however far right the pointer goes.
    let (_, y) = band.centre();
    assert_eq!(pane_hit(band, slots, band.x + band.w - 1.0, y), None);
}

/// A slot that is not offered is a zero-width rect, and `Rect::contains` is
/// inclusive on every edge — so it has to be excluded explicitly here too.
#[test]
fn a_pane_header_slot_that_is_not_offered_takes_no_space_and_no_press() {
    let band = a_band();
    let none = Slots::NONE;
    assert_eq!(pane_profile_slot(band, none).w, 0.0);
    assert_eq!(pane_workspace_slot(band, none).w, 0.0);
    assert_eq!(pane_end(band, none), band.x + theme::NAV_PAD);
    assert_eq!(pane_hit(band, none, band.x, band.y), None);
}

/// **The band is the whole budget.** [`fits`] asks what the tab lane keeps;
/// [`pane_fits`] asks whether the chips still fit the column, which is the pane
/// header ladder's one trigger.
#[test]
fn the_pane_header_ladder_triggers_when_the_chips_outgrow_the_band() {
    let band = a_band();
    // 36 + (90 + 26) = 152, inside the 216 a 232 band leaves once both insets
    // are taken — the room `pane_workspace_slot` actually hands out.
    assert!(pane_fits(band, full(90.0)));
    // 36 + 220 (the ceiling) = 256, which is wider than the column.
    assert!(!pane_fits(band, full(400.0)));
    // Step 1 fits again, which is what makes the ladder terminate.
    assert!(pane_fits(
        band,
        Slots {
            profile: true,
            workspace: Workspace::truncated(),
            settings: true,
        }
    ));
}

/// **The clamp is the same clamp**, and in the band it is the band that clamps.
///
/// The ceiling is the caption's rule — a chip there is measured to its label and
/// capped, so it cannot crowd the tab lane. In the pane header the chip fills,
/// so what stops a name is the column's own trailing inset; either way the slot
/// never overhangs what it is drawn in, and the ellipsis is applied once, to the
/// slot's width, by the painter.
#[test]
fn a_pane_header_name_is_clamped_by_the_band_it_is_drawn_in() {
    let band = a_band();
    let slot = pane_workspace_slot(band, full(400.0));
    assert_eq!(
        slot.x + slot.w,
        band.x + band.w - theme::NAV_PAD,
        "the column's trailing inset, not the caption's ceiling: {slot:?}"
    );
    assert!(
        slot.x + slot.w <= band.x + band.w,
        "and it never overhangs the column it is drawn in: {slot:?}"
    );
    // The caption's ceiling is untouched by any of this.
    let caption = Rect::new(0.0, 0.0, 1200.0, theme::CAPTION_H);
    assert_eq!(
        workspace_slot(caption, full(400.0)).w,
        WORKSPACE_MAX_W,
        "a caption chip is still measured and still capped"
    );
}

/// The named chip's box is the tab's own insets, so the pane header's workspace
/// chip and a tab pill across the divide are one shape.
#[test]
fn a_named_chip_wears_the_tabs_own_insets() {
    let band = a_band();
    let slot = pane_workspace_slot(band, full(90.0));
    let inner = named_chip_box(slot);
    assert_eq!(inner.y - slot.y, theme::CAPTION_INSET_TOP);
    assert_eq!(
        (slot.y + slot.h) - (inner.y + inner.h),
        theme::CAPTION_INSET_BOTTOM
    );
}
