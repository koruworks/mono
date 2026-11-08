// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_draw::inspect;

#[test]
fn resize_badge_hosts_preserve_sidebar_clamp_pane_center_and_small_pane_suppression() {
    let (mut view, _workspace) = crate::mainview::harness::on_document();
    view.sidebar_drag = Some(0.0);
    for width in [900.0, 350.0, 120.0] {
        view.viewport = (width, 600.0);
        let frame = inspect::Frame::begin(1, Affine::IDENTITY, scene::rect(0.0, 0.0, width, 600.0));
        view.draw_sidebar_width(&mut Scene::new(), Affine::IDENTITY);
        let snapshot = frame.finish();
        let badge = snapshot
            .records
            .iter()
            .find(|r| r.name == "KoruMeasurementBadge")
            .unwrap()
            .rect();
        let label = view.engine.simple_layout_in(
            &format!("{:.0} px", view.sidebar.full()),
            theme::role::hint(),
            theme::code_stack(),
        );
        let tw = label.width() + 14.0;
        let edge = view.sidebar_rect();
        assert_eq!(
            badge.x0,
            (edge.x + edge.w + 8.0).min((width - tw - 4.0).max(4.0)) as f64
        );
        assert_eq!(badge.y0, (theme::CAPTION_H + 12.0) as f64);
        // The painted rectangle adds the f32 origin before capture widens to f64.
        assert!((badge.width() - tw as f64).abs() < 0.001);
    }
    for pane in [
        Rect::new(30.0, 40.0, 480.0, 360.0),
        Rect::new(30.0, 40.0, 120.0, 80.0),
        Rect::new(30.0, 40.0, 119.0, 80.0),
        Rect::new(30.0, 40.0, 120.0, 79.0),
    ] {
        let frame = inspect::Frame::begin(1, Affine::IDENTITY, scene::rect(0.0, 0.0, 900.0, 600.0));
        view.draw_pane_size(&mut Scene::new(), Affine::IDENTITY, pane);
        let snapshot = frame.finish();
        let badge = snapshot
            .records
            .iter()
            .find(|r| r.name == "KoruMeasurementBadge");
        if pane.w < 120.0 || pane.h < 80.0 {
            assert!(badge.is_none());
        } else {
            let badge = badge.unwrap().rect();
            assert!((badge.center().x - pane.centre().0 as f64).abs() < 0.001);
            assert!((badge.center().y - pane.centre().1 as f64).abs() < 0.001);
        }
    }
    view.sidebar_drag = None;
    let frame = inspect::Frame::begin(1, Affine::IDENTITY, scene::rect(0.0, 0.0, 900.0, 600.0));
    view.draw_sidebar_width(&mut Scene::new(), Affine::IDENTITY);
    assert!(
        frame.finish().records.is_empty(),
        "drag lifetime stays host-owned"
    );
}

/// The band's fill, the tab strip and the caption's furniture are one row, so
/// the inspector shows them as children of a single HeaderRow — never as
/// loose records beside the sidebar, which stays a sibling.
#[test]
fn the_header_row_parents_the_strip_and_the_caption_furniture() {
    fn inside(records: &[inspect::Record], ancestor: usize, at: usize) -> bool {
        let mut parent = records[at].parent;
        while let Some(p) = parent {
            if p == ancestor {
                return true;
            }
            parent = records[p].parent;
        }
        false
    }

    let (mut view, workspace) = crate::mainview::harness::on_document();
    view.viewport = (1200.0, 700.0);
    let mut scene_out = Scene::new();
    let root = Affine::IDENTITY;
    let frame = inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, 1200.0, 700.0));
    view.draw_frame(
        &mut scene_out,
        &workspace,
        &ImageCache::new(),
        root,
        false,
        (-1.0, -1.0),
    );
    let records = frame.finish().records;
    let header = records
        .iter()
        .position(|r| r.name == "HeaderRow")
        .expect("the header row is captured");
    assert_eq!(
        records.iter().filter(|r| r.name == "HeaderRow").count(),
        1,
        "one row, one node"
    );
    let band = records[header].rect();
    let cap = view.drawn_caption();
    assert_eq!(
        (band.x0, band.y0, band.x1, band.y1),
        (
            cap.x as f64,
            cap.y as f64,
            (cap.x + cap.w) as f64,
            (cap.y + cap.h) as f64
        ),
        "the node bounds the drawn caption band"
    );
    for (i, record) in records.iter().enumerate() {
        if matches!(
            record.name,
            "Tab strip" | "KoruButtonGroup" | "KoruBranchChip"
        ) {
            assert!(
                inside(&records, header, i),
                "{} escapes the HeaderRow",
                record.name
            );
        }
        if record.name == "Explore sidebar" {
            assert!(
                !inside(&records, header, i),
                "the sidebar is the HeaderRow's sibling, not its child"
            );
        }
        if record.name == "KoruRail"
            && let Some(sidebar) = records.iter().position(|r| r.name == "Explore sidebar")
        {
            assert!(
                !inside(&records, sidebar, i),
                "the rail is the sidebar's sibling, not its child (E75)"
            );
        }
        if record.name == "KoruIconButton" {
            let panes: Vec<usize> = records
                .iter()
                .enumerate()
                .filter_map(|(p, r)| (r.name == "Content pane").then_some(p))
                .collect();
            // The explore column's `…` buttons are icon buttons too (E76).
            let sidebar = records.iter().position(|r| r.name == "Explore sidebar");
            assert!(
                inside(&records, header, i)
                    || panes.iter().any(|&p| inside(&records, p, i))
                    || sidebar.is_some_and(|s| inside(&records, s, i)),
                "a KoruIconButton answers to neither the HeaderRow, a Content pane nor the sidebar"
            );
        }
    }
    // The document's own AI and Context actions are the point of the rule:
    // they are in the capture, and they are inside the pane.
    let panes: Vec<usize> = records
        .iter()
        .enumerate()
        .filter_map(|(p, r)| (r.name == "Content pane").then_some(p))
        .collect();
    assert!(
        records.iter().enumerate().any(|(i, r)| {
            r.name == "KoruIconButton" && panes.iter().any(|&p| inside(&records, p, i))
        }),
        "the page's AI/Context buttons draw inside the Content pane"
    );
}

/// **No rule between the Koru mark and the first app** (2026-11-03, the
/// user's screenshot). The rail's ground and the lane's are one colour, and
/// they used to be two rects meeting at the lane's foot: at a display scale
/// where that edge falls between device pixels both fills antialiased the
/// row and the clear colour showed through as a short line across the rail.
/// The rail's ground now runs up under the lane, so in the rail's column no
/// fill has an edge at the lane's foot — and the first square has not moved.
#[test]
fn the_rail_ground_runs_under_the_lane_so_no_seam_shows_under_the_mark() {
    let (mut view, workspace) = crate::mainview::harness::on_document();
    view.viewport = (1200.0, 700.0);
    view.show_navigator(crate::panes::explore::NavigatorMode::Documents);
    view.sidebar
        .advance(std::time::Instant::now() + std::time::Duration::from_secs(5));
    view.relayout();
    let root = Affine::IDENTITY;
    let frame = inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, 1200.0, 700.0));
    view.draw_frame(
        &mut Scene::new(),
        &workspace,
        &ImageCache::new(),
        root,
        false,
        (-1.0, -1.0),
    );
    let records = frame.finish().records;
    let rail = records
        .iter()
        .position(|r| r.name == "KoruRail")
        .expect("the rail is drawn");
    let band = records[rail].rect();
    let lane = records
        .iter()
        .find(|r| r.name == "KoruLane")
        .expect("the lane is drawn")
        .rect();
    assert_eq!(band.y0, lane.y1, "the rail starts at the lane's foot");
    let (ground_at, ground) = records
        .iter()
        .enumerate()
        .find(|(_, r)| r.name == "fill" && r.parent == Some(rail))
        .map(|(i, r)| (i, r.rect()))
        .expect("the rail's ground");
    // One opaque fill across the lane's foot, drawn before the lane: the
    // lane's own edge then lands on its own colour, and nothing shows.
    assert!(
        ground.y0 <= lane.y0 && ground.y1 >= band.y1,
        "the ground runs from under the lane to the rail's foot: {ground:?}"
    );
    let lane_at = records.iter().position(|r| r.name == "KoruLane").unwrap();
    assert!(ground_at < lane_at, "the ground is under the lane");
    // The spacing is the rail's own: the first square where it always was.
    let squares = view.explore.apps_rail_rects(view.rail_area()).workspaces;
    assert_eq!(
        squares[0].1.y as f64,
        band.y0 + crate::panes::sidebar::apps_rail::PAD_TOP as f64
    );
}

/// **No notch between the pane and the document** (2026-11-03, the user's
/// screenshot). The document's top-left corner, where it meets the pane,
/// was masked in the ground like an outer corner: a wedge of the header's
/// colour opened into the pane's top-right, with no border on the pane's
/// side and the header's hairline broken over it. Outside that arc is the
/// pane, so the mask is the pane's fill and the hairline runs on across the
/// corner; the outer corners still mask in the ground.
#[test]
fn the_document_corner_beside_the_pane_is_masked_in_the_pane_and_keeps_the_hairline() {
    let (mut view, workspace) = crate::mainview::harness::on_document();
    view.viewport = (1200.0, 700.0);
    view.show_navigator(crate::panes::explore::NavigatorMode::Documents);
    view.sidebar
        .advance(std::time::Instant::now() + std::time::Duration::from_secs(5));
    view.relayout();
    let root = Affine::IDENTITY;
    let frame = inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, 1200.0, 700.0));
    view.draw_frame(
        &mut Scene::new(),
        &workspace,
        &ImageCache::new(),
        root,
        false,
        (-1.0, -1.0),
    );
    let records = frame.finish().records;
    let colour = |i: usize| {
        records[i]
            .properties
            .iter()
            .find(|p| p.name == "color")
            .map(|p| p.value.clone())
            .expect("a fill records its colour")
    };
    let fills_of = |corner: usize| {
        records
            .iter()
            .enumerate()
            .filter(move |(_, r)| r.name == "fill" && r.parent == Some(corner))
            .map(|(i, _)| i)
            .collect::<Vec<_>>()
    };
    let corner_at = |x: f32| {
        records
            .iter()
            .position(|r| r.name == "CardCorner" && r.rect().x0 == x as f64)
            .unwrap_or_else(|| panic!("a card corner at x = {x}"))
    };
    let sb = view.sidebar_rect();
    let top = view.content().y - 1.0;
    let r = theme::PAGE_RADIUS;
    let pane_fill = inspect::color(crate::panes::explore::pane_fill());
    let ground = inspect::color(theme::base_fill());
    assert_ne!(
        pane_fill, ground,
        "the notch only shows when the two differ"
    );

    // The document's top-left, on the pane's inside hairline.
    let inner = corner_at(sb.x + sb.w - 1.0);
    assert_eq!(records[inner].rect().y0, top as f64);
    let fills = fills_of(inner);
    assert_eq!(
        colour(fills[0]),
        pane_fill,
        "masked in the pane, not the ground"
    );
    let run = fills
        .iter()
        .copied()
        .find(|&i| records[i].rect() == scene::rect(sb.x + sb.w - 1.0, top, r, 1.0))
        .expect("the header's hairline runs across the corner");
    assert_eq!(colour(run), inspect::color(theme::p().panel_border));

    // The pane's own top-left is an outer corner: still the ground, and no
    // hairline drawn across it.
    let outer = corner_at(MainView::pane_of(sb).x - 1.0);
    let fills = fills_of(outer);
    assert_eq!(colour(fills[0]), ground);
    assert_eq!(
        fills.len(),
        1,
        "only the mask is a fill; the arc is a stroke"
    );
}
