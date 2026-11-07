// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::panes::explore::{ExploreView, NavigatorMode};
use koru_draw::theme;

fn band() -> Rect {
    Rect::new(
        0.0,
        theme::CAPTION_H,
        theme::RAIL_W,
        600.0 - theme::CAPTION_H,
    )
}

#[test]
fn every_control_uses_one_frame_for_hit_focus_and_geometry() {
    let view = ExploreView::default();
    view.with_rail_frame(band(), |rail, frame| {
        let controls = rail.controls(frame);
        assert_eq!(controls.first(), Some(&Hit::WaitingWorkspace));
        assert_eq!(controls.last(), Some(&Hit::Profile));
        assert_eq!(controls.len(), view.rail_modes().len() + 4);
        for control in controls {
            let node = frame.node(key(control)).unwrap();
            let (x, y) = node.rect.centre();
            assert_eq!(rail.hit(frame, x, y), Some(control));
            assert_eq!(node.rect.w, metrics::RAIL_SQUARE.value);
            assert_eq!(node.rect.h, metrics::RAIL_SQUARE.value);
            assert!(node.rect.x >= band().x);
            assert!(node.rect.x + node.rect.w <= band().x + band().w);
        }
        let rects = rail.rectangles(frame, band());
        assert!(rects.workspaces[0].1.y + rects.workspaces[0].1.h < rects.apps[0].1.y);
        assert!(rects.apps.last().unwrap().1.y + metrics::RAIL_SQUARE.value <= rects.help.y);
        assert!(rects.help.y + rects.help.h <= rects.settings.y);
        assert!(rects.settings.y + rects.settings.h <= rects.profile.y);
        assert!(rects.profile.y + rects.profile.h <= band().y + band().h);
    });
}

#[test]
fn a_folded_rail_has_no_pointer_targets() {
    let view = ExploreView::default();
    let folded = Rect::new(0.0, theme::CAPTION_H, 0.0, 500.0);
    let rects = view.apps_rail_rects(folded);
    assert!(rects.workspaces.iter().all(|(_, rect)| rect.w == 0.0));
    assert!(rects.apps.iter().all(|(_, rect)| rect.w == 0.0));
    assert_eq!(view.rail_hit(folded, 18.0, 60.0), None);
}

#[test]
fn keyboard_order_follows_reordered_views_after_the_workspace() {
    let mut rail = ExploreView::default().rail_model();
    rail.views.reverse();
    crate::input_engine::with(|engine| {
        let layout = rail.layout(
            engine,
            &mut TextCache::default(),
            band(),
            Interaction::default(),
        );
        let controls = rail.controls(&layout.frame);
        let expected: Vec<_> = rail.views.iter().map(|item| item.hit).collect();
        assert_eq!(&controls[1..1 + expected.len()], expected);
        assert!(controls.contains(&Hit::App(NavigatorMode::Documents)));
    });
}
