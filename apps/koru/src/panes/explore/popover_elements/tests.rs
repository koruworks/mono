// SPDX-License-Identifier: MPL-2.0

//! **Rung 4 of the plan's ladder**: the canonical workspace row runs end to
//! end on the app's real types. One frame drives the targets and the
//! accessibility tree, and both agree with today's page wherever today's
//! page already decides; where D2 adds something — the gear as a target of
//! its own, named and reachable — it is asserted as the addition it is.

use std::time::Duration;

use accesskit::{Action as AkAction, Role as AkRole};
use koru_core::{ProfileId, WorkspaceId};
use koru_draw::el::{FrameCx, Interaction, TextCache};
use koru_draw::inspect;
use koru_draw::theme::{self as dtheme, Contrast, Ink, Surface, themes};
use koru_i18n::Lang;
use koru_workspace_host::accessibility::{ControlPart, Occurrence, ViewSlot, WindowSlot};
use vello::kurbo::Affine;

use super::*;
use crate::panes::Outcome;
use crate::panes::explore::accessibility::Context;
use crate::panes::explore::{HeadButton, ProfileSummary, WorkspaceEntry};

const VIEW: (f32, f32) = (1200.0, 700.0);

fn uuid(n: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(0x6b6f_7275_0000_4000_8000_0000_0000_0000 | n)
}

fn area(width: f32) -> Rect {
    Rect::new(koru_draw::theme::RAIL_W, 0.0, width, VIEW.1)
}

fn workspaces(n: usize) -> Vec<WorkspaceEntry> {
    (0..n)
        .map(|i| {
            let mut w = WorkspaceEntry::new(
                WorkspaceId::from_uuid(uuid(i as u128 + 1)),
                &format!("Workspace {i:02}"),
                0,
            );
            if i == 2 {
                w.color = Some("blue".into());
            }
            w
        })
        .collect()
}

/// The pane with `list`, the second workspace current, the Workspaces page
/// open and settled.
fn opened(list: Vec<WorkspaceEntry>) -> ExploreView {
    let mut v = ExploreView::new();
    let profile = ProfileSummary {
        id: ProfileId::from_uuid(uuid(900)),
        name: "koru".into(),
        email: "koru@example.com".into(),
        avatar: None,
        icon: None,
        color: None,
        account: None,
        folder: "C:/profiles/koru".into(),
    };
    let current = list.get(1).map(|w| w.id);
    v.set_context(vec![profile.clone()], profile.id, list, current);
    v.toggle_popover(PopoverKind::Summary);
    v.advance_popover(std::time::Instant::now() + Duration::from_secs(5));
    v
}

fn light() -> FrameCx {
    FrameCx::new(&themes::LIGHT_THEME, Contrast::Standard)
}

fn lay(v: &ExploreView, a: Rect) -> (El, Frame) {
    let mut engine = TextEngine::new();
    let mut cache = TextCache::default();
    v.workspaces_frame(a, VIEW, &mut engine, &mut cache, light())
        .expect("the page takes input")
}

fn cx(a: Rect, lang: Lang) -> Context {
    Context {
        window: WindowSlot::from_raw(1),
        view: ViewSlot::from_raw(1),
        area: a,
        rail: ExploreView::new().rail_of(a),
        viewport: VIEW,
        lang,
    }
}

fn near(a: Rect, b: Rect) -> bool {
    (a.x - b.x).abs() < 0.01
        && (a.y - b.y).abs() < 0.01
        && (a.w - b.w).abs() < 0.01
        && (a.h - b.h).abs() < 0.01
}

/// **The element page lays out where today's page is**, row for row and
/// part for part, at both widths, scrolled and not.
#[test]
fn the_element_page_matches_todays_geometry() {
    for (n, scroll) in [(4, 0.0), (40, 300.0)] {
        for width in [dtheme::SIDEBAR_MIN_W, dtheme::SIDEBAR_MAX_W] {
            let mut v = opened(workspaces(n));
            let a = area(width);
            v.scroll_popover(a, VIEW, -scroll);
            let (_, frame) = lay(&v, a);
            let today: Vec<_> = v
                .popover_rects(a, VIEW)
                .into_iter()
                .filter(|(row, _)| matches!(row, PopoverRow::Workspace(_)))
                .collect();
            let rows: Vec<_> = frame.named("Workspace row").collect();
            assert_eq!(rows.len(), today.len());
            for ((row, r), node) in today.iter().zip(rows) {
                let parts = v.workspace_row_parts(*row, *r);
                let PopoverRow::Workspace(i) = row else {
                    unreachable!()
                };
                let id = v.workspaces[*i].id;
                assert!(
                    near(node.rect, *r),
                    "{width}/{n}: row {r:?} vs {:?}",
                    node.rect
                );
                let tile = frame
                    .nodes
                    .iter()
                    .filter(|t| t.name == "Workspace tile")
                    .find(|t| near(t.rect, parts.chip));
                assert!(tile.is_some(), "the tile at {:?}", parts.chip);
                let gear = frame.node(manage(id)).expect("the gear");
                assert!(near(gear.rect, parts.trailing), "lane {:?}", gear.rect);
            }
            let band = v.popover_list_band(a, VIEW).unwrap();
            assert!(near(frame.rect_of("Workspace list").unwrap(), band));
            let heading = v
                .popover_rects(a, VIEW)
                .into_iter()
                .find(|(row, _)| *row == PopoverRow::WorkspacesHeading)
                .unwrap()
                .1;
            assert!(near(frame.node(HEADING).unwrap().rect, heading));
        }
    }
}

/// **Every point presses what today's hit test presses** on a workspace
/// row — its words or its gear, never both — and the heading presses
/// nothing (it is semantic only; today a press there does nothing either).
#[test]
fn every_point_presses_what_todays_hit_test_presses() {
    for (n, scroll) in [(4, 0.0), (40, 300.0)] {
        let mut v = opened(workspaces(n));
        let a = area(dtheme::SIDEBAR_MIN_W);
        v.scroll_popover(a, VIEW, -scroll);
        let (_, frame) = lay(&v, a);
        let panel = v.popover_panel(a, VIEW).unwrap();
        let mut checked = 0;
        // Off the shared edges, which the next test is about.
        let mut y = panel.y - 3.75;
        while y < panel.y + panel.h + 4.0 {
            let mut x = panel.x - 3.75;
            while x < panel.x + panel.w + 4.0 {
                let today = v.popover_part_at(a, VIEW, x, y).and_then(|(_, row, part)| {
                    matches!(row, PopoverRow::Workspace(_)).then_some((row, part))
                });
                let now = frame.hit_at(x, y).and_then(|key| row_part_of(&v, key));
                assert_eq!(now, today, "{n} rows, scrolled {scroll}: ({x}, {y})");
                checked += usize::from(today.is_some());
                x += 1.5;
            }
            y += 1.5;
        }
        assert!(checked > 1000, "the rows were reached");
        assert!(
            !frame.hits().iter().any(|h| h.key == HEADING),
            "the heading is no target"
        );
    }
}

/// **A shared edge has one owner**: the row that starts on it. Today's
/// test checks rows in order with inclusive edges, so the row above took
/// the line; a region now owns its start edge and not its end edge.
#[test]
fn a_shared_edge_belongs_to_the_row_that_starts_on_it() {
    let v = opened(workspaces(3));
    let a = area(dtheme::SIDEBAR_MIN_W);
    let (_, frame) = lay(&v, a);
    let second = frame.node(primary(v.workspaces[1].id)).unwrap().rect;
    let x = second.x + 10.0;
    assert_eq!(frame.hit_at(x, second.y), Some(primary(v.workspaces[1].id)));
    assert_eq!(
        frame.hit_at(x, second.y - 0.01),
        Some(primary(v.workspaces[0].id))
    );
}

/// The page's tree: the Menu (named as today), the heading, and each row's
/// primary and gear as sibling items. The primaries keep today's locators
/// and names; the gear is D2's addition, named *Manage workspace*.
#[test]
fn the_semantic_tree_puts_the_gear_beside_its_row() {
    let v = opened(workspaces(3));
    let a = area(dtheme::SIDEBAR_MIN_W);
    let (_, frame) = lay(&v, a);
    let cx = cx(a, Lang::En);
    let nodes = v.element_semantics(&cx, &frame);
    assert_eq!(nodes.len(), 1);
    let menu = &nodes[0];
    assert_eq!(menu.role, AkRole::Menu);
    // The locator and name the page's Menu has always had.
    assert_eq!(
        menu.occurrence,
        Occurrence::surface(
            WindowSlot::from_raw(1),
            ViewSlot::from_raw(1),
            SURFACE,
            ControlPart::new("popover")
        )
    );
    assert_eq!(
        menu.label.as_deref(),
        Some("Open account and workspace menu")
    );
    assert_eq!(menu.size_of_set, Some(6), "three primaries, three gears");
    let roles: Vec<_> = menu.children.iter().map(|c| c.role).collect();
    assert_eq!(roles[0], AkRole::Heading);
    assert!(roles[1..].iter().all(|r| *r == AkRole::MenuItem));
    for (i, w) in v.workspaces.iter().enumerate() {
        let primary = &menu.children[1 + i * 2];
        let gear = &menu.children[2 + i * 2];
        // The locator and name each row has always had: the workspace's
        // own id and root part, and its title.
        assert_eq!(
            primary.occurrence,
            Occurrence::model(
                WindowSlot::from_raw(1),
                ViewSlot::from_raw(1),
                w.id.as_uuid(),
                ControlPart::ROOT
            ),
            "today's locator"
        );
        assert_eq!(primary.label.as_deref(), Some(w.name.as_str()));
        assert_eq!(primary.position, Some(i * 2));
        assert_eq!(primary.selected, Some(v.current == Some(w.id)));
        assert_eq!(
            gear.occurrence,
            Occurrence::model(
                WindowSlot::from_raw(1),
                ViewSlot::from_raw(1),
                w.id.as_uuid(),
                ControlPart::new("manage")
            )
        );
        assert_eq!(gear.label.as_deref(), Some("Manage workspace"));
        assert_eq!(gear.actions, [AkAction::Click, AkAction::Focus]);
    }
    assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
}

/// The gear's name exists in every language the app ships, and is
/// translated where the language is not English.
#[test]
fn the_manage_names_resolve_in_every_language() {
    for key in [
        koru_i18n::Key::ManageWorkspace,
        koru_i18n::Key::ManageProfile,
    ] {
        let english = koru_i18n::t(Lang::En, key);
        for lang in Lang::ALL {
            let name = koru_i18n::t(lang, key);
            assert!(!name.trim().is_empty(), "{lang:?} {key:?}");
            if lang != Lang::En {
                assert_ne!(name, english, "{lang:?} {key:?} is translated");
            }
        }
    }
}

fn gear_of(id: WorkspaceId) -> Occurrence {
    Occurrence::model(
        WindowSlot::from_raw(1),
        ViewSlot::from_raw(1),
        id.as_uuid(),
        ControlPart::new("manage"),
    )
}

/// **A request for the gear acts on the gear** (D2). Focus lights that
/// row's gear and chooses nothing; Click opens Manage on that row, even
/// with the keyboard on another.
#[test]
fn focus_lights_the_gear_and_click_opens_manage_on_that_row() {
    use winit::keyboard::{Key as WKey, NamedKey};
    let list = workspaces(3);
    let target = list[2].id;
    let mut v = opened(list);
    let a = area(dtheme::SIDEBAR_MIN_W);
    v.popover_key(&WKey::Named(NamedKey::ArrowDown));
    let keyboard_on = v.popover_hover();
    assert_eq!(
        v.accessibility_action(a, gear_of(target), AkAction::Focus, VIEW),
        Some(Outcome::Consumed)
    );
    assert!(v.live().is_some(), "focus chose nothing");
    assert_ne!(v.popover_hover(), keyboard_on, "it moved to the gear's row");
    assert_eq!(v.popover_part, Some(PopoverPart::Gear));
    let (_, frame) = lay(&v, a);
    let gear = frame.node(manage(target)).unwrap();
    assert!(gear.state.hovered, "the gear is lit");
    assert_eq!(
        v.popover_interaction().hot,
        Some(manage(target)),
        "and it is the gear, not the row's words"
    );

    let mut v = opened(workspaces(3));
    v.popover_key(&WKey::Named(NamedKey::ArrowDown));
    assert_eq!(
        v.accessibility_action(a, gear_of(target), AkAction::Click, VIEW),
        Some(Outcome::ManageWorkspace(target)),
        "the request names the third row; the keyboard's row is not chosen"
    );
}

/// **A key survives a reorder and dies with its workspace**: the same key
/// is the same workspace's target after the list changes order, and gone —
/// inherited by nothing — once the workspace is.
#[test]
fn a_target_survives_reorder_and_deletion_invalidates_it() {
    let a = area(dtheme::SIDEBAR_MIN_W);
    let list = workspaces(4);
    let moving = list[3].id;
    let before = lay(&opened(list.clone()), a).1;
    let mut reordered = list.clone();
    reordered[3].pinned = true;
    let after = lay(&opened(reordered), a).1;
    let (was, now) = (
        before.node(primary(moving)).unwrap().rect,
        after.node(primary(moving)).unwrap().rect,
    );
    assert!(now.y < was.y, "pinned, it moved up");
    assert_eq!(
        after.hit_at(now.x + 10.0, now.y + 5.0),
        Some(primary(moving))
    );
    let without: Vec<_> = list.into_iter().filter(|w| w.id != moving).collect();
    let gone = lay(&opened(without), a).1;
    assert!(!gone.contains(primary(moving)) && !gone.contains(manage(moving)));
}

/// **Scale is applied once.** Painted at 1.5, every element's record is
/// where the frame put it, in logical units; a press converted back by the
/// host lands on the same target.
#[test]
fn the_frame_holds_at_scale_one_and_a_half() {
    let v = opened(workspaces(4));
    let a = area(dtheme::SIDEBAR_MIN_W);
    let (tree, frame) = lay(&v, a);
    let mut engine = TextEngine::new();
    let mut scene = vello::Scene::new();
    let root = Affine::scale(1.5);
    let capture = inspect::Frame::begin(1, root, koru_draw::scene::rect(0.0, 0.0, VIEW.0, VIEW.1));
    frame.paint(&tree, &mut scene, root, &mut engine);
    let records = capture.finish().records;
    let elements: Vec<_> = records.iter().filter(|r| r.kind == "element").collect();
    assert_eq!(elements.len(), frame.nodes.len());
    for (record, node) in elements.iter().zip(&frame.nodes) {
        let b = record.rect();
        let got = Rect::new(
            b.x0 as f32,
            b.y0 as f32,
            (b.x1 - b.x0) as f32,
            (b.y1 - b.y0) as f32,
        );
        assert!(
            near(got, node.rect),
            "{}: {got:?} vs {:?}",
            node.name,
            node.rect
        );
    }
    let gear = frame.node(manage(v.workspaces[0].id)).unwrap().rect;
    let (device_x, device_y) = ((gear.x + 5.0) * 1.5, (gear.y + 5.0) * 1.5);
    assert_eq!(
        frame.hit_at(device_x / 1.5, device_y / 1.5),
        Some(manage(v.workspaces[0].id))
    );
}

/// Input follows the host's policies: nothing while the popover closes,
/// nothing for a page that is not the one showing.
#[test]
fn a_closing_popover_or_another_page_lays_out_no_targets() {
    let a = area(dtheme::SIDEBAR_MIN_W);
    let mut engine = TextEngine::new();
    let mut cache = TextCache::default();
    let mut v = opened(workspaces(3));
    v.close_popover();
    assert!(
        v.workspaces_frame(a, VIEW, &mut engine, &mut cache, light())
            .is_none()
    );
    let mut v = opened(workspaces(3));
    v.toggle_profile_popover();
    v.advance_popover(std::time::Instant::now() + Duration::from_secs(5));
    assert!(
        v.workspaces_frame(a, VIEW, &mut engine, &mut cache, light())
            .is_none()
    );
}

/// **Two themes in one frame, through the real adapters.** A light subtree
/// and a fixture-theme subtree, each with an uncoloured tile, a named-colour
/// tile, a name and a lit gear, paint every one of those in their own
/// theme's table; each element's record names its theme and contrast.
#[test]
fn two_themed_subtrees_paint_tiles_text_and_gear_in_their_own_themes() {
    let v = opened(workspaces(4));
    let w = |n: usize| v.workspaces[n].clone();
    // Each half its own two workspaces, so no key repeats in the frame: an
    // uncoloured one (current) and a blue one.
    let half = |name: &'static str,
                theme: &'static dtheme::Theme,
                plain: WorkspaceEntry,
                blue: WorkspaceEntry| {
        col(name)
            .fill()
            .theme(theme)
            .child(v.workspace_row(plain.id, &plain.name, None, None, true))
            .child(v.workspace_row(blue.id, &blue.name, None, Some("blue".into()), false))
    };
    let tree = row("Split")
        .child(half("Light half", &themes::LIGHT_THEME, w(0), w(2)))
        .child(half("Fixture half", &themes::FIXTURE_THEME, w(1), w(3)));
    // The light half's gear under the pointer, the fixture half's under the
    // keyboard: both reveal and light their gear.
    let interaction = Interaction {
        hot: Some(manage(w(0).id)),
        focused: Some(manage(w(1).id)),
        ..Interaction::default()
    };
    let mut engine = TextEngine::new();
    let mut cache = TextCache::default();
    let frame = tree.layout(
        &mut engine,
        &mut cache,
        Rect::new(0.0, 0.0, 600.0, 60.0),
        light().with_interaction(interaction),
    );
    let mut scene = vello::Scene::new();
    let capture = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        koru_draw::scene::rect(0.0, 0.0, 600.0, 60.0),
    );
    frame.paint(&tree, &mut scene, Affine::IDENTITY, &mut engine);
    let records = capture.finish().records;

    // Every record under each half, by the half's theme.
    let under = |half: &str| -> Vec<&inspect::Record> {
        let top = records.iter().position(|r| r.name == half).unwrap();
        records
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                let mut at = Some(*i);
                while let Some(j) = at {
                    if j == top {
                        return true;
                    }
                    at = records[j].parent;
                }
                false
            })
            .map(|(_, r)| r)
            .collect()
    };
    for (half, theme) in [
        ("Light half", &themes::LIGHT_THEME),
        ("Fixture half", &themes::FIXTURE_THEME),
    ] {
        let p = theme.standard;
        let records = under(half);
        let colours: Vec<&str> = records
            .iter()
            .flat_map(|r| r.properties.iter())
            .filter(|p| matches!(p.name, "color" | "foreground"))
            .map(|p| p.value.as_str())
            .collect();
        let has = |c: vello::peniko::Color, what: &str| {
            let c = inspect::color(c);
            assert!(
                colours.contains(&c.as_str()),
                "{half}: {what} {c} in {colours:?}"
            );
        };
        has(p.panel_bg, "the uncoloured tile");
        has(p.panel_border, "its outline");
        has(
            p.workspace_colors
                .iter()
                .find(|(n, _)| *n == "blue")
                .unwrap()
                .1,
            "the named-colour tile",
        );
        has(Ink::Primary.of(p), "the current name");
        has(Surface::Hover.of(p), "the lit gear's plate");
        let themed = records.iter().filter(|r| r.kind == "element").all(|r| {
            r.properties
                .iter()
                .any(|q| q.name == "theme" && q.value == theme.key)
                && r.properties
                    .iter()
                    .any(|q| q.name == "contrast" && q.value == "standard")
        });
        assert!(themed, "{half}: every element record names its theme");
    }
}

/// **Rung 5: F12 on any element reads everything §3.9 lists** — where it was
/// written, its key or the key it belongs to, the authored token beside the
/// value, the profile, theme and contrast, the state rule that won, its clip,
/// and what it is to a screen reader and the pointer.
#[test]
fn every_element_record_reads_what_the_contract_lists() {
    let list = workspaces(4);
    let target = list[2].id;
    let mut v = opened(list);
    let a = area(dtheme::SIDEBAR_MIN_W);
    v.accessibility_action(a, gear_of(target), AkAction::Focus, VIEW);
    let (tree, frame) = lay(&v, a);
    let mut engine = TextEngine::new();
    let mut scene = vello::Scene::new();
    let capture = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        koru_draw::scene::rect(0.0, 0.0, VIEW.0, VIEW.1),
    );
    frame.paint(&tree, &mut scene, Affine::IDENTITY, &mut engine);
    let records = capture.finish().records;
    let elements: Vec<_> = records.iter().filter(|r| r.kind == "element").collect();
    assert_eq!(
        elements.len(),
        frame.nodes.len(),
        "every element has a record"
    );

    let value = |record: &inspect::Record, name: &str| {
        record
            .properties
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.value.clone())
            .unwrap_or_else(|| panic!("{}: no {name}", record.name))
    };
    for record in &elements {
        assert!(
            record
                .source
                .starts_with("apps/koru/src/panes/explore/popover_elements.rs:"),
            "{}: sourced where it was written, not where it painted: {}",
            record.name,
            record.source
        );
        for name in [
            "element",
            "profile",
            "theme",
            "appearance",
            "contrast",
            "semantics",
            "input",
        ] {
            value(record, name);
        }
    }
    let nth = |name: &str, n: usize| {
        *elements
            .iter()
            .filter(|r| r.name == name)
            .nth(n)
            .unwrap_or_else(|| panic!("no {name} #{n}"))
    };

    let row = nth("Workspace row", 2);
    assert_eq!(value(row, "occurrence"), "3 of 4 named alike");
    assert_eq!(value(row, "min_h"), "metrics::ROW_MIN -> 26");
    assert_eq!(
        value(row, "pad_x"),
        "alias::ROW_PLATE_INSET = Space::S1 -> 4"
    );
    assert_eq!(
        value(row, "semantics"),
        "none: transparent to semantic parenting"
    );
    assert!(value(row, "input").starts_with("presses model "));
    assert!(value(row, "input").ends_with("/root"));

    let plate = nth("Workspace plate", 2);
    assert_eq!(
        value(plate, "radius"),
        "metrics::WORKSPACE_GEAR_RADIUS -> 5"
    );
    assert_eq!(value(plate, "gap"), "Space::S1 -> 4");
    assert_eq!(value(plate, "state rule"), "workspace row: hover");
    assert!(value(plate, "fill colour").starts_with("Surface::Hover -> #"));
    assert_eq!(
        value(nth("Workspace plate", 0), "state rule"),
        "workspace row: base"
    );

    let primary = nth("Workspace primary action", 2);
    assert!(value(primary, "key").ends_with("/root"));
    assert_eq!(value(primary, "name"), "model content");
    assert_eq!(value(primary, "semantics"), "MenuItem");

    let gear = nth("Manage gear", 2);
    let id = target.as_uuid().hyphenated().to_string();
    assert_eq!(value(gear, "key"), format!("model {id}/manage"));
    assert_eq!(value(gear, "name"), "Key::ManageWorkspace");
    assert_eq!(value(gear, "w"), "metrics::TRAILING_LANE_W -> 20");
    assert_eq!(
        value(gear, "semantics"),
        "MenuItem: delegated to the leaf's own node"
    );
    assert_eq!(value(gear, "state"), "hovered");
    let input = value(gear, "input");
    assert!(input.starts_with("pointer target; focusable; Click/Focus; lit by model "));

    let tile = nth("Workspace tile", 2);
    assert_eq!(value(tile, "semantics"), "decorative");
    assert_eq!(value(tile, "within"), format!("model {id}/root"));

    let name = nth("Workspace name", 2);
    assert_eq!(value(name, "text role"), "row");
    assert!(value(name, "ink").starts_with("Ink::Secondary -> #"));
    assert_eq!(value(name, "text mode"), "single line, elided");

    let heading = nth("Workspaces heading", 0);
    assert!(value(heading, "ink").starts_with("Ink::Hint -> #"));
    assert_eq!(value(heading, "pad_x"), "alias::NAV_PAD = Space::S2 -> 8");

    let list = nth("Workspace list", 0);
    assert!(value(list, "scroll").starts_with("content "));
    // The rows are clipped to the list they scroll in.
    let viewport = list.rect();
    assert_eq!(
        inspect::rect(row.clip),
        viewport.intersect(inspect::rect(list.clip))
    );
}

/// **Rung 7: the keyboard reaches the gear.** Down lights a row on its
/// primary; Right moves onto its gear, which the page then paints lit and
/// reports as the focused node; Enter opens Manage on that workspace.
#[test]
fn right_reaches_the_gear_and_enter_opens_manage() {
    use winit::keyboard::{Key as WKey, NamedKey};
    let list = workspaces(3);
    let mut v = opened(list);
    let a = area(dtheme::SIDEBAR_MIN_W);
    let cx = cx(a, Lang::En);
    v.popover_key(&WKey::Named(NamedKey::ArrowDown));
    let row = v.popover_hover().and_then(|i| match v.popover_rows()[i] {
        PopoverRow::Workspace(i) => Some(v.workspaces[i].id),
        _ => None,
    });
    let id = row.expect("a workspace row is lit");
    assert_eq!(
        v.accessible_focus(&cx).map(|o| o.part),
        Some(ControlPart::ROOT),
        "a row is reached on its primary"
    );
    v.popover_key(&WKey::Named(NamedKey::ArrowRight));
    assert_eq!(
        v.accessible_focus(&cx),
        Some(gear_of(id)),
        "the gear is the focused node"
    );
    let (_, frame) = lay(&v, a);
    assert!(
        frame.node(manage(id)).unwrap().state.hovered,
        "the gear is lit"
    );
    assert!(
        !frame.node(primary(id)).unwrap().state.hovered,
        "and the primary is not"
    );
    assert_eq!(
        v.popover_key(&WKey::Named(NamedKey::Enter)),
        Some(Outcome::ManageWorkspace(id))
    );
    assert!(v.live().is_none(), "Manage is the end of the page");
}

// ------------------------------------------------------ the Profiles page (P5)

/// `n` Profiles, the second one open, the Profiles page open and settled.
fn opened_profiles(n: usize) -> ExploreView {
    let mut v = ExploreView::new();
    let profiles: Vec<ProfileSummary> = (0..n)
        .map(|i| ProfileSummary {
            id: ProfileId::from_uuid(uuid(800 + i as u128)),
            name: format!("Profile {i:02}"),
            email: format!("p{i:02}@example.com"),
            avatar: None,
            icon: None,
            color: None,
            account: None,
            folder: format!("C:/profiles/{i}"),
        })
        .collect();
    let open = profiles[1].id;
    let list = workspaces(2);
    let current = list.first().map(|w| w.id);
    v.set_context(profiles, open, list, current);
    v.toggle_profile_popover();
    v.advance_popover(std::time::Instant::now() + Duration::from_secs(5));
    v
}

fn profile_gear(id: ProfileId) -> Occurrence {
    Occurrence::model(
        WindowSlot::from_raw(1),
        ViewSlot::from_raw(1),
        id.as_uuid(),
        ControlPart::new("manage"),
    )
}

/// **Rung 8's tree**: the Menu named as it always was, its heading, each
/// Profile beside its *Manage profile* gear, the open one selected, and the
/// foot's three items in P0's order with the names and locators they have
/// always had.
#[test]
fn the_profiles_page_puts_each_gear_beside_its_profile_and_keeps_the_foot() {
    let v = opened_profiles(3);
    let a = area(dtheme::SIDEBAR_MIN_W);
    let cx = cx(a, Lang::En);
    let menu = v
        .with_page(a, VIEW, |frame| v.element_semantics(&cx, frame))
        .expect("the page")
        .pop()
        .expect("its Menu");
    assert_eq!(menu.role, AkRole::Menu);
    assert_eq!(menu.label.as_deref(), Some("Switch profile"));
    assert_eq!(
        menu.size_of_set,
        Some(9),
        "three Profiles, three gears, three items"
    );
    assert_eq!(menu.children[0].role, AkRole::Heading);
    assert_eq!(menu.children[0].label.as_deref(), Some("PROFILES · 3"));
    for (i, profile) in v.profiles.iter().enumerate() {
        let primary = &menu.children[1 + i * 2];
        let gear = &menu.children[2 + i * 2];
        assert_eq!(primary.label.as_deref(), Some(profile.name.as_str()));
        assert_eq!(primary.selected, Some(i == 1), "the open Profile");
        assert_eq!(gear.occurrence, profile_gear(profile.id));
        assert_eq!(gear.label.as_deref(), Some("Manage profile"));
    }
    let foot: Vec<_> = menu.children[7..]
        .iter()
        .map(|node| (node.occurrence.part.name(), node.label.as_deref()))
        .collect();
    assert_eq!(
        foot,
        vec![
            ("profile-status", Some("Subscribe to Koru Cloud")),
            ("profile-devices", Some("0 devices")),
            ("new-profile", Some("New profile")),
        ]
    );
}

/// **A request for a Profile's gear acts on that gear**: Focus lights it
/// and chooses nothing; Click opens Manage on that Profile, whatever the
/// keyboard was on.
#[test]
fn focus_lights_a_profiles_gear_and_click_opens_manage_on_it() {
    use winit::keyboard::{Key as WKey, NamedKey};
    let a = area(dtheme::SIDEBAR_MIN_W);
    let mut v = opened_profiles(3);
    let target = v.profiles[2].id;
    assert_eq!(
        v.accessibility_action(a, profile_gear(target), AkAction::Focus, VIEW),
        Some(Outcome::Consumed)
    );
    assert!(v.live().is_some(), "focus chose nothing");
    assert_eq!(v.popover_part, Some(PopoverPart::Gear));
    assert_eq!(
        v.popover_interaction().hot,
        Some(profiles::manage(target)),
        "the gear is lit, not the row's words"
    );

    let mut v = opened_profiles(3);
    v.popover_key(&WKey::Named(NamedKey::ArrowDown));
    assert_eq!(
        v.accessibility_action(a, profile_gear(target), AkAction::Click, VIEW),
        Some(Outcome::ManageProfile(target))
    );
}

/// The foot's items are pressed where they are drawn: Koru Cloud,
/// Linked devices, and `+`.
#[test]
fn each_foot_item_opens_its_destination() {
    let a = area(dtheme::SIDEBAR_MIN_W);
    for (button, outcome) in [
        (HeadButton::Status, Outcome::ManageCloud),
        (HeadButton::Devices, Outcome::ManageDevices),
        (HeadButton::New, Outcome::NewProfile),
    ] {
        let mut v = opened_profiles(3);
        let r = v
            .popover_head_button_rect(a, VIEW, button)
            .expect("the foot's item");
        let (x, y) = r.centre();
        assert_eq!(
            v.popover_part_at(a, VIEW, x, y)
                .map(|(_, row, part)| (row, part)),
            Some((PopoverRow::ProfilesFooter, PopoverPart::Button(button))),
            "{button:?}"
        );
        assert_eq!(v.mouse_down(a, VIEW, x, y, false), outcome, "{button:?}");
    }
}

/// **A Profile row's record names its tokens**: its minimum, the plate's
/// inset, the name cut at a word and the detail elided, the gear's name.
#[test]
fn a_profile_rows_record_names_its_tokens() {
    let v = opened_profiles(3);
    let a = area(dtheme::SIDEBAR_MIN_W);
    let panel = v.popover_panel(a, VIEW).expect("open");
    let mut engine = TextEngine::new();
    let (tree, frame) = v
        .page_at(&mut engine, PopoverKind::Profiles, panel, 0.0, None, None)
        .expect("the page");
    let mut scene = vello::Scene::new();
    let capture = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        koru_draw::scene::rect(0.0, 0.0, VIEW.0, VIEW.1),
    );
    frame.paint(&tree, &mut scene, Affine::IDENTITY, &mut engine);
    let records = capture.finish().records;
    let find = |name: &str| {
        records
            .iter()
            .find(|r| r.kind == "element" && r.name == name)
            .unwrap_or_else(|| panic!("no {name}"))
    };
    let value = |record: &inspect::Record, name: &str| {
        record
            .properties
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.value.clone())
            .unwrap_or_else(|| panic!("{}: no {name}", record.name))
    };
    let row = find("Profile row");
    assert!(
        row.source
            .starts_with("apps/koru/src/panes/explore/popover_elements/profiles.rs:"),
        "{}",
        row.source
    );
    assert_eq!(value(row, "min_h"), "metrics::PROFILE_ROW_MIN -> 44");
    assert_eq!(
        value(row, "pad_x"),
        "alias::ROW_PLATE_INSET = Space::S1 -> 4"
    );
    assert_eq!(
        value(find("Profile name"), "text mode"),
        "single line, elided"
    );
    assert_eq!(value(find("Profile name"), "h"), "Space::S4 -> 16");
    assert_eq!(
        value(find("Profile detail"), "text mode"),
        "single line, elided"
    );
    assert_eq!(value(find("Manage gear"), "name"), "Key::ManageProfile");
    assert_eq!(
        value(find("Sync status words"), "ink"),
        format!(
            "Ink::Accent -> {}",
            inspect::color(Ink::Accent.of(dtheme::p()))
        )
    );
}
