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
        assert_eq!(
            controls[1],
            Hit::WorkspaceMenu,
            "the `…` follows the squares"
        );
        assert_eq!(controls.last(), Some(&Hit::Profile));
        // The waiting square, the `…`, every view, then the foot's three:
        // Help, App settings and the profile square.
        assert_eq!(controls.len(), view.rail_modes().len() + 3);
        assert_eq!(controls[controls.len() - 1..], [Hit::Profile]);
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
        assert!(rects.workspaces[0].1.y + rects.workspaces[0].1.h <= rects.workspace_menu.y);
        assert!(rects.workspace_menu.y + rects.workspace_menu.h < rects.apps[0].1.y);
        assert!(rects.apps.last().unwrap().1.y + metrics::RAIL_SQUARE.value <= rects.profile.y);
        assert_eq!(rects.help.w, 0.0);
        assert_eq!(rects.settings.w, 0.0);
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
        // After the waiting square and the `…`.
        assert_eq!(&controls[2..2 + expected.len()], expected);
        assert!(controls.contains(&Hit::App(NavigatorMode::Documents)));
    });
}

#[test]
fn active_workspace_paints_an_accent_border_and_inactive_workspaces_do_not() {
    use koru_core::WorkspaceId;
    let active = WorkspaceId::new();
    let other = WorkspaceId::new();
    let rail = Rail {
        workspaces: vec![
            Item {
                hit: Hit::Workspace(active),
                name: "Active".into(),
                contents: Contents::Tile {
                    name: "Active".into(),
                    icon: None,
                    color: Some("red".into()),
                },
                selected: true,
                dimmed: false,
            },
            Item {
                hit: Hit::Workspace(other),
                name: "Other".into(),
                contents: Contents::Tile {
                    name: "Other".into(),
                    icon: None,
                    color: Some("blue".into()),
                },
                selected: false,
                dimmed: false,
            },
        ],
        views: Vec::new(),
        pins: Vec::new(),
        foot: Vec::new(),
    };
    crate::input_engine::with(|engine| {
        let layout = rail.layout(
            engine,
            &mut TextCache::default(),
            band(),
            Interaction::default(),
        );
        let active_node = layout.frame.node(key(Hit::Workspace(active))).unwrap();
        let other_node = layout.frame.node(key(Hit::Workspace(other))).unwrap();
        assert!(active_node.state.selected);
        assert!(!other_node.state.selected);

        // Paint both squares into separate scenes: the active workspace adds
        // its accent border stroke while an inactive workspace does not.
        let mut active_scene = Scene::new();
        let mut other_scene = Scene::new();
        let cx = FrameCx::root();
        let active_leaf = SquareContents {
            contents: rail.workspaces[0].contents.clone(),
            dimmed: false,
            selected_border: true,
        };
        let other_leaf = SquareContents {
            contents: rail.workspaces[1].contents.clone(),
            dimmed: false,
            selected_border: true,
        };
        active_leaf.paint(
            &mut active_scene,
            Affine::IDENTITY,
            engine,
            active_node.rect,
            &cx,
            active_node.state,
        );
        other_leaf.paint(
            &mut other_scene,
            Affine::IDENTITY,
            engine,
            other_node.rect,
            &cx,
            other_node.state,
        );
        assert!(
            active_scene.encoding().path_tags.len() > other_scene.encoding().path_tags.len(),
            "the active workspace draws an additional accent border path"
        );
    });
}

#[test]
fn active_app_paints_an_accent_border_and_inactive_apps_do_not() {
    let rail = Rail {
        workspaces: Vec::new(),
        views: vec![
            Item {
                hit: Hit::App(NavigatorMode::Documents),
                name: "Documents".into(),
                contents: Contents::Glyph(koru_draw::icons::FILES),
                selected: true,
                dimmed: false,
            },
            Item {
                hit: Hit::App(NavigatorMode::Tasks),
                name: "Tasks".into(),
                contents: Contents::Glyph(koru_draw::icons::FILES),
                selected: false,
                dimmed: false,
            },
        ],
        pins: Vec::new(),
        foot: Vec::new(),
    };
    crate::input_engine::with(|engine| {
        let layout = rail.layout(
            engine,
            &mut TextCache::default(),
            band(),
            Interaction::default(),
        );
        let active_node = layout
            .frame
            .node(key(Hit::App(NavigatorMode::Documents)))
            .unwrap();
        let other_node = layout
            .frame
            .node(key(Hit::App(NavigatorMode::Tasks)))
            .unwrap();
        assert!(active_node.state.selected);
        assert!(!other_node.state.selected);

        let mut active_scene = Scene::new();
        let mut other_scene = Scene::new();
        let cx = FrameCx::root();
        let active_leaf = SquareContents {
            contents: rail.views[0].contents.clone(),
            dimmed: false,
            selected_border: true,
        };
        let other_leaf = SquareContents {
            contents: rail.views[1].contents.clone(),
            dimmed: false,
            selected_border: true,
        };
        active_leaf.paint(
            &mut active_scene,
            Affine::IDENTITY,
            engine,
            active_node.rect,
            &cx,
            active_node.state,
        );
        other_leaf.paint(
            &mut other_scene,
            Affine::IDENTITY,
            engine,
            other_node.rect,
            &cx,
            other_node.state,
        );
        assert!(
            active_scene.encoding().path_tags.len() > other_scene.encoding().path_tags.len(),
            "the active app draws the same accent border path as the active workspace"
        );
    });
}
