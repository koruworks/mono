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
