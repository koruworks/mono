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
use crate::panes::explore::{ProfileSummary, WorkspaceEntry};

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

/// **A Workspace row says its organization reading where it already shows
/// state** (root-catalog Plan 2.2): a word after the name while the
/// Workspace is pending, paused, blocked or held, nothing while it is up
/// to date or unknown, and the row's geometry is unchanged by it.
#[test]
fn a_workspace_row_shows_its_organization_reading_only_while_not_up_to_date() {
    use koru_agent_wire::{RootSyncState, RootSyncStatus, RootSyncSubject};
    let mut v = opened(workspaces(2));
    let a = area(dtheme::SIDEBAR_MIN_W);
    let (_, frame) = lay(&v, a);
    assert!(
        frame.named(ORGANIZATION).next().is_none(),
        "nothing said, nothing shown"
    );

    let pending = v.workspaces[0].id;
    let up_to_date = v.workspaces[1].id;
    let subject = |workspace: WorkspaceId, state| RootSyncSubject {
        peer_device_id: "laptop".into(),
        profile_id: None,
        workspace_id: Some(workspace.to_string()),
        state,
    };
    assert!(v.set_root_sync(&RootSyncStatus {
        subjects: vec![
            subject(pending, RootSyncState::Pending),
            subject(up_to_date, RootSyncState::UpToDate),
        ],
    }));
    let (_, frame) = lay(&v, a);
    let words: Vec<_> = frame.named(ORGANIZATION).collect();
    assert_eq!(words.len(), 1, "one row is not up to date");
    let rows: Vec<_> = frame.named(ROW).collect();
    assert_eq!(rows.len(), 2);
    assert!(
        words[0].rect.y >= rows[0].rect.y && words[0].rect.y < rows[0].rect.y + rows[0].rect.h,
        "the word sits in the pending row"
    );
    let gear = frame.node(manage(pending)).expect("the gear");
    assert!(
        words[0].rect.x + words[0].rect.w <= gear.rect.x,
        "the word stays before the trailing lane"
    );
    assert_eq!(
        v.workspace_organization_status(pending),
        Some(super::super::SyncStatus::Pending)
    );
    assert_eq!(v.workspace_organization_status(up_to_date), None);
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
                let now = frame
                    .hit_at(x, y)
                    .and_then(|key| row_part_of(&v, key))
                    .filter(|(row, _)| matches!(row, PopoverRow::Workspace(_)));
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
    assert_eq!(menu.label.as_deref(), Some("Workspaces"));
    assert_eq!(
        menu.size_of_set,
        Some(12),
        "three primaries, three pins, three gears, the heading's manage, New and Import"
    );
    // The heading leads: the menu has no profile header any more.
    let roles: Vec<_> = menu.children.iter().map(|c| c.role).collect();
    assert_eq!(roles[0], AkRole::Heading);
    assert!(
        roles
            .iter()
            .enumerate()
            .all(|(i, r)| i == 0 || *r == AkRole::MenuItem)
    );
    for (i, w) in v.workspaces.iter().enumerate() {
        let primary = &menu.children[2 + i * 3];
        let gear = &menu.children[3 + i * 3];
        let pin = &menu.children[4 + i * 3];
        assert_eq!(
            pin.occurrence,
            Occurrence::model(
                WindowSlot::from_raw(1),
                ViewSlot::from_raw(1),
                w.id.as_uuid(),
                ControlPart::new(PIN)
            )
        );
        assert_eq!(
            pin.label.as_deref(),
            Some(format!("Pin {} to the rail", w.name).as_str())
        );
        assert_eq!(pin.actions, [AkAction::Click, AkAction::Focus]);
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
        assert_eq!(primary.position, Some(1 + i * 3));
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
        koru_i18n::Key::ProfileManageProfiles,
        koru_i18n::Key::ProfileAddNew,
        koru_i18n::Key::ProfileGlobalSync,
        koru_i18n::Key::ProfileAppSettings,
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
                .starts_with("apps/koru/src/panes/explore/popover_elements.rs:")
                || record
                    .source
                    .starts_with("apps/koru/src/panes/explore/workspace_menu_elements.rs:"),
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
    assert_eq!(value(heading, "pad_x"), "alias::NAV_PAD = Space::S2 -> 8");
    let title = elements
        .iter()
        .find(|record| {
            record.properties.iter().any(|property| {
                property.name == "key" && property.value.ends_with("/workspaces-title")
            })
        })
        .expect("the heading title has its own inspectable element");
    assert!(value(title, "ink").starts_with("Ink::Hint -> #"));

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
    for _ in 0..3 {
        v.popover_key(&WKey::Named(NamedKey::ArrowDown));
    }
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

#[test]
fn catalog_refresh_keeps_the_same_workspace_and_pin_part_even_when_its_index_is_unchanged() {
    let mut view = opened(workspaces(3));
    let id = view.workspaces[1].id;
    view.highlight_popover_workspace(id);
    view.popover_part = Some(PopoverPart::Pin);
    let cx = cx(area(dtheme::SIDEBAR_MIN_W), Lang::En);
    let focus = view.accessible_focus(&cx);
    let profile = view.current_profile.unwrap();
    view.set_context(
        view.profiles.clone(),
        profile,
        view.workspaces.clone(),
        view.current,
    );
    assert_eq!(view.accessible_focus(&cx), focus);
    let mut changed = view.workspaces.clone();
    changed[2].pinned = true;
    changed[1].name = "Renamed remotely".into();
    view.set_context(view.profiles.clone(), profile, changed, view.current);
    assert_eq!(view.accessible_focus(&cx), focus);
    assert_eq!(view.popover_part, Some(PopoverPart::Pin));
}

#[test]
fn space_toggles_the_pin_and_a_vanished_row_moves_focus_to_its_neighbor() {
    use winit::keyboard::{Key as WKey, NamedKey};
    let mut view = opened(workspaces(3));
    let id = view.workspaces[1].id;
    let next = view.workspaces[2].id;
    view.highlight_popover_workspace(id);
    view.popover_key(&WKey::Named(NamedKey::ArrowRight));
    assert_eq!(view.popover_part, Some(PopoverPart::Gear));
    view.popover_key(&WKey::Named(NamedKey::ArrowRight));
    assert_eq!(view.popover_part, Some(PopoverPart::Pin));
    assert_eq!(
        view.popover_key(&WKey::Named(NamedKey::Space)),
        Some(Outcome::ToggleWorkspacePin(id))
    );
    assert!(view.popover_open());
    let remaining: Vec<_> = view
        .workspaces
        .iter()
        .filter(|row| row.id != id)
        .cloned()
        .collect();
    let profile = view.current_profile.unwrap();
    view.set_context(view.profiles.clone(), profile, remaining, view.current);
    let cx = cx(area(dtheme::SIDEBAR_MIN_W), Lang::En);
    assert_eq!(
        view.accessible_focus(&cx),
        Some(Occurrence::model(
            WindowSlot::from_raw(1),
            ViewSlot::from_raw(1),
            next.as_uuid(),
            ControlPart::new(PIN)
        ))
    );
    assert_eq!(view.popover_part, Some(PopoverPart::Pin));
    view.set_context(view.profiles.clone(), profile, vec![], None);
    assert_eq!(
        view.popover_hover().map(|i| view.popover_rows()[i]),
        Some(PopoverRow::WorkspacesHeading)
    );
    assert_eq!(view.popover_part, Some(PopoverPart::Gear));
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

/// Profile identities retain their gears and descriptions beside the fixed actions.
#[test]
fn the_profiles_page_puts_each_gear_beside_its_profile_and_lists_fixed_actions() {
    let mut v = opened_profiles(3);
    v.profiles[0].email.clear();
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
        Some(10),
        "Manage profiles, three Profiles, three gears, two fixed actions, device control"
    );
    assert_eq!(menu.children[0].role, AkRole::Heading);
    assert_eq!(menu.children[0].label.as_deref(), Some("Profiles"));
    assert_eq!(menu.children[1].role, AkRole::MenuItem);
    assert_eq!(menu.children[1].label.as_deref(), Some("Manage profiles"));
    assert_eq!(menu.children[1].occurrence.part.name(), "manage-profiles");
    for (i, profile) in v.profiles.iter().enumerate() {
        let primary = &menu.children[2 + i * 2];
        let gear = &menu.children[3 + i * 2];
        assert_eq!(primary.label.as_deref(), Some(profile.name.as_str()));
        assert_eq!(primary.description, Some(v.profile_description(profile)));
        assert_eq!(primary.selected, Some(i == 1), "the open Profile");
        assert_eq!(gear.occurrence, profile_gear(profile.id));
        assert_eq!(gear.label.as_deref(), Some("Manage profile"));
    }
    let actions: Vec<_> = menu.children[8..]
        .iter()
        .map(|node| (node.occurrence.part.name(), node.label.as_deref()))
        .collect();
    assert_eq!(
        actions,
        vec![
            ("new-profile", Some("Add new profile")),
            ("workspace-sync", Some("Devices linked to this computer")),
            ("global-sync", Some("Global sync")),
        ]
    );
    assert_eq!(
        menu.children[9].description.as_deref(),
        Some(v.sync_control_name().as_str())
    );
    assert_eq!(
        menu.children[10].description.as_deref(),
        Some(v.sync_control_name().as_str())
    );
    for node in &menu.children[8..] {
        assert_eq!(node.role, AkRole::MenuItem);
        assert_eq!(node.actions, [AkAction::Click, AkAction::Focus]);
    }
    for profile in &v.profiles {
        let rect = v
            .with_page(a, VIEW, |frame| {
                frame.node(profiles::primary(profile.id)).unwrap().rect
            })
            .unwrap();
        let (x, y) = rect.centre();
        assert_eq!(
            v.menu_tooltip(a, VIEW, x, y).map(|(_, text, _)| text),
            Some(v.profile_description(profile))
        );
    }
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

/// Every fixed action agrees across geometry, pointer, accessibility and keyboard.
#[test]
fn each_profile_action_uses_its_own_row_and_destination() {
    use profiles::MenuAction;
    use winit::keyboard::{Key as WKey, NamedKey};
    let a = area(dtheme::SIDEBAR_MIN_W);
    for (action, outcome) in [
        (MenuAction::New, Outcome::NewProfile),
        (MenuAction::Sync, Outcome::OpenSyncSettings),
    ] {
        let mut v = opened_profiles(3);
        let (_, r) = v
            .popover_rects(a, VIEW)
            .into_iter()
            .find(|(row, _)| *row == PopoverRow::ProfileAction(action))
            .expect("the fixed action row");
        assert_eq!(r.h, dtheme::metrics::ROW_MIN.value);
        v.with_page(a, VIEW, |frame| {
            assert!(near(frame.node(action.key()).unwrap().rect, r));
            for (x, y) in [(r.x + 0.5, r.y + 0.5), (r.x + r.w - 0.5, r.y + r.h - 0.5)] {
                assert_eq!(frame.hit_at(x, y), Some(action.key()));
            }
        })
        .unwrap();
        let (x, y) = r.centre();
        assert_eq!(
            v.popover_part_at(a, VIEW, x, y)
                .map(|(_, row, part)| (row, part)),
            Some((PopoverRow::ProfileAction(action), PopoverPart::Row)),
            "{action:?}"
        );
        assert_eq!(v.mouse_down(a, VIEW, x, y, false), outcome, "{action:?}");
        assert!(!v.popover_open());
        let occurrence = Occurrence::surface(
            WindowSlot::from_raw(1),
            ViewSlot::from_raw(1),
            SURFACE,
            ControlPart::new(action.key().part),
        );
        let mut v = opened_profiles(3);
        assert_eq!(
            v.accessibility_action(a, occurrence, AkAction::Click, VIEW),
            Some(outcome.clone())
        );
        assert!(!v.popover_open());
        for key in [NamedKey::Enter, NamedKey::Space] {
            let mut v = opened_profiles(3);
            assert_eq!(
                v.accessibility_action(a, occurrence, AkAction::Focus, VIEW),
                Some(Outcome::Consumed)
            );
            assert_eq!(v.popover_interaction().hot, Some(action.key()));
            assert_eq!(v.popover_key(&WKey::Named(key)), Some(outcome.clone()));
            assert!(!v.popover_open());
        }
    }
}

#[test]
fn manage_profiles_has_its_own_heading_target_and_opens_the_current_profile() {
    use winit::keyboard::{Key as WKey, NamedKey};
    let a = area(dtheme::SIDEBAR_MIN_W);
    let mut v = opened_profiles(3);
    let current = v.current_profile.unwrap();
    let rect = v
        .with_page(a, VIEW, |frame| {
            frame.node(profiles::MANAGE_PROFILES).unwrap().rect
        })
        .unwrap();
    let (x, y) = rect.centre();
    assert_eq!(
        v.popover_part_at(a, VIEW, x, y)
            .map(|(_, row, part)| (row, part)),
        Some((PopoverRow::ProfilesHeading, PopoverPart::Gear))
    );
    assert_eq!(
        v.mouse_down(a, VIEW, x, y, false),
        Outcome::ManageProfile(current)
    );
    assert!(!v.popover_open());
    for key in [NamedKey::Enter, NamedKey::Space] {
        let mut v = opened_profiles(3);
        v.popover_key(&WKey::Named(NamedKey::ArrowDown));
        assert_eq!(v.popover_interaction().hot, Some(profiles::MANAGE_PROFILES));
        assert_eq!(
            v.popover_key(&WKey::Named(key)),
            Some(Outcome::ManageProfile(current))
        );
        assert!(!v.popover_open());
    }
}

#[test]
fn profile_actions_stay_fixed_below_the_list_with_one_divider_after_creation() {
    use profiles::MenuAction;
    let a = area(dtheme::SIDEBAR_MIN_W);
    let mut v = opened_profiles(40);
    let actions = |v: &ExploreView| {
        v.popover_rects(a, VIEW)
            .into_iter()
            .filter(|(row, _)| matches!(row, PopoverRow::ProfileAction(_)))
            .collect::<Vec<_>>()
    };
    let before = actions(&v);
    assert!(v.scroll_popover(a, VIEW, -400.0));
    assert_eq!(actions(&v), before);
    assert_eq!(
        before.iter().map(|(row, _)| *row).collect::<Vec<_>>(),
        [
            PopoverRow::ProfileAction(MenuAction::New),
            PopoverRow::ProfileAction(MenuAction::Sync)
        ]
    );
    let [(_, new), (_, sync)] = before.as_slice() else {
        panic!("two fixed actions");
    };
    let divider = dtheme::Space::S1.px(dtheme::Profile::Desktop) * 2.0 + dtheme::BORDER_W as f32;
    assert_eq!(sync.y - (new.y + new.h), divider);
}

/// **A Profile row's record names its tokens**: its minimum, the plate's
/// inset, one elided name, and the gear's name. Identity details are not painted.
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
    assert_eq!(value(row, "min_h"), "metrics::ROW_MIN -> 26");
    assert_eq!(
        value(row, "pad_x"),
        "alias::ROW_PLATE_INSET = Space::S1 -> 4"
    );
    assert_eq!(
        value(find("Profile name"), "text mode"),
        "single line, elided"
    );
    assert!(
        !records
            .iter()
            .any(|record| record.name == "Profile detail" || record.name == "Profile lines")
    );
    assert_eq!(value(find("Profile name"), "lines"), "1");
    assert_eq!(value(find("Manage gear"), "name"), "Key::ManageProfile");
}

// ------------------------------------------- recovery (root catalog Plan 1.3)
use crate::panes::explore::workspace_menu_elements;

fn recovery_controller(unassigned: koru_core::WorkspaceId) -> crate::manage::SharedWorkspaces {
    use koru_workspaces_ui::{DeletedProfileRow, Recovery, UnassignedRow, WorkspacesController};
    let former = koru_store_core::ids::ProfileId::from_uuid(uuid(901));
    let mut controller = WorkspacesController::new();
    controller.fixture_recovery(Recovery {
        unassigned: vec![UnassignedRow {
            id: crate::manage::workspaces::catalog_workspace(unassigned),
            name: "Orphan".into(),
            icon: None,
            color: None,
            former_profile: Some(former),
            reason: koru_api_client::UnassignedReason::ProfileDeleted,
        }],
        deleted_profiles: vec![DeletedProfileRow {
            profile: former,
            name: Some("Work".into()),
            deletion: koru_store_core::ids::DeletionId::from_uuid(uuid(902)),
            at_ms: 1_000,
            retention_ms: 30,
            targets: 2,
            expired_or_purged: 0,
        }],
    });
    std::rc::Rc::new(std::cell::RefCell::new(controller))
}

/// The Unassigned group leads the menu (root-catalog §3.6): its heading and
/// rows come before this Profile's own, each row is reachable by the
/// keyboard and named for a reader, and without anything unassigned there is
/// no group at all.
#[test]
fn the_unassigned_group_leads_the_menu_and_is_absent_without_anything_unassigned() {
    let orphan = WorkspaceId::from_uuid(uuid(77));
    let mut v = opened(workspaces(2));
    let a = area(dtheme::SIDEBAR_MIN_W);
    let (_, frame) = lay(&v, a);
    assert!(
        frame
            .node(workspace_menu_elements::UNASSIGNED_HEADING)
            .is_none()
    );
    assert!(
        !v.rows_for(PopoverKind::Summary)
            .iter()
            .any(|row| matches!(row, PopoverRow::Unassigned(_)))
    );

    v.set_workspace_controller(recovery_controller(orphan));
    let rows = v.rows_for(PopoverKind::Summary);
    let heading = rows
        .iter()
        .position(|row| *row == PopoverRow::WorkspacesHeading)
        .unwrap();
    assert_eq!(
        &rows[heading - 3..heading],
        &[
            PopoverRow::Unassigned(0),
            PopoverRow::UnassignedMove(0),
            PopoverRow::UnassignedRestore(0),
        ],
        "the group sits right above this Profile's heading, and nothing of this Profile's above it"
    );
    assert!(
        rows[..heading - 3]
            .iter()
            .all(|row| matches!(row, PopoverRow::WorkspaceAction(_))),
        "only the header's sync controls precede it"
    );

    let (_, frame) = lay(&v, a);
    let group_heading = frame
        .node(workspace_menu_elements::UNASSIGNED_HEADING)
        .expect("the group's heading");
    let own_heading = frame.node(HEADING).expect("this Profile's heading");
    assert!(group_heading.rect.y < own_heading.rect.y);
    let row_rect = frame
        .node(workspace_menu_elements::unassigned(orphan))
        .expect("the row")
        .rect;
    let move_rect = frame
        .node(workspace_menu_elements::unassigned_move(orphan))
        .expect("Move to…")
        .rect;
    let restore_rect = frame
        .node(workspace_menu_elements::unassigned_restore(orphan))
        .expect("Restore Profile")
        .rect;
    assert!(row_rect.y < move_rect.y && move_rect.y < restore_rect.y);
    // The page's rows and the frame's rects agree on every one of them.
    let page_rows = v.page_rows(&frame);
    for row in [
        PopoverRow::Unassigned(0),
        PopoverRow::UnassignedMove(0),
        PopoverRow::UnassignedRestore(0),
    ] {
        assert!(
            page_rows.iter().any(|(r, _)| *r == row),
            "{row:?} is placed"
        );
    }
    // A press on each is what it says: Manage, the destination menu, the
    // batch restore of the former Profile.
    assert_eq!(
        v.choose(PopoverRow::Unassigned(0), PopoverPart::Row),
        Outcome::ManageWorkspace(orphan)
    );
    assert_eq!(
        v.choose(PopoverRow::UnassignedMove(0), PopoverPart::Row),
        Outcome::MoveUnassigned(orphan)
    );
    assert!(matches!(
        v.choose(PopoverRow::UnassignedRestore(0), PopoverPart::Row),
        Outcome::RestoreProfile { profile, .. } if profile.as_uuid() == uuid(901)
    ));

    // Named for a reader, in order: the heading, then the row and its two
    // actions as menu items.
    let cx = cx(a, Lang::En);
    let nodes = v.element_semantics(&cx, &frame);
    let menu = &nodes[0];
    let labels: Vec<&str> = menu
        .children
        .iter()
        .filter_map(|child| child.label.as_deref())
        .collect();
    let at = labels
        .iter()
        .position(|label| *label == "Unassigned (from deleted Profile)")
        .expect("the group heading is published");
    assert_eq!(
        menu.children[at].role,
        AkRole::Heading,
        "a heading, like this Profile's own"
    );
    assert_eq!(labels[at + 1], "Orphan, Unassigned (from deleted Profile)");
    assert_eq!(labels[at + 2], "Move to…: Orphan");
    assert_eq!(labels[at + 3], "Restore Profile: Orphan");
    for child in &menu.children[at + 1..at + 4] {
        assert_eq!(child.role, AkRole::MenuItem);
        assert_eq!(child.actions, [AkAction::Click, AkAction::Focus]);
    }
    assert!(
        labels
            .iter()
            .position(|label| *label == "Workspaces")
            .unwrap()
            > at,
        "the group comes first"
    );
    assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
}

/// An empty Profile says so where its rows would be (root-catalog §3.10),
/// as furniture the keyboard skips; *New workspace* in the foot is the one
/// way out, and nothing is created by opening the menu.
#[test]
fn an_empty_profile_says_so_in_its_list_and_offers_only_new_workspace() {
    let v = opened(workspaces(0));
    let rows = v.rows_for(PopoverKind::Summary);
    assert!(rows.contains(&PopoverRow::WorkspacesEmpty));
    assert!(!PopoverRow::WorkspacesEmpty.selectable());
    assert!(rows.contains(&PopoverRow::WorkspaceAction(
        workspace_menu_elements::MenuAction::New
    )));
    let a = area(dtheme::SIDEBAR_MIN_W);
    let (_, frame) = lay(&v, a);
    let line = frame
        .node(workspace_menu_elements::EMPTY)
        .expect("the empty line is laid out");
    assert!(frame.node(HEADING).unwrap().rect.y < line.rect.y);
    let cx = cx(a, Lang::En);
    let nodes = v.element_semantics(&cx, &frame);
    assert!(
        nodes[0]
            .children
            .iter()
            .any(|child| child.label.as_deref() == Some("This profile has no workspaces")),
        "a reader hears why the list is empty"
    );
    let full = opened(workspaces(1));
    assert!(
        !full
            .rows_for(PopoverKind::Summary)
            .contains(&PopoverRow::WorkspacesEmpty)
    );
}
