// SPDX-License-Identifier: MPL-2.0

//! Two halves.
//!
//! The **geometry** is driven over a synthetic row cache, so it needs no
//! document — and therefore no font and no disk. The **rebuild** tests need a
//! real `WorkspaceDoc`, because what they are about is which nodes become rows;
//! they build one in memory through `koru-core`, since this crate deliberately
//! cannot see `koru-workspace`.

use std::time::Duration;

use super::*;
use crate::panes::sidebar::apps_rail;

#[test]
fn enrolled_empty_profile_explains_pending_sync_and_keeps_workspace_creation() {
    let mut view = ExploreView::new();
    let profile = ProfileId::new();
    view.current_profile = Some(profile);
    view.set_account_state(&koru_agent_wire::AccountState {
        enrollments: vec![koru_agent_wire::AccountProfileEnrollment {
            profile_id: profile.to_storage(),
            state: koru_agent_wire::EnrollmentState::Complete,
            failure_code: None,
            retry_at_millis: None,
        }],
        ..Default::default()
    });
    let (message, label, action) = view.empty_state();
    assert!(message.contains("device that has them comes online"));
    assert_eq!(label, "Create workspace");
    assert_eq!(action, Outcome::NewWorkspace);
    view.current = Some(WorkspaceId::new());
    assert!(view.empty_state().0.contains("Synced documents"));
    assert_eq!(view.empty_state().2, Outcome::NewDocument);
    view.current_profile = Some(ProfileId::new());
    assert!(
        !view
            .empty_state()
            .0
            .contains("device that has them comes online")
    );
}
use koru_core::node::{BlobHash, Node};
use koru_core::{ProfileId, WorkspaceId};
use winit::keyboard::{Key, NamedKey};

/// A point on a row's **label**: right of the caret column, and clear of the
/// `…`'s reserved lane at the far right.
///
/// The far right used to be a fine proxy for "on the label" and is not any
/// more: every row now reserves 20 px at its end for the menu button, whether
/// or not the button is showing, and a press there means the menu.
fn on_label(r: Rect) -> f32 {
    r.x + r.w * 0.5
}

/// Lay the column out once.
///
/// Needed because the empty-state button's rect depends on how its sentence
/// wraps at this column's width, so it is measured during the draw and cached —
/// the hit test has no engine to measure with. Same reason `WelcomeView` caches
/// its font chips.
fn draw_once(v: &mut ExploreView, a: Rect) {
    let mut scene = vello::Scene::new();
    let mut engine = TextEngine::new();
    v.draw(&mut scene, vello::kurbo::Affine::IDENTITY, &mut engine, a);
}

fn area() -> Rect {
    Rect::new(0.0, theme::CAPTION_H, theme::SIDEBAR_W, 600.0)
}

/// The app under a point in the rail beside `a`, through the same hit test a
/// press uses; `None` for the gaps and for the foot's controls.
fn mode_at(v: &ExploreView, a: Rect, x: f32, y: f32) -> Option<NavigatorMode> {
    match apps_rail::hit(&v.apps_rail_rects(v.rail_of(a)), x, y) {
        Some(apps_rail::Hit::App(mode)) => Some(mode),
        _ => None,
    }
}

/// `n` rows, alternating depth so the indent arithmetic is exercised too.
fn with_rows(n: usize) -> ExploreView {
    let mut v = ExploreView::new();
    v.rows = (0..n)
        .map(|i| Row {
            node: NodeId::new(),
            depth: (i % 3) as u16,
            label: format!("node {i}"),
            kind: Kind::Document,
            mime: None,
            icon: None,
            has_children: i % 2 == 0,
            expanded: true,
            file_state: FileState::Present,
            note: None,
        })
        .collect();
    v
}

/// The round-trip that stops the list being off by one — the same shape as the
/// date picker's `every_day_cell_hit_tests_back_to_its_own_date`.
#[test]
fn every_visible_row_hit_tests_back_to_its_own_index() {
    let v = with_rows(12);
    let a = area();
    let rects = v.row_rects(a);
    assert!(!rects.is_empty());
    for (i, r) in rects {
        let (x, y) = r.centre();
        assert_eq!(v.row_at(a, x, y), Some(i), "row {i}");
    }
}

#[test]
fn rows_run_top_to_bottom_without_overlapping() {
    let v = with_rows(10);
    let rects = v.row_rects(area());
    for pair in rects.windows(2) {
        let (a, b) = (pair[0].1, pair[1].1);
        assert!(a.y + a.h <= b.y + f32::EPSILON);
    }
}

/// A library of a thousand nodes must cost a screenful of rows per frame, not a
/// thousand — the same culling `rowlayout::visible_range` does for the editor.
#[test]
fn only_the_visible_band_is_produced() {
    let v = with_rows(1000);
    let a = area();
    let rects = v.row_rects(a);
    let fits = (a.h / theme::NAV_ROW_H).ceil() as usize;
    assert!(
        rects.len() <= fits + 3,
        "{} rows produced for a {}px column",
        rects.len(),
        a.h
    );
    for (_, r) in &rects {
        assert!(r.y + r.h > a.y && r.y < a.y + a.h, "a row outside the band");
    }
}

/// …and scrolling moves which ones, without ever running past the ends.
#[test]
fn scrolling_moves_the_rows_and_stops_at_both_ends() {
    let mut v = with_rows(200);
    let a = area();
    let first = |v: &ExploreView| v.row_rects(a).first().map(|(i, _)| *i).unwrap();
    assert_eq!(first(&v), 0);

    v.scroll_by(a, -theme::NAV_ROW_H * 10.0);
    assert!(first(&v) > 0, "scrolling down moved the window");

    // Past the top clamps to the first row.
    v.scroll_by(a, 10_000.0);
    assert_eq!(v.scroll, 0.0);
    assert_eq!(first(&v), 0);

    // And past the bottom stops with content still on screen.
    v.scroll_by(a, -1_000_000.0);
    let content = 200.0 * theme::NAV_ROW_H;
    assert!(v.scroll <= content, "scrolled past everything");
    assert!(!v.row_rects(a).is_empty(), "the column went blank");
}

/// A short list cannot scroll at all — otherwise the column drifts away from
/// its own heading.
#[test]
fn a_list_that_fits_does_not_scroll() {
    let mut v = with_rows(3);
    v.scroll_by(area(), -500.0);
    assert_eq!(v.scroll, 0.0);
    assert!(v.scrollbar_geom(area()).is_none());
}

#[test]
fn documents_actions_menu_opens_and_routes_every_action() {
    let mut v = with_rows(3);
    let a = area();
    let button = v.actions_button(a);
    assert!(button.w > 0.0);

    let (x, y) = button.centre();
    assert!(v.mouse_move(a, VIEW, x, y));
    assert_eq!(v.cursor_icon(), koru_ui::cursor::PRESS);
    assert_eq!(v.mouse_down(a, VIEW, x, y, false), Outcome::Consumed);
    assert_eq!(
        v.popover_rows(),
        vec![
            PopoverRow::LibraryAction(actions::Action::Import),
            PopoverRow::LibraryAction(actions::Action::CollapseAll),
            PopoverRow::LibraryAction(actions::Action::ExpandAll),
        ]
    );
    let import = v.popover_rects(a, VIEW)[0].1.centre();
    assert_eq!(
        v.mouse_down(a, VIEW, import.0, import.1, false),
        Outcome::ImportContent
    );

    v.toggle_popover(PopoverKind::LibraryActions);
    let collapse = v.popover_rects(a, VIEW)[1].1.centre();
    assert_eq!(
        v.mouse_down(a, VIEW, collapse.0, collapse.1, false),
        Outcome::Consumed
    );
    assert_eq!(v.pending_fold, Some(fold::Fold::Collapse));
}

#[test]
fn files_has_the_same_import_entry_point_for_multi_file_picker() {
    let mut v = with_rows(3);
    v.show_files();
    let a = area();
    let button = v.actions_button(a);
    assert!(button.w > 0.0);

    let (x, y) = button.centre();
    assert!(v.mouse_move(a, VIEW, x, y));
    assert_eq!(v.cursor_icon(), koru_ui::cursor::PRESS);
    assert_eq!(v.mouse_down(a, VIEW, x, y, false), Outcome::Consumed);
    assert_eq!(
        v.popover_rows(),
        vec![PopoverRow::LibraryAction(actions::Action::Import)]
    );
    assert!(
        v.popover_list_band(a, VIEW).is_none(),
        "the one-command menu has no footer divider"
    );
    let import = v.popover_rects(a, VIEW)[0].1.centre();
    assert_eq!(
        v.mouse_down(a, VIEW, import.0, import.1, false),
        Outcome::ImportFiles
    );
}

#[test]
fn the_library_scrollbar_uses_the_rows_band_and_drags_to_the_end() {
    let mut v = with_rows(200);
    let a = area();
    let g = v.scrollbar_geom(a).expect("long library has a scrollbar");
    assert_eq!(g.track_top, v.rows_top(a));
    assert_eq!(g.track_bottom, v.rows_bottom(a));

    let x = g.gutter_x + g.gutter_w * 0.5;
    let y = g.thumb_top + g.thumb_h * 0.5;
    assert!(v.mouse_move(a, VIEW, x, y));
    assert_eq!(v.cursor_icon(), CursorIcon::NsResize);
    assert_eq!(v.mouse_down(a, VIEW, x, y, false), Outcome::Consumed);
    assert!(v.pointer_active());

    assert!(v.mouse_move(a, VIEW, x, g.track_bottom + 100.0));
    assert!((v.scroll - g.max_scroll).abs() < 0.01);
    assert_eq!(v.mouse_up(), Outcome::Consumed);
    assert!(!v.pointer_active());
}

/// The caret is inside the row, so it has to be asked first or expanding is
/// unreachable — and it must *not* open the node, or every expand navigates.
#[test]
fn the_caret_toggles_without_opening_the_node() {
    let mut v = with_rows(4);
    let a = area();
    let (i, r) = v
        .row_rects(a)
        .into_iter()
        .find(|(i, _)| i % 2 == 0)
        .unwrap();
    let row = v.rows[i].clone();
    assert!(row.has_children, "the fixture row has children");

    let (cx, cy) = v.caret_rect(&row, r).centre();
    assert_eq!(
        v.mouse_down(a, VIEW, cx, cy, false),
        Outcome::Consumed,
        "not a navigation"
    );
    assert!(v.collapsed.contains(&row.node), "it collapsed");

    assert_eq!(v.mouse_down(a, VIEW, cx, cy, false), Outcome::Consumed);
    assert!(!v.collapsed.contains(&row.node), "and expanded again");
}

/// Pressing a document row seeds a latent drag (it does not open on press any
/// longer); the open happens on release if the pointer never crossed the arm
/// threshold, preserving the click-to-open gesture.
#[test]
fn pressing_a_document_row_opens_on_release() {
    let mut v = with_rows(4);
    let a = area();
    let (i, r) = v.row_rects(a).into_iter().next().unwrap();
    let node = v.rows[i].node;
    // Well right of the caret column, on the label.
    let x = on_label(r);
    assert_eq!(
        v.mouse_down(a, VIEW, x, r.centre().1, false),
        Outcome::Consumed,
        "a document press seeds a latent drag, not an open"
    );
    assert!(v.drag_active(), "the drag is latent");
    assert!(!v.drag_armed(), "but not yet armed");
    assert_eq!(
        v.mouse_up(),
        Outcome::OpenNode(node),
        "a release under the arm threshold opens the node"
    );
    assert!(!v.drag_active());
}

/// A single click on an image is plain navigation.
#[test]
fn clicking_an_image_row_opens_it() {
    let mut v = with_rows(1);
    v.rows[0].kind = Kind::Image;
    let node = v.rows[0].node;
    let (_, r) = v.row_rects(area()).into_iter().next().unwrap();
    assert_eq!(
        v.mouse_down(area(), VIEW, on_label(r), r.centre().1, false),
        Outcome::OpenNode(node)
    );
}

#[test]
fn double_clicking_an_image_row_requests_rename() {
    let mut v = with_rows(1);
    v.rows[0].kind = Kind::Image;
    let node = v.rows[0].node;
    let (_, r) = v.row_rects(area()).into_iter().next().unwrap();
    assert_eq!(
        v.mouse_double_click(area(), VIEW, on_label(r), r.centre().1),
        Outcome::RenameImage(node)
    );
}

/// A leaf has no caret, so its whole row is a target — a zero-sized caret rect
/// is what makes that fall out rather than needing a second code path.
#[test]
fn a_leaf_row_has_no_caret_to_hit() {
    let v = with_rows(4);
    let a = area();
    let (i, r) = v
        .row_rects(a)
        .into_iter()
        .find(|(i, _)| i % 2 == 1)
        .unwrap();
    let row = v.rows[i].clone();
    assert!(!row.has_children);
    assert_eq!(v.caret_rect(&row, r).w, 0.0);
}

/// A press on the column's background is still the column's: it must not fall
/// through to whatever is behind it.
#[test]
fn a_press_on_the_background_is_consumed_not_ignored() {
    let mut v = with_rows(1);
    let a = area();
    let below = a.y + a.h - 2.0;
    assert_eq!(
        v.mouse_down(a, VIEW, a.x + 4.0, below, false),
        Outcome::Consumed
    );
}

/// A collapsed column is a zero-width rect, and a row must not be reachable in
/// one — `Rect::contains` is inclusive on every edge.
#[test]
fn nothing_is_hit_in_a_collapsed_column() {
    let v = with_rows(5);
    let shut = Rect::new(0.0, theme::CAPTION_H, 0.0, 600.0);
    assert!(v.row_rects(shut).iter().all(|(_, r)| r.w <= 0.0));
    assert_eq!(v.row_at(shut, 0.0, theme::CAPTION_H + 40.0), None);
}

/// Hovering a row does **not** change the pointer, and that is the rule rather
/// than an oversight: Koru does not use the hand cursor, so a row says it is
/// pressable with its hover background instead (`docs/features/design/DESIGN.md`). The grab
/// hand is still one press-and-hold away — see the test below.
#[test]
fn hovering_a_row_leaves_the_pointer_alone() {
    let mut v = with_rows(3);
    let a = area();
    assert_eq!(v.cursor_icon(), CursorIcon::Default);
    let (_, r) = v.row_rects(a).into_iter().next().unwrap();
    assert!(v.mouse_move(a, VIEW, r.centre().0, r.centre().1));
    assert_eq!(v.cursor_icon(), CursorIcon::Default);
    assert_ne!(v.cursor_icon(), CursorIcon::Pointer);
    assert!(v.clear_hover());
    assert_eq!(v.cursor_icon(), CursorIcon::Default);
}

#[test]
fn a_held_press_reveals_the_grab_hand() {
    let mut v = with_rows(2);
    let a = area();
    let y = row_centre(&v, a, 0);
    v.mouse_down(a, VIEW, on_label(a), y, false);
    // Immediately after the press: still the ordinary cursor.
    assert_eq!(v.cursor_icon(), CursorIcon::Default);
    // Past the hold threshold: the grab hand appears.
    let d = v.drag.as_mut().expect("latent drag");
    d.pressed_at = Instant::now() - std::time::Duration::from_millis(120);
    assert_eq!(v.cursor_icon(), CursorIcon::Grab);
    // And it is armed once the pointer moves, becoming the closed hand.
    let d = v.drag.as_mut().expect("latent drag");
    d.armed = true;
    assert_eq!(v.cursor_icon(), CursorIcon::Grabbing);
}

// ============================================================ what earns a row
//
// These need a real document, because what they are about is which nodes become
// rows at all.

/// An empty in-memory workspace.
fn doc_ws() -> WorkspaceDoc {
    WorkspaceDoc::create(WorkspaceId::new(), "t").expect("create")
}

/// A document in the library, with `paragraphs` lines under it.
fn document(ws: &mut WorkspaceDoc, title: &str, paragraphs: usize) -> NodeId {
    let doc = ws
        .insert(Parent::Root, Node::document(title).unwrap())
        .unwrap();
    for i in 0..paragraphs {
        ws.insert(
            Parent::Node(doc),
            Node::paragraph(format!("line {i}")).unwrap(),
        )
        .unwrap();
    }
    doc
}

fn add_image(ws: &mut WorkspaceDoc, parent: NodeId) -> NodeId {
    ws.insert(
        Parent::Node(parent),
        Node::image(BlobHash("hash".into()), "image/png", Some(80), Some(60)).unwrap(),
    )
    .unwrap()
}

fn labels(v: &ExploreView) -> Vec<&str> {
    v.rows.iter().map(|r| r.label.as_str()).collect()
}

fn built(ws: &WorkspaceDoc) -> ExploreView {
    let mut v = ExploreView::new();
    v.rebuild(ws);
    v
}

/// **The change.** A document's paragraphs are its text, not places to jump to,
/// and one long document would otherwise bury every other thing in the library.
#[test]
fn a_documents_paragraphs_do_not_earn_rows() {
    let mut ws = doc_ws();
    document(&mut ws, "spec", 200);
    document(&mut ws, "notes", 3);

    let v = built(&ws);
    assert_eq!(labels(&v), ["spec", "notes"], "two documents, no prose");
}

/// The numbering rules for images, code blocks and lists, asked of the function
/// directly.
///
/// They used to be asked through `rebuild`, when those kinds still earned rows.
/// They do not today — the column is documents-only — so this is what keeps the
/// naming honest until they come back. See ROADMAP.
#[test]
fn an_unnamed_block_is_numbered_and_a_name_replaces_the_number() {
    let img = Node::image(BlobHash("hash".into()), "image/png", Some(80), Some(60)).unwrap();
    assert_eq!(label_for(&img, 2, 0), "Image 2");

    let mut named = Node::image(BlobHash("hash".into()), "image/png", None, None).unwrap();
    named.meta.name = Some("System diagram".into());
    assert_eq!(label_for(&named, 2, 0), "System diagram");

    assert_eq!(
        label_for(&Node::code("fn one() {}").unwrap(), 0, 3),
        "Code 3"
    );
    assert_eq!(label_for(&Node::divider().unwrap(), 0, 0), "Divider");

    // Everything else is its own text, and an empty one is not left blank.
    assert_eq!(label_for(&Node::document("spec").unwrap(), 0, 0), "spec");
    assert_eq!(label_for(&Node::document("").unwrap(), 0, 0), "Untitled");
}

/// A leading emoji in the title is the tree's page icon, stripped from the
/// label so the row does not show the glyph twice.
#[test]
fn a_leading_title_emoji_replaces_the_document_icon() {
    let mut ws = doc_ws();
    let id = document(&mut ws, "💻Craft.do Pricing", 0);
    let v = built(&ws);
    let row = v.rows.iter().find(|r| r.node == id).unwrap();
    assert_eq!(row.icon.as_deref(), Some("💻"));
    assert_eq!(row.label, "Craft.do Pricing");
}

/// Mid-title emoji stays in the label; the icon slot falls through to the
/// kind glyph (no `meta.icon`).
#[test]
fn a_mid_title_emoji_stays_in_the_label() {
    let mut ws = doc_ws();
    let id = document(&mut ws, "Price 💻", 0);
    let v = built(&ws);
    let row = v.rows.iter().find(|r| r.node == id).unwrap();
    assert_eq!(row.icon, None);
    assert_eq!(row.label, "Price 💻");
}

/// The F2 page icon is only the fallback when the title has no leading emoji.
#[test]
fn title_emoji_wins_over_the_page_icon() {
    let mut ws = doc_ws();
    let id = document(&mut ws, "🚀 Launch", 0);
    ws.set_icon(id, Some("🔥")).unwrap();
    let v = built(&ws);
    let row = v.rows.iter().find(|r| r.node == id).unwrap();
    assert_eq!(row.icon.as_deref(), Some("🚀"));
    assert_eq!(row.label, "Launch");
}

#[test]
fn page_icon_is_used_when_the_title_has_no_leading_emoji() {
    let mut ws = doc_ws();
    let id = document(&mut ws, "Notes", 0);
    ws.set_icon(id, Some("📝")).unwrap();
    let v = built(&ws);
    let row = v.rows.iter().find(|r| r.node == id).unwrap();
    assert_eq!(row.icon.as_deref(), Some("📝"));
    assert_eq!(row.label, "Notes");
}

/// A caret that expands to nothing is worse than no caret: it says there is
/// something here when there is not.
#[test]
fn a_document_of_pure_prose_shows_no_caret() {
    let mut ws = doc_ws();
    let bare = document(&mut ws, "prose", 20);
    let parent = document(&mut ws, "illustrated", 2);
    ws.insert(Parent::Node(parent), Node::document("inner").unwrap())
        .unwrap();

    let v = built(&ws);
    let row = |id| v.rows.iter().find(|r| r.node == id).expect("row");
    assert!(!row(bare).has_children, "nothing under it earns a row");
    assert!(row(parent).has_children, "a nested document does");
}

/// …and the answer must be the same when the node is collapsed, which is the
/// one case the row cache cannot supply it from.
#[test]
fn a_collapsed_document_still_knows_it_has_something_inside() {
    let mut ws = doc_ws();
    let doc = document(&mut ws, "outer", 2);
    ws.insert(Parent::Node(doc), Node::document("inner").unwrap())
        .unwrap();

    let mut v = built(&ws);
    v.collapsed.insert(doc);
    v.rebuild(&ws);

    assert_eq!(labels(&v), ["outer"], "the nested document is hidden");
    assert!(
        v.rows[0].has_children,
        "but the caret has to stay, or there is no way to open it again"
    );
}

/// People remain workspace data, but Explore is document navigation rather
/// than a people directory.
#[test]
fn contacts_are_hidden_from_explore() {
    let mut ws = doc_ws();
    ws.insert(
        Parent::Root,
        Node::new(Kind::Contact, koru_core::Content::text("Kim")).unwrap(),
    )
    .unwrap();
    document(&mut ws, "spec", 5);

    let v = built(&ws);
    assert_eq!(labels(&v), ["spec"]);
}

/// **Documents, and nothing else.** Images, code blocks and the rest used to
/// earn a row each; the column filled with structure that has nowhere to go yet.
/// Deliberately temporary — see ROADMAP.
#[test]
fn only_documents_earn_a_row() {
    let mut ws = doc_ws();
    let doc = document(&mut ws, "spec", 2);
    add_image(&mut ws, doc);

    assert_eq!(labels(&built(&ws)), ["spec"]);
}

/// **The whole tree, not the visible part of it.** Collapse all has to reach the
/// folds hidden behind other folds, or reopening the top document would reveal a
/// subtree still spread out underneath it.
#[test]
fn collapse_all_folds_nodes_that_have_no_row_yet() {
    let mut ws = doc_ws();
    let root = document(&mut ws, "root", 0);
    let child = ws
        .insert(Parent::Node(root), Node::document("child").unwrap())
        .unwrap();
    ws.insert(Parent::Node(child), Node::document("grand").unwrap())
        .unwrap();

    let mut v = built(&ws);
    assert_eq!(labels(&v), ["root", "child", "grand"]);

    v.pending_fold = Some(fold::Fold::Collapse);
    v.rebuild(&ws);
    assert_eq!(labels(&v), ["root"], "only the top level is left");
    // `child` has no row at all now, and this is the assertion that matters:
    // a session capture that filtered against the rows would forget it.
    assert!(v.collapsed().contains(&child));
    assert!(v.collapsed().contains(&root));
}

/// Expand all is the other half, and it is total for the same reason.
#[test]
fn expand_all_unfolds_everything() {
    let mut ws = doc_ws();
    let root = document(&mut ws, "root", 0);
    let child = ws
        .insert(Parent::Node(root), Node::document("child").unwrap())
        .unwrap();
    ws.insert(Parent::Node(child), Node::document("grand").unwrap())
        .unwrap();

    let mut v = built(&ws);
    v.pending_fold = Some(fold::Fold::Collapse);
    v.rebuild(&ws);
    v.pending_fold = Some(fold::Fold::Expand);
    v.rebuild(&ws);

    assert_eq!(labels(&v), ["root", "child", "grand"]);
    assert!(v.collapsed().is_empty());
}

/// A leaf earns no fold: collapsing everything must not leave a caret on a
/// document with nothing under it.
#[test]
fn collapse_all_leaves_childless_documents_alone() {
    let mut ws = doc_ws();
    let lone = document(&mut ws, "lone", 3); // paragraphs, not documents

    let mut v = built(&ws);
    v.pending_fold = Some(fold::Fold::Collapse);
    v.rebuild(&ws);
    assert!(!v.collapsed().contains(&lone));
}

/// The set is pruned against the *document*, not the rows — that is the whole
/// reason the pruning moved out of `collapsed()`.
#[test]
fn a_deleted_node_leaves_the_collapsed_set() {
    let mut ws = doc_ws();
    let root = document(&mut ws, "root", 0);
    let child = ws
        .insert(Parent::Node(root), Node::document("child").unwrap())
        .unwrap();

    let mut v = built(&ws);
    v.collapsed.insert(child);
    ws.delete(child).unwrap();
    v.rebuild(&ws);

    assert!(!v.collapsed().contains(&child));
}

/// A non-document is recursed **through**, not skipped: a document nested under
/// a paragraph keeps its row, flattened to the depth of its nearest document
/// ancestor rather than vanishing with its parent.
#[test]
fn a_document_under_a_hidden_node_survives_at_its_parents_depth() {
    let mut ws = doc_ws();
    let outer = document(&mut ws, "outer", 1);
    let para = ws.children(Parent::Node(outer)).unwrap().pop().unwrap();
    let inner = ws
        .insert(Parent::Node(para), Node::document("inner").unwrap())
        .unwrap();

    let v = built(&ws);
    assert_eq!(labels(&v), ["outer", "inner"]);
    let depths: Vec<u16> = v.rows.iter().map(|r| r.depth).collect();
    assert_eq!(
        depths,
        vec![0, 1],
        "one under the document, not two under the line"
    );
    assert_eq!(v.rows[1].node, inner);
}

// -------------------------------------------------- the header and the popovers

/// The window the popovers are clamped inside.
const VIEW: (f32, f32) = (1200.0, 700.0);

fn a_profile(name: &str) -> ProfileSummary {
    ProfileSummary {
        id: ProfileId::new(),
        name: name.into(),
        email: format!("{}@example.com", name.to_lowercase()),
        avatar: None,
        icon: None,
        color: None,
        account: None,
        folder: format!("C:/dev/{name}"),
    }
}

fn ws(name: &str) -> WorkspaceEntry {
    WorkspaceEntry::new(WorkspaceId::new(), name, 0)
}

/// A pane with three profiles, two workspaces and some library rows — the shape
/// the bug was reported against.
fn with_context(rows: usize) -> (ExploreView, Vec<ProfileSummary>, Vec<WorkspaceEntry>) {
    let mut v = with_rows(rows);
    let profiles = vec![
        a_profile("koru"),
        a_profile("dev-data"),
        a_profile("space-b"),
    ];
    let list = vec![ws("Example"), ws("Client work")];
    v.set_context(profiles.clone(), profiles[1].id, list.clone(), list[0].id);
    (v, profiles, list)
}

/// `Rect::contains` is inclusive on every edge, so a zero-width floor would
/// answer for clicks meant for the view beside it.
#[test]
fn nothing_in_the_floor_is_hit_in_a_collapsed_column() {
    let (v, _, _) = with_context(4);
    let shut = Rect::new(0.0, 0.0, 0.0, 600.0);
    assert_eq!(crate::panes::sidebar::floor::hit(shut, 0.0, 580.0), None);
    assert_eq!(v.popover_panel(shut, VIEW), None);
    assert!(v.popover_rects(shut, VIEW).is_empty());
    assert!(v.row_rects(shut).is_empty());
}

/// The pane grew a header; the library moved only by the shell's section gap.
#[test]
fn the_first_library_row_sits_where_it_always_did() {
    let (v, _, _) = with_context(4);
    let a = Rect::new(0.0, 0.0, theme::SIDEBAR_W, 600.0);
    // Lane, gap, heading band, gap: the trailing NAV_PAD is the deliberate
    // section gap between the heading band and the nodes content — the same
    // gap every band of the stack has. The apps are in the rail beside the
    // pane, so no band of theirs sits in the stack any more.
    let stacked = theme::CAPTION_H + theme::NAV_PAD + HEADER_H + theme::NAV_PAD;
    assert_eq!(v.row_rects(a)[0].1.y, stacked);
}

/// **And an open popover does not move it either** — that is the whole reason it
/// stopped being a section that pushed the list down.
#[test]
fn opening_a_popover_does_not_move_the_library() {
    let (mut v, _, _) = with_context(6);
    let a = Rect::new(0.0, 0.0, theme::SIDEBAR_W, 600.0);
    let before: Vec<f32> = v.row_rects(a).into_iter().map(|(_, r)| r.y).collect();

    for kind in [PopoverKind::Summary, PopoverKind::Profiles] {
        v.toggle_popover(kind);
        let after: Vec<f32> = v.row_rects(a).into_iter().map(|(_, r)| r.y).collect();
        assert_eq!(before, after, "{kind:?} moved the library");
        v.close_popover();
    }
}

/// The switcher caps at `SWITCHER_MAX_H`, and only its list scrolls: your
/// card and the heading stay at the top, the toolbar at the floor.
#[test]
fn a_long_switcher_caps_at_its_max_and_scrolls_only_its_list() {
    let (mut v, profiles, _) = with_context(0);
    let workspaces: Vec<_> = (0..30).map(|i| ws(&format!("Workspace {i}"))).collect();
    v.set_context(
        profiles.clone(),
        profiles[1].id,
        workspaces.clone(),
        workspaces[0].id,
    );
    let a = area();
    v.toggle_popover(PopoverKind::Summary);
    assert_eq!(v.popover_panel(a, VIEW).expect("panel").h, SWITCHER_MAX_H);

    let find = |rects: &[(PopoverRow, Rect)], want: PopoverRow| {
        rects
            .iter()
            .find(|(row, _)| *row == want)
            .expect("listed")
            .1
    };
    let before = v.popover_rects(a, VIEW);
    assert!(v.scroll_popover(a, VIEW, -200.0));
    let after = v.popover_rects(a, VIEW);
    assert!(
        find(&after, PopoverRow::Workspace(0)).y < find(&before, PopoverRow::Workspace(0)).y,
        "the list scrolls"
    );
    assert_eq!(
        find(&after, PopoverRow::WorkspacesHeading),
        find(&before, PopoverRow::WorkspacesHeading),
        "the heading stays fixed"
    );
    let band = v.popover_list_band(a, VIEW).expect("a band");
    let panel = v.popover_panel(a, VIEW).unwrap();
    assert!(
        (band.y + band.h - (panel.y + panel.h - POPOVER_PAD)).abs() < 0.01,
        "with no footer the list runs to the panel's floor"
    );
}

/// A scrolled row must be cut at the header, never drawn over it.
#[test]
fn a_scrolled_row_never_rides_up_under_the_header() {
    let (mut v, _, _) = with_context(60);
    let a = Rect::new(0.0, 0.0, theme::SIDEBAR_W, 600.0);
    v.scroll_by(a, -400.0);
    let top = v.rows_top(a);
    for (i, r) in v.row_rects(a) {
        assert!(r.y + r.h > top, "row {i} at {} is above {top}", r.y);
    }
}

/// **Your card is a card, not a Profile row** (frame 6A): two lines tall,
/// so where you sync and how many devices hold the Profile fit under the
/// name; and the device count comes from the window.
#[test]
fn a_profile_row_is_two_lines_and_the_linked_count_is_set_from_outside() {
    let (mut v, _, _) = with_context(0);
    v.toggle_profile_popover();
    let a = area();
    let rects = v.popover_rects(a, VIEW);
    let h = |want: PopoverRow| rects.iter().find(|(row, _)| *row == want).map(|(_, r)| r.h);
    assert_eq!(h(PopoverRow::Profile(0)), Some(super::PROFILE_ROW_H));
    assert!(
        h(PopoverRow::Profile(0)) > h(PopoverRow::ProfilesTitle),
        "taller than a one-line row"
    );
    assert!(v.set_linked_count(2), "a new count is a change");
    assert!(!v.set_linked_count(2), "the same count is not");
}

/// The second line under a name: the open Profile says where it syncs and
/// how many other devices hold it; another says its email.
#[test]
fn the_profile_detail_says_where_and_how_many() {
    let (mut v, profiles, _) = with_context(0);
    let lang = koru_i18n::Lang::En;
    assert_eq!(v.profile_detail(&profiles[1], lang), "On this computer");
    assert!(v.set_linked_count(2));
    assert_eq!(
        v.profile_detail(&profiles[1], lang),
        "On this computer · 2 devices"
    );
    assert_eq!(v.profile_detail(&profiles[0], lang), "koru@example.com");
    v.set_account_state(&syncing(&profiles[1]));
    assert_eq!(
        v.profile_detail(&profiles[1], lang),
        "Up to date · 2 devices",
        "two facts at most: the count stands in for the cloud's name"
    );
    assert!(v.set_linked_count(0));
    assert_eq!(
        v.profile_detail(&profiles[1], lang),
        "Up to date · Koru Cloud"
    );
}

/// An account state in which `profile` syncs to Koru Cloud.
fn syncing(profile: &ProfileSummary) -> koru_agent_wire::AccountState {
    koru_agent_wire::AccountState {
        stage: koru_agent_wire::AccountStage::SignedIn,
        cloud_profiles: vec![koru_agent_wire::AccountProfileCloud {
            profile_id: profile.id.to_string(),
            stage: koru_agent_wire::CloudConnectionStage::Syncing,
        }],
        ..Default::default()
    }
}

/// The sync icon's five states, from the Agent's account state for the open
/// Profile (the CTO's artboard).
#[test]
fn the_sync_status_reads_the_account_state() {
    use koru_agent_wire::{AccountStage, AccountState, CloudConnectionStage};
    let (mut v, profiles, _) = with_context(0);
    assert_eq!(v.sync_status(), SyncStatus::NotSetUp, "nothing said yet");
    let with = |stage: CloudConnectionStage| AccountState {
        stage: AccountStage::SignedIn,
        cloud_profiles: vec![koru_agent_wire::AccountProfileCloud {
            profile_id: profiles[1].id.to_string(),
            stage,
        }],
        ..Default::default()
    };
    v.set_account_state(&with(CloudConnectionStage::Syncing));
    assert_eq!(v.sync_status(), SyncStatus::UpToDate);
    v.set_account_state(&with(CloudConnectionStage::Connecting));
    assert_eq!(v.sync_status(), SyncStatus::Syncing);
    v.set_account_state(&with(CloudConnectionStage::Retrying));
    assert_eq!(v.sync_status(), SyncStatus::Offline);
    v.set_account_state(&with(CloudConnectionStage::WaitingForConsent));
    assert_eq!(v.sync_status(), SyncStatus::NeedsAttention);
    // Another Profile's row says nothing about this one.
    v.set_account_state(&syncing(&profiles[0]));
    assert_eq!(v.sync_status(), SyncStatus::NotSetUp);
    v.set_account_state(&AccountState {
        stage: AccountStage::Unavailable,
        ..Default::default()
    });
    assert_eq!(v.sync_status(), SyncStatus::Offline, "no cloud to reach");
    v.set_account_state(&AccountState {
        stage: AccountStage::Failed,
        ..Default::default()
    });
    assert_eq!(v.sync_status(), SyncStatus::NeedsAttention);
    v.set_account_state(&AccountState {
        stage: AccountStage::NotConfigured,
        ..Default::default()
    });
    assert_eq!(v.sync_status(), SyncStatus::NotSetUp);
}

/// The floor chip opens the main page, and pressing it again shuts it — one
/// control, one popover, two pages behind it.
#[test]
fn the_floor_chip_opens_the_switcher_and_closes_it_again() {
    let (mut v, _, _) = with_context(0);
    let a = area();
    let chip = crate::panes::sidebar::floor::solve(a).chip;
    let (cx, cy) = chip.centre();
    assert_eq!(v.mouse_down(a, VIEW, cx, cy, false), Outcome::Consumed);
    // The chip's page: the heading and the workspaces, nothing else (the
    // CTO's artboard 11 and the notes after it).
    assert_eq!(
        v.popover.as_ref().map(|p| p.kind),
        Some(PopoverKind::Summary)
    );
    assert!(v.popover_open());
    let rows = v.popover_rows();
    assert_eq!(rows[0], PopoverRow::WorkspacesHeading);
    assert!(rows.contains(&PopoverRow::Workspace(0)));
    assert!(matches!(rows.last(), Some(PopoverRow::Workspace(_))));
    // Pressing the chip while it is open shuts it.
    assert_eq!(v.mouse_down(a, VIEW, cx, cy, false), Outcome::Consumed);
    assert!(!v.popover_open());
    v.advance_popover(Instant::now() + Duration::from_secs(1));

    // The rail's profile square: the Profiles page, no views inside it.
    let rail = v.rail_of(a);
    let (sx, sy) = v.apps_rail_rects(rail).profile.centre();
    assert_eq!(v.apps_rail_mouse_down(rail, sx, sy), Outcome::Consumed);
    assert!(v.popover_open());
    assert_eq!(
        v.popover.as_ref().map(|p| p.kind),
        Some(PopoverKind::Profiles)
    );
    let rows = v.popover_rows();
    assert_eq!(rows[0], PopoverRow::ProfilesTitle);
    assert!(rows[0].selectable(), "the title carries the `+`");
    assert!(rows.contains(&PopoverRow::Profile(0)));
    assert!(matches!(rows.last(), Some(PopoverRow::Profile(_))));
    // The square shuts it.
    assert_eq!(v.apps_rail_mouse_down(rail, sx, sy), Outcome::Consumed);
    assert!(!v.popover_open());
}

/// The chords open their pages with the keyboard somewhere useful:
/// `Ctrl+Alt+W` the main page on the workspace you are in, `Ctrl+Alt+P`
/// the Profiles page on the Profile you are in.
#[test]
fn the_chords_open_their_pages_on_where_you_are() {
    let (mut v, _, _) = with_context(0);
    assert!(v.open_popover(PopoverKind::Summary));
    assert_eq!(
        v.popover_hover().map(|i| v.popover_rows()[i]),
        Some(PopoverRow::Workspace(0)),
        "on the open workspace"
    );
    assert!(v.open_popover(PopoverKind::Profiles));
    assert_eq!(
        v.popover.as_ref().map(|p| p.kind),
        Some(PopoverKind::Profiles)
    );
    assert_eq!(
        v.popover_hover().map(|i| v.popover_rows()[i]),
        Some(PopoverRow::Profile(1)),
        "on the open Profile"
    );
    assert!(!v.open_popover(PopoverKind::Profiles), "already showing");
}

/// Both pages hang from the same corner — the floor chip's — so a slide
/// between them never jumps the panel sideways, and the panel sits **above**
/// the chip, inside the window.
#[test]
fn both_pages_anchor_above_the_floor_chip() {
    let (mut v, _, _) = with_context(0);
    let a = area();
    let chip = crate::panes::sidebar::floor::solve(a).chip;
    // The chip's page rises from the chip.
    v.toggle_popover(PopoverKind::Summary);
    let panel = v.popover_panel(a, VIEW).unwrap();
    assert_eq!(panel.x, chip.x);
    assert!(panel.y + panel.h <= chip.y, "sits above the chip");
    assert!(panel.y >= 0.0);
    // The Profiles page, and the square's page, hang beside the rail with
    // their foot level with the profile square's (the CTO's artboard 11).
    let rail = v.rail_of(a);
    let square = v.apps_rail_rects(rail).profile;
    for open in [
        (|v: &mut ExploreView| v.toggle_popover(PopoverKind::Profiles)) as fn(&mut ExploreView),
        |v: &mut ExploreView| v.toggle_profile_popover(),
    ] {
        open(&mut v);
        let panel = v.popover_panel(a, VIEW).unwrap();
        assert_eq!(panel.x, rail.x + rail.w + POPOVER_GAP, "beside the rail");
        assert!(
            (panel.y + panel.h - (square.y + square.h)).abs() < 0.5,
            "its foot level with the square's: {panel:?} vs {square:?}"
        );
        assert!(panel.y >= 0.0);
    }
}

#[test]
fn every_popover_row_hit_tests_back_to_itself() {
    for kind in [PopoverKind::Summary, PopoverKind::Profiles] {
        let (mut v, _, _) = with_context(4);
        let a = area();
        v.toggle_popover(kind);
        let rects = v.popover_rects(a, VIEW);
        assert!(!rects.is_empty(), "{kind:?}");
        for (i, (row, r)) in rects.iter().enumerate() {
            let (x, y) = r.centre();
            assert_eq!(v.popover_at(a, VIEW, x, y), Some((i, *row)), "{row:?}");
        }
    }
}

/// **The parts of a row hit-test to themselves too**: the gear at a
/// workspace's or Profile's trailing end, the heading's ⋯, each of the
/// toolbar's three controls — and the row elsewhere.
#[test]
fn every_popover_part_hit_tests_back_to_itself() {
    let (mut v, _, _) = with_context(4);
    let a = area();
    v.toggle_popover(PopoverKind::Summary);
    let find = |v: &ExploreView, want: PopoverRow| {
        v.popover_rects(a, VIEW)
            .into_iter()
            .find(|(row, _)| *row == want)
            .expect("listed")
            .1
    };
    let r = find(&v, PopoverRow::Workspace(0));
    let (x, y) = super::trailing_lane(r).centre();
    assert_eq!(
        v.popover_part_at(a, VIEW, x, y)
            .map(|(_, row, part)| (row, part)),
        Some((PopoverRow::Workspace(0), PopoverPart::Gear))
    );
    let (x, y) = v
        .workspace_row_parts(PopoverRow::Workspace(0), r)
        .name
        .centre();
    assert_eq!(
        v.popover_part_at(a, VIEW, x, y)
            .map(|(_, row, part)| (row, part)),
        Some((PopoverRow::Workspace(0), PopoverPart::Row))
    );
    let r = find(&v, PopoverRow::WorkspacesHeading);
    let buttons = super::head_button_rects(r, PopoverRow::WorkspacesHeading);
    assert_eq!(
        buttons.iter().map(|(b, _)| *b).collect::<Vec<_>>(),
        vec![HeadButton::New, HeadButton::Import],
        "a `+`, then import, at the trailing end"
    );
    assert!(buttons[0].1.x + buttons[0].1.w <= buttons[1].1.x);
    assert!(
        (buttons[1].1.x + buttons[1].1.w - (super::trailing_lane(r).x + super::trailing_lane(r).w))
            .abs()
            < 0.5,
        "the last button ends where every row's control does"
    );
    for (button, rect) in &buttons {
        let (x, y) = rect.centre();
        assert_eq!(
            v.popover_part_at(a, VIEW, x, y)
                .map(|(_, row, part)| (row, part)),
            Some((PopoverRow::WorkspacesHeading, PopoverPart::Button(*button)))
        );
    }
    let (x, y) = (r.x + 30.0, r.centre().1);
    assert_eq!(
        v.popover_part_at(a, VIEW, x, y)
            .map(|(_, row, part)| (row, part)),
        Some((PopoverRow::WorkspacesHeading, PopoverPart::Row)),
        "the words are the row"
    );

    v.toggle_popover(PopoverKind::Profiles);
    let r = find(&v, PopoverRow::Profile(0));
    let (x, y) = super::trailing_lane(r).centre();
    assert_eq!(
        v.popover_part_at(a, VIEW, x, y)
            .map(|(_, row, part)| (row, part)),
        Some((PopoverRow::Profile(0), PopoverPart::Gear))
    );
}

/// The pieces of a workspace row are laid out once, by `workspace_row_parts`,
/// and they must not overlap: the chip and the name are what you read, and the
/// trailing lane — the tick, or the gear — sits at the right end, clear of
/// the name.
#[test]
fn the_pieces_of_a_workspace_row_do_not_overlap() {
    let (mut v, _, _) = with_context(4);
    let a = area();
    v.toggle_popover(PopoverKind::Summary);
    let rows: Vec<_> = v
        .popover_rects(a, VIEW)
        .into_iter()
        .filter(|(row, _)| matches!(row, PopoverRow::Workspace(_)))
        .collect();
    assert!(!rows.is_empty());

    for (row, r) in rows {
        let p = v.workspace_row_parts(row, r);
        assert!(p.chip.x >= r.x, "the chip starts inside the row");
        assert!(
            p.chip.x + p.chip.w <= p.name.x,
            "the chip runs into the name"
        );
        assert!(
            p.name.x + p.name.w <= p.trailing.x,
            "the name runs under the trailing lane"
        );
        assert!(
            p.trailing.x + p.trailing.w <= r.x + r.w,
            "the trailing lane runs off the row"
        );
        assert!(p.name.w > 0.0, "there is room left for a name");
    }
}

#[test]
fn profile_and_workspace_rows_share_the_same_padding_lanes() {
    let (v, _, _) = with_context(1);
    let row = Rect::new(20.0, 40.0, 300.0, theme::ROW_H);
    let workspace = v.workspace_row_parts(PopoverRow::Workspace(0), row);
    let profile = koru_ui::profile_picker::row_parts(row);

    assert_eq!(profile.chip, workspace.chip);
    assert_eq!(profile.name.x, workspace.name.x);
    assert_eq!(profile.name.y, workspace.name.y);
    assert_eq!(profile.name.h, workspace.name.h);
    assert!(
        workspace.name.x + workspace.name.w <= workspace.trailing.x,
        "the trailing lane owns the end of the row"
    );
    // The gear sits where a Profile row's trailing control does.
    assert_eq!(profile.trailing, workspace.trailing);
    assert_eq!(koru_ui::profile_picker::ROW_H, theme::ROW_H);
}

/// **The gear is a control** (the CTO's artboard): a press on a workspace
/// row's trailing lane opens the Manage tab on it rather than switching, and
/// the page shuts behind it. A Profile row's gear does the same for the
/// Profile.
#[test]
fn a_rows_gear_opens_manage_on_it() {
    let (mut v, profiles, workspaces) = with_context(1);
    let a = area();
    v.toggle_popover(PopoverKind::Summary);
    let (row, r) = v
        .popover_rects(a, VIEW)
        .into_iter()
        .find(|(row, _)| matches!(row, PopoverRow::Workspace(1)))
        .expect("the workspace row");
    let (x, y) = v.workspace_row_parts(row, r).trailing.centre();
    assert_eq!(
        v.mouse_down(a, VIEW, x, y, false),
        Outcome::ManageWorkspace(workspaces[1].id)
    );
    assert!(!v.popover_open());
    v.advance_popover(Instant::now() + Duration::from_secs(1));

    v.toggle_popover(PopoverKind::Profiles);
    let (_, r) = v
        .popover_rects(a, VIEW)
        .into_iter()
        .find(|(row, _)| matches!(row, PopoverRow::Profile(2)))
        .expect("the profile row");
    let (x, y) = super::trailing_lane(r).centre();
    assert_eq!(
        v.mouse_down(a, VIEW, x, y, false),
        Outcome::ManageProfile(profiles[2].id)
    );
    assert!(!v.popover_open());
}

#[test]
fn opening_a_workspace_context_menu_highlights_its_row() {
    let (mut v, _, workspaces) = with_context(1);
    v.toggle_popover(PopoverKind::Summary);
    assert!(v.highlight_popover_workspace(workspaces[1].id));
    let highlighted = v
        .popover_rows()
        .iter()
        .position(|row| *row == PopoverRow::Workspace(1));
    assert_eq!(v.popover_hover(), highlighted);
}

/// **Every workspace row reserves the same space**, whether or not it is the
/// current one, pinned, or under the pointer — otherwise the names would not
/// line up down the list.
#[test]
fn a_workspace_rows_name_column_does_not_move_between_rows() {
    let (mut v, profiles, workspaces) = with_context(4);
    let a = area();
    // The *second* workspace is the current one, so the ticked row is not the
    // first — a bug that only misplaced the first row would pass otherwise.
    v.set_context(
        profiles.clone(),
        profiles[1].id,
        workspaces.clone(),
        workspaces[1].id,
    );
    v.toggle_popover(PopoverKind::Summary);

    let names: Vec<_> = v
        .popover_rects(a, VIEW)
        .into_iter()
        .filter(|(row, _)| matches!(row, PopoverRow::Workspace(_)))
        .map(|(row, r)| {
            let p = v.workspace_row_parts(row, r);
            (p.name.x, p.name.w)
        })
        .collect();

    assert!(names.len() > 1, "more than one workspace to compare");
    for pair in names.windows(2) {
        assert_eq!(
            pair[0], pair[1],
            "the ticked row's name column differs from the others'"
        );
    }
}

/// A row that is not a workspace has no chip and no trailing lane. Zero-sized
/// rather than optional — so a caller may ask about any row without first
/// checking what kind it is.
#[test]
fn a_row_that_is_not_a_workspace_has_no_parts() {
    let (mut v, _, _) = with_context(4);
    let a = area();
    v.toggle_popover(PopoverKind::Summary);
    for (row, r) in v.popover_rects(a, VIEW) {
        if matches!(row, PopoverRow::Workspace(_)) {
            continue;
        }
        let p = v.workspace_row_parts(row, r);
        for part in [p.chip, p.name, p.trailing] {
            assert_eq!((part.w, part.h), (0.0, 0.0), "{row:?}");
        }
    }
}

#[test]
fn the_popover_rows_run_top_to_bottom_inside_the_panel() {
    for kind in [PopoverKind::Summary, PopoverKind::Profiles] {
        let (mut v, _, _) = with_context(0);
        let a = area();
        v.toggle_popover(kind);
        let panel = v.popover_panel(a, VIEW).expect("open");
        let rects = v.popover_rects(a, VIEW);
        for pair in rects.windows(2) {
            assert!(
                (pair[0].1.y + pair[0].1.h - pair[1].1.y).abs() < 0.01,
                "{kind:?}: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
        let last = rects.last().unwrap().1;
        assert!(
            last.y + last.h <= panel.y + panel.h + 0.01,
            "{kind:?} overflows its own panel"
        );
    }
}

/// A panel that ran off the bottom of a short window would put the toolbar
/// somewhere unreachable.
#[test]
fn a_popover_never_leaves_the_window() {
    for view in [
        (1200.0, 700.0),
        (400.0, 240.0),
        (320.0, 120.0),
        (200.0, 90.0),
    ] {
        for kind in [PopoverKind::Summary, PopoverKind::Profiles] {
            let (mut v, _, _) = with_context(0);
            v.toggle_popover(kind);
            let panel = v.popover_panel(area(), view).expect("open");
            assert!(
                panel.x >= 0.0 && panel.y >= 0.0,
                "{view:?} {kind:?} {panel:?}"
            );
            assert!(
                panel.x + panel.w <= view.0 + 0.01 || panel.w >= view.0,
                "{view:?} {kind:?} runs off the right: {panel:?}"
            );
        }
    }
}

// ----------------------------------------------------------------- the bug

/// **The bug.** The pane was handed one profile — the open one — so the list had
/// a single row and it named where you already were.
#[test]
fn every_known_profile_gets_a_row() {
    let (mut v, profiles, _) = with_context(0);
    v.toggle_popover(PopoverKind::Profiles);

    let listed: Vec<PopoverRow> = v
        .popover_rows()
        .into_iter()
        .filter(|r| matches!(r, PopoverRow::Profile(_)))
        .collect();
    assert_eq!(listed.len(), profiles.len(), "one row per profile");
    assert_eq!(listed.len(), 3, "the three from the report");
}

/// **The other half of the bug**, and the one a weaker test would have got
/// wrong: every row used to resolve to `self.profile.id`. Choosing a Profile
/// switches to **that** Profile (the CTO's artboard) and slides back to the
/// main page, with the popover still open; choosing the one you are in only
/// slides back.
#[test]
fn choosing_a_profile_switches_to_it_and_leaves_the_list_up() {
    let (mut v, profiles, _) = with_context(0);
    let a = area();
    let current = profiles[1].id;

    for (i, want) in profiles.iter().enumerate() {
        v.toggle_popover(PopoverKind::Profiles);
        let (_, r) = *v
            .popover_rects(a, VIEW)
            .iter()
            .find(|(row, _)| *row == PopoverRow::Profile(i))
            .expect("the row is listed");
        let (x, y) = r.centre();
        let expected = if want.id == current {
            Outcome::Consumed
        } else {
            Outcome::SwitchProfile(want.id)
        };
        assert_eq!(v.mouse_down(a, VIEW, x, y, false), expected, "row {i}");
        assert!(v.popover_open(), "row {i}: the list stays up");
        assert_eq!(
            v.popover.as_ref().map(|p| p.kind),
            Some(PopoverKind::Profiles),
            "row {i}: still the Profiles page — no views inside it"
        );
        v.close_popover();
        v.advance_popover(Instant::now() + Duration::from_secs(1));
    }
    // And the one you are in is not special-cased into something else.
    assert_eq!(v.current_profile, Some(current));
}

/// Back returns to the main page, with the keyboard on your card — so Enter
/// goes straight back in.
#[test]
fn the_profiles_page_has_no_page_behind_it() {
    use winit::keyboard::{Key, NamedKey};
    let (mut v, _, _) = with_context(0);
    let a = area();
    v.toggle_profile_popover();
    let (_, r) = *v
        .popover_rects(a, VIEW)
        .iter()
        .find(|(row, _)| *row == PopoverRow::ProfilesTitle)
        .expect("the title");
    let (x, y) = r.centre();
    assert_eq!(v.mouse_down(a, VIEW, x, y, false), Outcome::Consumed);
    assert!(v.popover_open(), "the title is furniture, not a way out");
    assert_eq!(
        v.popover.as_ref().map(|p| p.kind),
        Some(PopoverKind::Profiles)
    );
    // Left has nowhere to go and falls through to the container.
    assert_eq!(v.popover_key(&Key::Named(NamedKey::ArrowLeft)), None);
    assert!(v.popover_open());
}

/// The title is a stop now that it carries *New profile*'s `+` (the CTO's
/// note): Down from nothing lands on it, Enter there is the `+`, and Up from
/// the first Profile wraps to the last Profile — there is no footer any more.
#[test]
fn the_arrows_land_on_the_profiles_title_and_enter_is_its_plus() {
    use winit::keyboard::{Key, NamedKey};
    let (mut v, _, _) = with_context(0);
    v.toggle_profile_popover();
    v.popover_key(&Key::Named(NamedKey::ArrowDown));
    assert_eq!(
        v.popover_hover().map(|i| v.popover_rows()[i]),
        Some(PopoverRow::ProfilesTitle)
    );
    v.popover_key(&Key::Named(NamedKey::ArrowDown));
    assert_eq!(
        v.popover_hover().map(|i| v.popover_rows()[i]),
        Some(PopoverRow::Profile(0))
    );
    v.popover_key(&Key::Named(NamedKey::ArrowUp));
    v.popover_key(&Key::Named(NamedKey::ArrowUp));
    assert_eq!(
        v.popover_hover().map(|i| v.popover_rows()[i]),
        Some(PopoverRow::Profile(2)),
        "wraps to the last Profile"
    );
    v.popover_key(&Key::Named(NamedKey::ArrowDown));
    assert_eq!(
        v.popover_key(&Key::Named(NamedKey::Enter)),
        Some(Outcome::NewProfile),
        "Enter on the title presses its `+`"
    );
    assert!(!v.popover_open(), "the creator is the end of it");
}

#[test]
fn ctrl_alt_p_lands_the_keyboard_on_the_profile_you_are_in() {
    let (mut v, _, _) = with_context(0);
    assert!(v.open_popover(PopoverKind::Profiles));
    assert_eq!(
        v.popover_hover().map(|i| v.popover_rows()[i]),
        Some(PopoverRow::Profile(1)),
        "on the open Profile"
    );
}

/// Enter on a Profile row switches and slides back, the popover still up.
#[test]
fn enter_on_a_profile_row_switches_and_stays_open() {
    use winit::keyboard::{Key, NamedKey};
    let (mut v, profiles, _) = with_context(0);
    v.toggle_popover(PopoverKind::Profiles);
    // The title is a stop (its `+`): four Downs reach the third Profile.
    for _ in 0..4 {
        v.popover_key(&Key::Named(NamedKey::ArrowDown));
    }
    assert_eq!(
        v.popover_hover().map(|i| v.popover_rows()[i]),
        Some(PopoverRow::Profile(2))
    );
    assert_eq!(
        v.popover_key(&Key::Named(NamedKey::Enter)),
        Some(Outcome::SwitchProfile(profiles[2].id))
    );
    assert!(v.popover_open(), "the list stays up");
    assert_eq!(
        v.popover.as_ref().map(|p| p.kind),
        Some(PopoverKind::Profiles)
    );
}

/// Left on the main page is not the switcher's; Right on a row that is not
/// your card is not either.
#[test]
fn left_and_right_fall_through_where_they_mean_nothing() {
    use winit::keyboard::{Key, NamedKey};
    let (mut v, _, _) = with_context(0);
    v.toggle_popover(PopoverKind::Summary);
    assert_eq!(v.popover_key(&Key::Named(NamedKey::ArrowLeft)), None);
    for _ in 0..2 {
        v.popover_key(&Key::Named(NamedKey::ArrowDown));
    }
    assert_eq!(
        v.popover_hover().map(|i| v.popover_rows()[i]),
        Some(PopoverRow::Workspace(0))
    );
    assert_eq!(v.popover_key(&Key::Named(NamedKey::ArrowRight)), None);
    assert!(v.popover_open());
}

/// **The toolbar is three controls in one row**: ← and → move between
/// them, wrapping; Enter presses the one the keyboard is on. Sync and
/// settings are destinations that shut the page; help opens beside it and
/// leaves it up.
#[test]
fn left_and_right_fall_through_on_the_profiles_page() {
    use winit::keyboard::{Key, NamedKey};
    let (mut v, _, _) = with_context(0);
    v.toggle_profile_popover();
    v.popover_key(&Key::Named(NamedKey::ArrowDown));
    assert_eq!(v.popover_key(&Key::Named(NamedKey::ArrowLeft)), None);
    assert_eq!(v.popover_key(&Key::Named(NamedKey::ArrowRight)), None);
    assert!(v.popover_open(), "and neither dismissed it");
}

/// The heading's import button asks the container for the formats and keeps
/// the page; its `+` asks for a workspace and shuts it, and Enter on the
/// heading is the `+`. A press on the heading's words does nothing.
#[test]
fn the_headings_buttons_report_themselves() {
    use winit::keyboard::{Key, NamedKey};
    let (mut v, _, _) = with_context(0);
    let a = area();
    v.toggle_popover(PopoverKind::Summary);
    let import = v
        .popover_head_button_rect(a, VIEW, HeadButton::Import)
        .expect("the import button");
    let (x, y) = import.centre();
    assert_eq!(
        v.mouse_down(a, VIEW, x, y, false),
        Outcome::WorkspacesImport
    );
    assert!(v.popover_open(), "the page stays under the formats");
    let (_, r) = *v
        .popover_rects(a, VIEW)
        .iter()
        .find(|(row, _)| *row == PopoverRow::WorkspacesHeading)
        .expect("the heading");
    assert_eq!(
        v.mouse_down(a, VIEW, r.x + 30.0, r.centre().1, false),
        Outcome::Consumed,
        "the words do nothing"
    );
    assert!(v.popover_open());
    v.popover_key(&Key::Named(NamedKey::ArrowDown));
    assert_eq!(
        v.popover_hover().map(|i| v.popover_rows()[i]),
        Some(PopoverRow::WorkspacesHeading)
    );
    assert_eq!(
        v.popover_key(&Key::Named(NamedKey::Enter)),
        Some(Outcome::NewWorkspace),
        "Enter on the heading is its `+`"
    );
    assert!(!v.popover_open(), "the creator is the end of it");
    v.advance_popover(Instant::now() + Duration::from_secs(1));
    v.toggle_popover(PopoverKind::Summary);
    let plus = v
        .popover_head_button_rect(a, VIEW, HeadButton::New)
        .expect("the `+`");
    let (x, y) = plus.centre();
    assert_eq!(v.mouse_down(a, VIEW, x, y, false), Outcome::NewWorkspace);
    assert!(!v.popover_open());
}

/// After Back, the chip shuts the main page — it must not need a second
/// press because the Profiles page was visited.
#[test]
fn the_square_shuts_the_profiles_page() {
    let (mut v, _, _) = with_context(0);
    let a = area();
    v.toggle_popover(PopoverKind::Profiles);
    // The page hangs off the rail's square, so the square is what shuts it
    // — the panel hangs over the floor chip from there.
    let rail = v.rail_of(a);
    let (x, y) = v.apps_rail_rects(rail).profile.centre();
    assert_eq!(v.apps_rail_mouse_down(rail, x, y), Outcome::Consumed);
    assert!(!v.popover_open());
}

/// The chip while the Profiles page is showing shuts the switcher; reopening
/// it shows the main page.
#[test]
fn reopening_from_the_chip_shows_the_main_page() {
    let (mut v, _, _) = with_context(0);
    let a = area();
    v.toggle_popover(PopoverKind::Profiles);
    let rail = v.rail_of(a);
    let (sx, sy) = v.apps_rail_rects(rail).profile.centre();
    assert_eq!(v.apps_rail_mouse_down(rail, sx, sy), Outcome::Consumed);
    assert!(!v.popover_open());
    v.advance_popover(Instant::now() + Duration::from_secs(1));
    let (x, y) = crate::panes::sidebar::floor::solve(a).chip.centre();
    assert_eq!(v.mouse_down(a, VIEW, x, y, false), Outcome::Consumed);
    assert!(v.popover_open());
    assert_eq!(
        v.popover.as_ref().map(|p| p.kind),
        Some(PopoverKind::Summary)
    );
}

/// The slide between pages asks for frames while it runs, and stops.
#[test]
fn a_slide_animates_and_then_settles() {
    let (mut v, _, _) = with_context(0);
    v.toggle_popover(PopoverKind::Summary);
    v.advance_popover(Instant::now() + Duration::from_secs(1));
    assert!(!v.popover_animating(), "opened and settled");

    v.toggle_popover(PopoverKind::Profiles);
    assert!(v.popover_animating(), "mid-slide");
    let half = Instant::now() + Duration::from_millis((theme::POPOVER_FADE_MS * 0.5) as u64);
    v.advance_popover(half);
    let page = v.popover.as_ref().unwrap().page;
    assert!(page > 0.0 && page < 1.0, "between the pages: {page}");
    assert!(v.popover_animating());

    let past = Instant::now() + Duration::from_millis(theme::POPOVER_FADE_MS as u64 * 4);
    v.advance_popover(past);
    assert!(!v.popover_animating(), "the slide settles");
    assert_eq!(v.popover.as_ref().unwrap().page, 1.0);
}

#[test]
fn the_current_profile_and_workspace_are_the_active_ones() {
    let (v, profiles, list) = with_context(0);
    assert_eq!(v.current_profile, Some(profiles[1].id));
    assert_eq!(v.current, Some(list[0].id));
    assert_eq!(v.profile().map(|p| p.id), Some(profiles[1].id));
}

// ------------------------------------------------------------- the rest

#[test]
fn choosing_a_workspace_reports_it_and_closes_the_popover() {
    let (mut v, _, list) = with_context(0);
    let a = area();
    v.toggle_popover(PopoverKind::Summary);
    let (_, r) = *v
        .popover_rects(a, VIEW)
        .iter()
        .find(|(row, _)| *row == PopoverRow::Workspace(1))
        .unwrap();
    let (x, y) = r.centre();
    assert_eq!(
        v.mouse_down(a, VIEW, x, y, false),
        Outcome::SwitchWorkspace(list[1].id)
    );
    assert!(!v.popover_open(), "and it shut behind itself");
}

/// The Profiles title's `+` is *New profile* (the CTO's note): it reports
/// itself and shuts the page, since the creator is the end of it.
#[test]
fn the_titles_plus_reports_new_profile() {
    let a = area();
    let (mut v, _, _) = with_context(0);
    v.toggle_popover(PopoverKind::Profiles);
    assert!(
        v.popover_head_button_rect(a, VIEW, HeadButton::Import)
            .is_none(),
        "the Profiles title has no import"
    );
    let plus = v
        .popover_head_button_rect(a, VIEW, HeadButton::New)
        .expect("the `+`");
    let (x, y) = plus.centre();
    assert_eq!(v.mouse_down(a, VIEW, x, y, false), Outcome::NewProfile);
    assert!(!v.popover_open());
}

/// Each page's rows, in the order the artboard lists them.
#[test]
fn each_page_lists_its_head_list_and_footer_in_order() {
    let (mut v, _, _) = with_context(0);
    v.toggle_popover(PopoverKind::Profiles);
    assert_eq!(
        v.popover_rows(),
        vec![
            PopoverRow::ProfilesTitle,
            PopoverRow::Profile(0),
            PopoverRow::Profile(1),
            PopoverRow::Profile(2),
        ]
    );
    // The chip's page: the heading and the workspaces, nothing else.
    v.toggle_popover(PopoverKind::Summary);
    assert_eq!(
        v.popover_rows(),
        vec![
            PopoverRow::WorkspacesHeading,
            PopoverRow::Workspace(0),
            PopoverRow::Workspace(1),
        ]
    );
}

/// The head is drawn over the list's overflow, so its `+` — not a scrolled
/// row hidden under it — answers for a press there.
#[test]
fn the_head_outranks_a_scrolled_row_behind_it() {
    let a = area();
    let (mut v, _, list) = with_context(0);
    let profiles: Vec<_> = (0..30).map(|i| a_profile(&format!("p{i}"))).collect();
    v.set_context(profiles.clone(), profiles[1].id, list.clone(), list[0].id);
    v.toggle_popover(PopoverKind::Profiles);
    let plus = v
        .popover_head_button_rect(a, VIEW, HeadButton::New)
        .expect("the `+`");
    let (x, y) = plus.centre();
    assert_eq!(
        v.mouse_down(a, VIEW, x, y, false),
        Outcome::NewProfile,
        "the head answers, not the row under it"
    );
}

/// Without this the second click of a double-click re-toggles, and the chip
/// looks like it is ignoring every other press.
#[test]
fn double_clicking_the_floor_chip_does_nothing_extra() {
    let (mut v, _, _) = with_context(0);
    let a = area();
    let (x, y) = crate::panes::sidebar::floor::solve(a).chip.centre();
    v.toggle_popover(PopoverKind::Summary);
    assert_eq!(v.mouse_double_click(a, VIEW, x, y), Outcome::Consumed);
    assert!(v.popover_open(), "and it was left alone");
}

#[test]
fn a_press_elsewhere_in_the_pane_closes_the_popover_without_acting() {
    let (mut v, _, _) = with_context(6);
    let a = area();
    v.toggle_popover(PopoverKind::Summary);
    let below = v.rows_top(a) + 200.0;
    assert_eq!(
        v.mouse_down(a, VIEW, a.x + 4.0, below, false),
        Outcome::Consumed
    );
    assert!(!v.popover_open());
}

#[test]
fn escape_closes_the_popover() {
    use winit::keyboard::{Key, NamedKey};
    let (mut v, _, _) = with_context(0);
    v.toggle_popover(PopoverKind::Profiles);
    assert_eq!(
        v.popover_key(&Key::Named(NamedKey::Escape)),
        Some(Outcome::Consumed)
    );
    assert!(!v.popover_open());
}

#[test]
fn the_arrows_skip_the_furniture_and_wrap() {
    use winit::keyboard::{Key, NamedKey};
    for kind in [PopoverKind::Summary, PopoverKind::Profiles] {
        let (mut v, _, _) = with_context(0);
        v.toggle_popover(kind);
        let rows = v.popover_rows();

        let mut seen = Vec::new();
        for _ in 0..rows.len() + 2 {
            v.popover_key(&Key::Named(NamedKey::ArrowDown));
            let i = v.popover.as_ref().unwrap().hover.unwrap();
            assert!(rows[i].selectable(), "landed on {:?}", rows[i]);
            seen.push(i);
        }
        assert!(seen.windows(2).any(|w| w[1] < w[0]), "{kind:?}: {seen:?}");
    }
}

#[test]
fn arrow_navigation_reveals_rows_in_a_long_switcher() {
    use winit::keyboard::{Key, NamedKey};
    let (mut v, profiles, _) = with_context(0);
    let workspaces: Vec<_> = (0..30).map(|i| ws(&format!("Workspace {i}"))).collect();
    v.set_context(
        profiles.clone(),
        profiles[1].id,
        workspaces.clone(),
        workspaces[0].id,
    );
    let a = area();
    v.toggle_popover(PopoverKind::Summary);
    for _ in 0..20 {
        v.popover_key(&Key::Named(NamedKey::ArrowDown));
        v.reveal_popover_hover(a, VIEW);
    }
    let index = v.popover.as_ref().unwrap().hover.unwrap();
    let (row, rect) = v.popover_rects(a, VIEW)[index];
    let band = v.popover_list_band(a, VIEW).unwrap();
    assert!(matches!(row, PopoverRow::Workspace(_)));
    assert!(rect.y >= band.y && rect.y + rect.h <= band.y + band.h);
    assert!(v.popover.as_ref().unwrap().scroll > 0.0);
}

#[test]
fn enter_runs_the_highlighted_row() {
    use winit::keyboard::{Key, NamedKey};
    let (mut v, _, list) = with_context(0);
    v.toggle_popover(PopoverKind::Summary);
    // Down once from nothing lands on the heading; the second Down is the
    // first workspace.
    for _ in 0..2 {
        v.popover_key(&Key::Named(NamedKey::ArrowDown));
    }
    assert_eq!(
        v.popover_key(&Key::Named(NamedKey::Enter)),
        Some(Outcome::SwitchWorkspace(list[0].id))
    );
    assert!(!v.popover_open());
}

/// A panel hanging off a column is not a modal over the document, so a chord it
/// does not claim has to reach the container and still shut the pane.
#[test]
fn an_unclaimed_key_falls_through_to_the_container() {
    use winit::keyboard::Key;
    let (mut v, _, _) = with_context(0);
    v.toggle_popover(PopoverKind::Summary);
    assert_eq!(v.popover_key(&Key::Character("e".into())), None);
    assert!(v.popover_open(), "and it was not dismissed by the miss");
}

/// A profile with no picture still has to draw something, and `?` beats an empty
/// circle that reads as broken.
#[test]
fn initials_come_from_the_first_two_words_and_never_panic() {
    assert_eq!(koru_ui::profile_picker::initials("Kim Alvarez"), "KA");
    assert_eq!(koru_ui::profile_picker::initials("kim"), "K");
    assert_eq!(
        koru_ui::profile_picker::initials("Kim  van  der  Berg"),
        "KV"
    );
    assert_eq!(koru_ui::profile_picker::initials(""), "?");
    assert_eq!(koru_ui::profile_picker::initials("   "), "?");
    assert_eq!(koru_ui::profile_picker::initials("!!! ???"), "?");
}

/// The chip follows the **email**, so renaming yourself does not change colour
/// — and it comes from the four existing pill palettes, so the pane adds no
/// `Palette` fields of its own.
#[test]
fn a_chip_colour_follows_the_email_and_not_the_name() {
    assert_eq!(
        koru_ui::profile_picker::chip_slot("kim@personal.co"),
        koru_ui::profile_picker::chip_slot("  KIM@Personal.CO ")
    );
    assert_eq!(
        koru_ui::profile_picker::chip_slot("sam@acme.com"),
        koru_ui::profile_picker::chip_slot("sam@acme.com")
    );
    assert!(
        koru_ui::profile_picker::chip_slot("kim@personal.co") < 4,
        "one of the four"
    );
    assert!(koru_ui::profile_picker::chip_slot("") < 4);
}

// ---------------------------------------------------- folder-profile rows

/// The Profiles page carries no legacy folder actions and no remove box:
/// the title's `+` is its only command, whatever kind of profiles are
/// listed, and the Profiles end the page.
#[test]
fn the_profiles_page_offers_no_folder_actions() {
    let (mut v, _, _) = with_context(4);
    v.toggle_popover(PopoverKind::Profiles);
    let rows = v.popover_rows();
    assert!(matches!(rows.last(), Some(PopoverRow::Profile(_))));
    assert_eq!(PopoverRow::ProfilesTitle.head_buttons(), &[HeadButton::New]);
}

// ------------------------------------------------------------------ the fade

/// **The hazard the whole `closing` flag exists for.** A dismissed popover is
/// still on screen for the length of its fade; if it also still took clicks, a
/// press meant for what is behind it would choose a menu row instead.
#[test]
fn a_dismissed_popover_goes_deaf_before_it_goes_away() {
    let (mut v, _, _) = with_context(4);
    let a = area();
    let vp = (900.0, 700.0);
    v.toggle_popover(PopoverKind::Summary);
    let (_, r) = v
        .popover_rects(a, vp)
        .into_iter()
        .find(|(row, _)| *row == PopoverRow::WorkspacesHeading)
        .expect("the heading");
    let (x, y) = super::head_button_rects(r, PopoverRow::WorkspacesHeading)[0]
        .1
        .centre();
    assert!(v.close_popover());

    assert!(!v.popover_open(), "deaf immediately");
    assert!(v.popover_visible(), "but still drawn");
    assert!(
        v.popover_at(a, vp, x, y).is_none(),
        "the row is not hit-testable"
    );
    assert_eq!(v.mouse_double_click(a, vp, x, y), Outcome::Consumed);
    // ...and the key map has let go of it too, so Escape reaches the container.
    assert!(v.popover_key(&Key::Named(NamedKey::Escape)).is_none());
}

/// ADR-26: `animating` is the one exception to "a frame only when something
/// changed", and it has to end on its own or the window redraws forever.
#[test]
fn a_fade_finishes_and_stops_asking_for_frames() {
    let (mut v, _, _) = with_context(4);
    let past = Instant::now() + Duration::from_millis(theme::POPOVER_FADE_MS as u64 * 4);

    v.toggle_popover(PopoverKind::Summary);
    assert!(v.popover_animating());
    v.advance_popover(past);
    assert!(!v.popover_animating(), "opening settles");
    assert_eq!(v.popover_progress(PopoverKind::Summary), 1.0);

    v.close_popover();
    assert!(v.popover_animating());
    v.advance_popover(past + Duration::from_secs(1));
    assert!(!v.popover_animating(), "closing settles");
    assert!(
        !v.popover_visible(),
        "and the popover is gone, not merely at zero"
    );
    assert_eq!(v.popover_progress(PopoverKind::Summary), 0.0);
}

/// The chevron reads the popover's own progress rather than keeping a clock, so
/// it cannot drift out of step — and it adds nothing to `animating`.
#[test]
fn the_chevron_turns_with_the_main_page_and_no_other() {
    let (mut v, _, _) = with_context(4);
    assert_eq!(v.popover_progress(PopoverKind::Summary), 0.0);

    v.toggle_popover(PopoverKind::Profiles);
    v.advance_popover(Instant::now() + Duration::from_secs(1));
    assert_eq!(
        v.popover_progress(PopoverKind::Summary),
        0.0,
        "the Profiles page does not turn the main page's chevron"
    );
    assert_eq!(v.popover_progress(PopoverKind::Profiles), 1.0);
}

/// Asking for the other page slides to it rather than closing, and the panel
/// keeps the progress it had — so it stays on screen instead of blinking out
/// and back in.
#[test]
fn swapping_pages_keeps_the_panel_on_screen() {
    let (mut v, _, _) = with_context(4);
    v.toggle_popover(PopoverKind::Summary);
    v.advance_popover(Instant::now() + Duration::from_secs(1));
    assert_eq!(v.popover_progress(PopoverKind::Summary), 1.0);

    v.toggle_popover(PopoverKind::Profiles);
    assert!(v.popover_open());
    assert_eq!(
        v.popover_progress(PopoverKind::Profiles),
        1.0,
        "it carries its progress across rather than fading up from nothing"
    );
}

// -------------------------------------------------- what is remembered of it

/// Collapsing a node and then deleting it would otherwise leave its id in the
/// settings file forever — a slow leak of ids naming nothing.
///
/// The filtering is `rebuild`'s, against the **document**. It used to be
/// `collapsed()`'s, against the rows, and that was wrong in a way Collapse all
/// makes obvious: a node folded behind another fold contributes no row, so
/// everything below the first fold was thrown away on the way out.
#[test]
fn only_collapsed_nodes_that_still_exist_are_remembered() {
    let mut ws = doc_ws();
    let a = document(&mut ws, "a", 0);
    let b = document(&mut ws, "b", 0);
    let gone = NodeId::new();

    let mut v = ExploreView::new();
    v.set_collapsed([a, b, gone]);
    v.rebuild(&ws);

    let out = v.collapsed();
    assert!(out.contains(&a) && out.contains(&b));
    assert!(
        !out.contains(&gone),
        "a node that has gone is dropped on the next rebuild"
    );
    assert_eq!(out.len(), 2);
}

/// Shutting a window before the tree has been built once must not be read as
/// "nothing was collapsed" — that would quietly wipe the remembered set on the
/// way out, and the loss would look like the feature never worked.
#[test]
fn an_unbuilt_tree_is_not_evidence_that_nothing_was_collapsed() {
    let mut v = ExploreView::new();
    let id = NodeId::new();
    v.set_collapsed([id]);
    assert!(v.rows.is_empty());
    assert_eq!(v.collapsed(), vec![id]);
}

/// The round trip a session actually performs: collapse by clicking, hand the
/// set out, hand it back to a fresh pane.
#[test]
fn a_remembered_collapse_survives_a_restart() {
    let mut v = with_rows(4);
    let ids: Vec<NodeId> = v.rows.iter().map(|r| r.node).collect();
    let a = area();
    let rects = v.row_rects(a);
    let (_, r) = *rects.iter().find(|(i, _)| *i == 0).expect("the first row");
    let row = v.rows[0].clone();
    let (x, y) = v.caret_rect(&row, r).centre();
    v.mouse_down(a, (900.0, 700.0), x, y, false);
    assert_eq!(v.collapsed(), vec![ids[0]]);

    let mut fresh = with_rows(4);
    fresh.rows = v.rows.clone();
    fresh.set_collapsed(v.collapsed());
    assert_eq!(fresh.collapsed(), vec![ids[0]]);
}

// ----------------------------------------------------------- the row's caret

/// A quarter turn, not two glyphs: `0` points right (shut), `1` points down
/// (open). A settled caret costs no state at all, which is what keeps a tree of
/// a thousand nodes from carrying a thousand animations.
#[test]
fn a_settled_caret_needs_no_state_and_points_where_expanded_says() {
    let v = with_rows(4);
    let node = v.rows[0].node;
    assert!(v.turns.is_empty());
    assert_eq!(v.turn_of(node, true), 1.0, "open points down");
    assert_eq!(v.turn_of(node, false), 0.0, "shut points right");
}

/// Clicking the caret starts it turning, and the turn ends on its own —
/// ADR-26's rule, at the grain of a tree row.
#[test]
fn clicking_a_caret_turns_it_and_the_turn_settles() {
    let mut v = with_rows(4);
    let a = area();
    let node = v.rows[0].node;
    let rects = v.row_rects(a);
    let (_, r) = *rects.iter().find(|(i, _)| *i == 0).expect("the first row");
    let row = v.rows[0].clone();
    let (x, y) = v.caret_rect(&row, r).centre();

    v.mouse_down(a, (900.0, 700.0), x, y, false);
    assert!(v.popover_animating(), "the caret is turning");
    assert!(v.turn_of(node, false) > 0.0, "and it started from open");

    v.advance_popover(Instant::now() + Duration::from_secs(1));
    assert!(!v.popover_animating(), "it stops on its own");
    assert_eq!(v.turn_of(node, false), 0.0, "having arrived pointing right");
}

/// Clicking twice quickly turns back from where it had got to rather than
/// snapping to the far end first.
#[test]
fn a_caret_reversed_mid_turn_carries_on_from_where_it_was() {
    let mut v = with_rows(4);
    let node = v.rows[0].node;
    v.start_turn(node, false);
    v.advance_popover(Instant::now());
    let midway = v.turn_of(node, false);

    v.start_turn(node, true);
    assert_eq!(
        v.turn_of(node, true),
        midway,
        "it is where it was, not back at the start"
    );
    v.advance_popover(Instant::now() + Duration::from_secs(1));
    assert_eq!(v.turn_of(node, true), 1.0);
}

// ------------------------------------------------------- walking it by hand

/// A tree with a shape worth navigating: two top-level rows, the first with two
/// children, the second with one.
///
/// Built by hand rather than from a document, like every other geometry test
/// here — what these are about is the walk, not what earns a row.
fn a_tree() -> ExploreView {
    let mut v = ExploreView::new();
    let depths = [0u16, 1, 1, 0, 1];
    v.rows = depths
        .iter()
        .enumerate()
        .map(|(i, &depth)| Row {
            node: NodeId::new(),
            depth,
            label: format!("row {i}"),
            kind: Kind::Document,
            mime: None,
            icon: None,
            has_children: depth == 0,
            expanded: true,
            file_state: FileState::Present,
            note: None,
        })
        .collect();
    v
}

fn down() -> Key {
    Key::Named(NamedKey::ArrowDown)
}
fn up() -> Key {
    Key::Named(NamedKey::ArrowUp)
}
fn right() -> Key {
    Key::Named(NamedKey::ArrowRight)
}
fn left() -> Key {
    Key::Named(NamedKey::ArrowLeft)
}

/// Where the keyboard is, as an index — what every test below asserts on.
fn at(v: &ExploreView) -> Option<usize> {
    v.focus_index()
}

#[test]
fn up_and_down_walk_the_visible_rows_and_stop_at_the_ends() {
    let mut v = a_tree();
    let a = area();
    assert!(v.focus_tree(area()));
    assert_eq!(at(&v), Some(0));

    for expected in 1..5 {
        v.nav_key(a, &down());
        assert_eq!(at(&v), Some(expected));
    }
    v.nav_key(a, &down());
    assert_eq!(at(&v), Some(4), "the bottom is the bottom, not a wrap");

    for expected in (0..4).rev() {
        v.nav_key(a, &up());
        assert_eq!(at(&v), Some(expected));
    }
    v.nav_key(a, &up());
    assert_eq!(at(&v), Some(0), "and so is the top");
}

/// **Nothing but Enter opens anything.** Walking a tree is looking, not
/// choosing — the whole reason the keyboard's row and the open row are two
/// different pieces of state.
#[test]
fn moving_through_the_tree_opens_nothing() {
    let mut v = a_tree();
    let a = area();
    v.focus_tree(area());
    for key in [down(), down(), right(), left(), up()] {
        assert_eq!(
            v.nav_key(a, &key),
            Some(Outcome::Consumed),
            "{key:?} asked the container for something"
        );
    }
}

/// Enter opens the row the keyboard is on **and gives the keyboard back**: you
/// asked for the document, so the document should have it.
#[test]
fn enter_opens_the_focused_row_and_hands_the_keyboard_over() {
    let mut v = a_tree();
    let a = area();
    v.focus_tree(area());
    v.nav_key(a, &down());
    let want = v.rows[1].node;

    assert_eq!(
        v.nav_key(a, &Key::Named(NamedKey::Enter)),
        Some(Outcome::OpenNode(want))
    );
    assert!(!v.has_focus(), "the tree let go");
    assert_eq!(
        v.nav_key(a, &down()),
        None,
        "and the arrows fall through to the document"
    );
}

/// Right opens a shut row; pressed again — now that it is open — it steps into
/// it. One key walks a whole branch.
#[test]
fn right_opens_a_row_and_then_steps_into_it() {
    let mut v = a_tree();
    let a = area();
    v.focus_tree(area());
    let parent = v.rows[0].node;
    v.set_collapsed([parent]);

    v.nav_key(a, &right());
    assert!(
        !v.collapsed().contains(&parent),
        "the first press opened it"
    );
    assert_eq!(at(&v), Some(0), "and did not move");

    v.nav_key(a, &right());
    assert_eq!(at(&v), Some(1), "the second stepped inside");
}

/// **From anywhere in a branch, Left folds the branch and lands on what
/// contains it** — so pressed repeatedly it walks you out to the top, folding as
/// it goes. That is what "collapse to the nearest parent" means, and it is what
/// makes Left useful from a child rather than only from the row that has the
/// caret.
#[test]
fn left_folds_the_branch_you_are_in_and_lands_on_its_parent() {
    let mut v = a_tree();
    let a = area();
    v.focus_tree(area());
    v.nav_key(a, &down());
    v.nav_key(a, &down());
    assert_eq!(at(&v), Some(2), "on the second child");

    let parent = v.rows[0].node;
    v.nav_key(a, &left());
    assert!(v.collapsed().contains(&parent), "the branch folded");
    assert_eq!(v.focus, Some(parent), "and the keyboard is on it");
}

/// On a row that is open, Left shuts *that* row rather than jumping out of it —
/// the ordinary tree behaviour, which the rule above has to not break.
#[test]
fn left_on_an_open_row_shuts_that_row() {
    let mut v = a_tree();
    let a = area();
    v.focus_tree(area());
    let row = v.rows[0].node;

    v.nav_key(a, &left());
    assert!(v.collapsed().contains(&row));
    assert_eq!(at(&v), Some(0), "and stayed where it was");
}

/// Escape gives the keyboard back without opening anything — the way out that
/// is not a choice.
#[test]
fn escape_gives_the_keyboard_back() {
    let mut v = a_tree();
    let a = area();
    v.focus_tree(area());
    assert_eq!(
        v.nav_key(a, &Key::Named(NamedKey::Escape)),
        Some(Outcome::Consumed)
    );
    assert!(!v.has_focus());
}

/// **Only navigation is claimed.** Anything else falls through, so typing still
/// reaches the document even while the tree has the arrows — which is what
/// stops a focused list from making the window feel stuck.
#[test]
fn anything_that_is_not_navigation_falls_through() {
    let mut v = a_tree();
    let a = area();
    v.focus_tree(area());
    for key in [
        Key::Character("a".into()),
        Key::Named(NamedKey::Backspace),
        Key::Named(NamedKey::Home),
        Key::Named(NamedKey::Tab),
    ] {
        assert_eq!(v.nav_key(a, &key), None, "{key:?}");
    }
    assert!(v.has_focus(), "and the tree still has the arrows");
}

/// `Ctrl+E` lands on the row you already have open rather than at the top of a
/// list you would have to walk back down.
#[test]
fn taking_the_keyboard_lands_on_the_row_that_is_open() {
    let mut v = a_tree();
    let third = v.rows[3].node;
    v.set_selected(Some(third));
    assert!(v.focus_tree(area()));
    assert_eq!(v.focus, Some(third));
}

/// A focus naming a node that has gone would keep the keyboard while answering
/// for nothing, and the arrows would appear to be swallowed.
#[test]
fn a_focus_on_a_node_that_has_gone_is_dropped() {
    let mut v = a_tree();
    v.focus_tree(area());
    assert!(v.has_focus());
    // What `rebuild` does when the node is no longer in the tree.
    v.rows.clear();
    let keep = v.focus.take();
    v.focus = keep.filter(|id| v.rows.iter().any(|r| r.node == *id));
    assert!(!v.has_focus());
}

/// Clicking a row is also how the tree takes the keyboard, so the arrows carry
/// on from what was just clicked rather than from wherever they were last.
#[test]
fn clicking_a_row_puts_the_keyboard_on_it() {
    let mut v = a_tree();
    let a = area();
    let rects = v.row_rects(a);
    let (_, r) = *rects.iter().find(|(i, _)| *i == 3).expect("the fourth row");
    let (x, y) = r.centre();
    v.mouse_down(a, (900.0, 700.0), x, y, false);
    assert_eq!(at(&v), Some(3));
}

// =============================================================== drag-to-move
//
// The drag is driven over a synthetic cache the way the geometry tests are, so
// it needs no document. `relocate_carried` and `drop_target_for` are the two
// halves — where the block goes, and how the rows slide to get there.

/// `n` flat document rows; returns the view and the row node ids in order.
fn flat_rows(n: usize) -> (ExploreView, Vec<NodeId>) {
    let mut v = ExploreView::new();
    let mut ids = Vec::new();
    for _ in 0..n {
        let id = NodeId::new();
        ids.push(id);
        v.rows.push(Row {
            node: id,
            depth: 0,
            label: "d".to_string(),
            kind: Kind::Document,
            mime: None,
            icon: None,
            has_children: false,
            expanded: true,
            file_state: FileState::Present,
            note: None,
        });
    }
    (v, ids)
}

/// Where the centre of a press on row `i` lands, for pointer arithmetic.
fn row_centre(v: &ExploreView, a: Rect, i: usize) -> f32 {
    v.rows_top(a) + i as f32 * theme::NAV_ROW_H + theme::NAV_ROW_H * 0.5
}

#[test]
fn drop_zone_picks_before_above_a_quarter() {
    let (v, ids) = flat_rows(3);
    let a = area();
    // A press whose centre is just inside the top of row 1 → Before it.
    let centre = v.rows_top(a) + theme::NAV_ROW_H + theme::NAV_ROW_H * 0.1;
    let y = centre - theme::NAV_ROW_H * 0.5; // grab_dy = 0 → centre = y + H/2
    let target = v
        .drop_target_for(&HashSet::new(), y, 0.0, a)
        .expect("a target");
    assert_eq!(
        target,
        DropTarget::Row {
            node: ids[1],
            zone: DropZone::Before
        }
    );
}

#[test]
fn drop_zone_picks_after_below_three_quarters() {
    let (v, ids) = flat_rows(3);
    let a = area();
    let centre = v.rows_top(a) + theme::NAV_ROW_H + theme::NAV_ROW_H * 0.9;
    let y = centre - theme::NAV_ROW_H * 0.5;
    let target = v
        .drop_target_for(&HashSet::new(), y, 0.0, a)
        .expect("a target");
    assert_eq!(
        target,
        DropTarget::Row {
            node: ids[1],
            zone: DropZone::After
        }
    );
}

#[test]
fn drop_zone_nests_in_the_middle_of_a_document() {
    let (v, ids) = flat_rows(3);
    let a = area();
    let centre = v.rows_top(a) + theme::NAV_ROW_H + theme::NAV_ROW_H * 0.5;
    let y = centre - theme::NAV_ROW_H * 0.5;
    let target = v
        .drop_target_for(&HashSet::new(), y, 0.0, a)
        .expect("a target");
    assert_eq!(
        target,
        DropTarget::Row {
            node: ids[1],
            zone: DropZone::Nest
        }
    );
}

#[test]
fn drop_zone_skips_carried_rows() {
    // The carried block follows the pointer, so the pointer is always over one
    // of its own rows; the target has to be a *different* row.
    let (v, ids) = flat_rows(3);
    let a = area();
    let mut carried = HashSet::new();
    carried.insert(ids[1]);
    // Centre on row 1 — but it is carried, so the nearest non-carried row wins.
    let y = row_centre(&v, a, 1) - theme::NAV_ROW_H * 0.5;
    let target = v.drop_target_for(&carried, y, 0.0, a).expect("a target");
    assert_ne!(
        target,
        DropTarget::Row {
            node: ids[1],
            zone: DropZone::Nest
        },
        "a carried row is never its own target"
    );
}

#[test]
fn relocate_moves_a_root_before_the_target() {
    let (mut v, ids) = flat_rows(3); // [A, B, C]
    let a = area();
    let mut carried = HashSet::new();
    carried.insert(ids[2]); // C
    let target_i = v.rows.iter().position(|r| r.node == ids[0]).unwrap();
    v.drag = Some(RowDrag {
        grab: ids[2],
        index: 2,
        grab_dy: 0.0,
        origin_y: 0.0,
        pressed_at: Instant::now(),
        y: 0.0,
        area: a,
        armed: true,
        select_only: false,
        click: ClickAction::Open,
        carried: carried.clone(),
        carried_order: vec![ids[2]],
        item_count: 1,
        last: None,
        spring: None,
    });
    v.relocate_carried(target_i, DropZone::Before, &carried, Instant::now());
    let order: Vec<NodeId> = v.rows.iter().map(|r| r.node).collect();
    assert_eq!(order, vec![ids[2], ids[0], ids[1]], "C moved before A");
}

#[test]
fn relocate_nests_and_rebases_depth() {
    // [A(has children), Achild@d1, B] — nest A under B; A's child must deepen.
    let mut v = ExploreView::new();
    let a_id = NodeId::new();
    let child = NodeId::new();
    let b_id = NodeId::new();
    v.rows.push(Row {
        node: a_id,
        depth: 0,
        label: "A".into(),
        kind: Kind::Document,
        mime: None,
        icon: None,
        has_children: true,
        expanded: true,
        file_state: FileState::Present,
        note: None,
    });
    v.rows.push(Row {
        node: child,
        depth: 1,
        label: "c".into(),
        kind: Kind::Document,
        mime: None,
        icon: None,
        has_children: false,
        expanded: true,
        file_state: FileState::Present,
        note: None,
    });
    v.rows.push(Row {
        node: b_id,
        depth: 0,
        label: "B".into(),
        kind: Kind::Document,
        mime: None,
        icon: None,
        has_children: false,
        expanded: true,
        file_state: FileState::Present,
        note: None,
    });
    let a = area();
    let mut carried = HashSet::new();
    carried.insert(a_id);
    let target_i = v.rows.iter().position(|r| r.node == b_id).unwrap();
    v.drag = Some(RowDrag {
        grab: a_id,
        index: 0,
        grab_dy: 0.0,
        origin_y: 0.0,
        pressed_at: Instant::now(),
        y: 0.0,
        area: a,
        armed: true,
        select_only: false,
        click: ClickAction::Open,
        carried: carried.clone(),
        carried_order: vec![a_id],
        item_count: 1,
        last: None,
        spring: None,
    });
    v.relocate_carried(target_i, DropZone::Nest, &carried, Instant::now());
    let order: Vec<(NodeId, u16)> = v.rows.iter().map(|r| (r.node, r.depth)).collect();
    // B stays at the top, A becomes its child (depth 1), A's child deepens to 2.
    assert_eq!(
        order,
        vec![(b_id, 0), (a_id, 1), (child, 2)],
        "A nested under B with its subtree rebased"
    );
}

#[test]
fn an_armed_drag_ends_in_move_nodes() {
    let (mut v, ids) = flat_rows(3); // [A, B, C]
    let a = area();
    // Press on C, drag it up to just inside the top of A → Before A.
    let press_y = row_centre(&v, a, 2);
    v.mouse_down(a, VIEW, on_label(a), press_y, false);
    assert!(v.drag_active() && !v.drag_armed(), "latent on press");
    // `grab_dy` is H/2 (a centre press), and centre = y - grab_dy + H/2 = y, so
    // a y near the top of row 0 lands in its Before band.
    let y = v.rows_top(a) + theme::NAV_ROW_H * 0.1;
    v.mouse_move(a, VIEW, on_label(a), y);
    assert!(v.drag_armed(), "the move crossed the threshold");
    let out = v.mouse_up();
    match out {
        Outcome::MoveNodes { nodes, target } => {
            assert_eq!(nodes, vec![ids[2]]);
            assert_eq!(
                target,
                DropTarget::Row {
                    node: ids[0],
                    zone: DropZone::Before
                }
            );
        }
        other => panic!("expected MoveNodes, got {other:?}"),
    }
    let order: Vec<NodeId> = v.rows.iter().map(|r| r.node).collect();
    assert_eq!(order, vec![ids[2], ids[0], ids[1]]);
}

#[test]
fn a_ctrl_press_toggles_selection_without_opening() {
    let (mut v, ids) = flat_rows(2);
    let a = area();
    let y = row_centre(&v, a, 0);
    let out = v.mouse_down(a, VIEW, on_label(a), y, true);
    assert_eq!(out, Outcome::Consumed, "Ctrl does not open");
    assert!(v.selection.contains(&ids[0]));
    // Release without arming: a toggle, still no open.
    assert_eq!(v.mouse_up(), Outcome::Consumed);
}

#[test]
fn ctrl_clicks_select_individual_rows() {
    let (mut v, ids) = flat_rows(4);
    let a = area();
    for i in [0, 2] {
        v.mouse_down(a, VIEW, on_label(a), row_centre(&v, a, i), true);
        assert_eq!(v.mouse_up(), Outcome::Consumed);
    }
    assert_eq!(v.selection, HashSet::from([ids[0], ids[2]]));

    v.mouse_down(a, VIEW, on_label(a), row_centre(&v, a, 0), true);
    assert_eq!(v.mouse_up(), Outcome::Consumed);
    assert_eq!(v.selection, HashSet::from([ids[2]]), "Ctrl toggles");
}

#[test]
fn dragging_an_already_selected_row_carries_the_complete_selection() {
    let (mut v, ids) = flat_rows(4);
    let a = area();
    for i in [0, 2] {
        v.mouse_down(a, VIEW, on_label(a), row_centre(&v, a, i), true);
        v.mouse_up();
    }

    // No Ctrl is needed for the drag itself: a normal press inside the visible
    // selection preserves it until the gesture either arms or releases.
    v.mouse_down(a, VIEW, on_label(a), row_centre(&v, a, 2), false);
    v.mouse_move(a, VIEW, on_label(a), v.rows_top(a) + theme::NAV_ROW_H * 3.8);
    assert!(v.drag_armed());
    assert_eq!(
        v.rows
            .iter()
            .filter(|row| [ids[0], ids[2]].contains(&row.node))
            .count(),
        1,
        "the complete selection occupies one visual row while carried"
    );
    match v.mouse_up() {
        Outcome::MoveNodes { nodes, .. } => assert_eq!(nodes, vec![ids[0], ids[2]]),
        other => panic!("expected a multi-node move, got {other:?}"),
    }
    assert_eq!(
        v.selection,
        HashSet::from([ids[0], ids[2]]),
        "an ordinary drop keeps the moved selection"
    );
}

#[test]
fn a_drag_count_includes_documents_inside_selected_roots() {
    let mut ws = doc_ws();
    let parent = document(&mut ws, "One document", 0);
    let _first = ws
        .insert(
            Parent::Node(parent),
            Node::document("Three document").unwrap(),
        )
        .unwrap();
    let _second = ws
        .insert(
            Parent::Node(parent),
            Node::document("Two documents").unwrap(),
        )
        .unwrap();
    let other = document(&mut ws, "Testing Search", 0);
    let mut v = ExploreView::new();
    v.rebuild(&ws);
    let a = area();
    let index_of = |v: &ExploreView, node| v.rows.iter().position(|r| r.node == node).unwrap();

    for node in [parent, other] {
        let i = index_of(&v, node);
        v.mouse_down(a, VIEW, on_label(a), row_centre(&v, a, i), true);
        v.mouse_up();
    }
    let i = index_of(&v, other);
    v.mouse_down(a, VIEW, on_label(a), row_centre(&v, a, i), false);
    v.refresh_drag_item_count(&ws);

    assert_eq!(v.drag.as_ref().map(|d| d.item_count), Some(4));
}

#[test]
fn clicking_an_already_selected_row_without_dragging_collapses_to_that_row() {
    let (mut v, ids) = flat_rows(3);
    let a = area();
    for i in [0, 2] {
        v.mouse_down(a, VIEW, on_label(a), row_centre(&v, a, i), true);
        v.mouse_up();
    }
    v.mouse_down(a, VIEW, on_label(a), row_centre(&v, a, 2), false);
    assert_eq!(v.mouse_up(), Outcome::OpenNode(ids[2]));
    assert_eq!(v.selection, HashSet::from([ids[2]]));
}

/// The row cache is pre-order, so an inclusive Shift range contains nested
/// rows as well as the two endpoints instead of selecting siblings only.
#[test]
fn shift_click_selects_the_full_range_including_children() {
    let mut v = with_rows(7); // depths 0, 1, 2, 0, 1, 2, 0
    let a = area();
    let ids: Vec<_> = v.rows.iter().map(|r| r.node).collect();

    v.mouse_down(a, VIEW, on_label(a), row_centre(&v, a, 0), false);
    assert_eq!(v.mouse_up(), Outcome::OpenNode(ids[0]));
    v.mouse_down_with_modifiers(a, VIEW, on_label(a), row_centre(&v, a, 3), false, true);
    assert_eq!(v.mouse_up(), Outcome::Consumed);

    assert_eq!(
        v.selection,
        ids[..6].iter().copied().collect(),
        "both boundary roots contribute their children, but not the next sibling"
    );
}

#[test]
fn release_without_arming_drops_a_latent_drag() {
    let (mut v, ids) = flat_rows(2);
    let a = area();
    let y = row_centre(&v, a, 0);
    v.mouse_down(a, VIEW, on_label(a), y, false);
    // No move at all: a plain click.
    assert_eq!(v.mouse_up(), Outcome::OpenNode(ids[0]));
    assert!(!v.drag_active());
}

#[test]
fn clicking_a_file_selects_it_without_opening_or_saving_it() {
    let (mut v, ids) = flat_rows(3);
    for row in &mut v.rows {
        row.kind = Kind::File;
        row.label = "report.pdf".into();
    }
    let a = area();
    v.mouse_down(a, VIEW, on_label(a), row_centre(&v, a, 0), false);
    assert_eq!(v.mouse_up(), Outcome::Consumed);
    assert_eq!(v.selection, HashSet::from([ids[0]]));

    // Files use the very same Shift-range and Ctrl-toggle machinery as documents.
    v.mouse_down_with_modifiers(a, VIEW, on_label(a), row_centre(&v, a, 2), false, true);
    assert_eq!(v.mouse_up(), Outcome::Consumed);
    assert_eq!(v.selection, ids.iter().copied().collect());

    v.mouse_down(a, VIEW, on_label(a), row_centre(&v, a, 1), true);
    assert_eq!(v.mouse_up(), Outcome::Consumed);
    assert_eq!(v.selection, HashSet::from([ids[0], ids[2]]));
}

/// A grabbed node that was open folds up for the move: its children leave the
/// cache so the carried block is the root alone, and `collapsed` is untouched
/// (so the rebuilt cache after the drop opens it again). A node already shut
/// has nothing to fold.
#[test]
fn arming_folds_an_expanded_carried_root() {
    let mut v = ExploreView::new();
    let a = NodeId::new();
    let child = NodeId::new();
    let grandchild = NodeId::new();
    let b = NodeId::new();
    let row = |node: NodeId, depth: u16, has_children: bool| Row {
        node,
        depth,
        label: "d".to_string(),
        kind: Kind::Document,
        mime: None,
        icon: None,
        has_children,
        expanded: true,
        file_state: FileState::Present,
        note: None,
    };
    v.rows.push(row(a, 0, true));
    v.rows.push(row(child, 1, true));
    v.rows.push(row(grandchild, 2, false));
    v.rows.push(row(b, 0, false));

    let mut carried = HashSet::new();
    carried.insert(a);
    carried.insert(child);
    carried.insert(grandchild);
    v.collapse_carried_subtrees(&carried);

    let kept: Vec<NodeId> = v.rows.iter().map(|r| r.node).collect();
    assert_eq!(kept, vec![a, b], "A's subtree is folded away for the drag");
    // `collapsed` is the persistent truth and is not modified by the fold.
    assert!(!v.collapsed.contains(&a));
}

#[test]
fn carrying_every_visible_row_can_land_at_the_library_root() {
    let (v, ids) = flat_rows(3);
    let carried: HashSet<_> = ids.into_iter().collect();
    let a = area();
    assert_eq!(
        v.drop_target_for(&carried, row_centre(&v, a, 1), 0.0, a),
        Some(DropTarget::Library)
    );
}

/// A carried root that was already collapsed keeps nothing in the cache to
/// strip, and a sibling non-carried subtree is left alone.
#[test]
fn arming_leaves_a_collapsed_carried_root_as_is() {
    let mut v = ExploreView::new();
    let a = NodeId::new();
    let b = NodeId::new();
    let bchild = NodeId::new();
    let row = |node: NodeId, depth: u16| Row {
        node,
        depth,
        label: "d".to_string(),
        kind: Kind::Document,
        mime: None,
        icon: None,
        has_children: false,
        expanded: true,
        file_state: FileState::Present,
        note: None,
    };
    v.rows.push(row(a, 0));
    v.rows.push(row(b, 0));
    v.rows.push(row(bchild, 1));
    // A is collapsed (no children in the cache to begin with); B is open.
    let mut carried = HashSet::new();
    carried.insert(a);
    v.collapse_carried_subtrees(&carried);
    let kept: Vec<NodeId> = v.rows.iter().map(|r| r.node).collect();
    // B's subtree is not carried, so it is untouched.
    assert_eq!(kept, vec![a, b, bchild]);
}

/// Edge-band auto-scroll uses the same `dy` sign as the wheel: near the top
/// reveals content above (scroll decreases); near the bottom reveals below.
#[test]
fn drag_auto_scroll_moves_toward_the_edge_the_pointer_is_in() {
    let (mut v, ids) = flat_rows(80);
    let a = area();
    v.scroll_by(a, -theme::NAV_ROW_H * 20.0);
    let mid = v.scroll;
    assert!(mid > 0.0, "start mid-list");

    let top = v.rows_top(a);
    let bottom = v.rows_bottom(a);
    let arm = |v: &mut ExploreView, y: f32| {
        v.drag = Some(RowDrag {
            grab: ids[0],
            index: 0,
            grab_dy: 0.0,
            origin_y: y,
            pressed_at: Instant::now(),
            y,
            area: a,
            armed: true,
            select_only: false,
            click: ClickAction::Open,
            carried: HashSet::from([ids[0]]),
            carried_order: vec![ids[0]],
            item_count: 1,
            last: None,
            spring: None,
        });
        v.drag_tick = None;
    };

    // Near the top band: scroll should decrease (content above enters).
    arm(&mut v, top + 4.0);
    let t0 = Instant::now();
    let ws = doc_ws();
    v.advance_drag(&ws, t0);
    v.advance_drag(&ws, t0 + std::time::Duration::from_millis(50));
    assert!(
        v.scroll < mid,
        "top band should scroll toward earlier rows: was {mid}, now {}",
        v.scroll
    );

    v.scroll = mid;
    // Near the bottom band (library floor, not trash): scroll increases.
    arm(&mut v, bottom - 4.0);
    let t1 = Instant::now();
    v.advance_drag(&ws, t1);
    v.advance_drag(&ws, t1 + std::time::Duration::from_millis(50));
    assert!(
        v.scroll > mid,
        "bottom band should scroll toward later rows: was {mid}, now {}",
        v.scroll
    );
}

/// The edge band is the library rows, not the pinned Trash footer: parking a
/// drag on the bin must stop the auto-scroll, not keep the list crawling
/// underneath the pointer.
#[test]
fn drag_auto_scroll_stops_over_the_trash() {
    let (mut v, ids) = flat_rows(80);
    let a = area();
    v.scroll_by(a, -theme::NAV_ROW_H * 20.0);
    let mid = v.scroll;
    assert!(mid > 0.0, "start mid-list");

    let trash = v.trash_rect(a);
    let y = trash.y + trash.h * 0.5;
    v.drag = Some(RowDrag {
        grab: ids[0],
        index: 0,
        grab_dy: 0.0,
        origin_y: y,
        pressed_at: Instant::now(),
        y,
        area: a,
        armed: true,
        select_only: false,
        click: ClickAction::Open,
        carried: HashSet::from([ids[0]]),
        carried_order: vec![ids[0]],
        item_count: 1,
        last: None,
        spring: None,
    });
    v.drag_tick = None;

    let t0 = Instant::now();
    let ws = doc_ws();
    v.advance_drag(&ws, t0);
    v.advance_drag(&ws, t0 + std::time::Duration::from_millis(200));
    assert_eq!(
        v.scroll, mid,
        "over the trash the list must not scroll: was {mid}, now {}",
        v.scroll
    );
}

/// A Nest target that is collapsed becomes a spring-open candidate, so resting
/// on it will open it; a Before/After target does not.
#[test]
fn a_collapsed_nest_target_becomes_a_spring_candidate() {
    let (mut v, ids) = flat_rows(2); // [A, B], both documents
    let a = area();
    v.collapsed.insert(ids[0]); // A is collapsed
    let mut carried = HashSet::new();
    carried.insert(ids[1]); // carrying B, so A is a valid target
    let y = row_centre(&v, a, 0); // centre on row 0
    v.drag = Some(RowDrag {
        grab: ids[1],
        index: 1,
        grab_dy: theme::NAV_ROW_H * 0.5,
        origin_y: 0.0,
        pressed_at: Instant::now(),
        y,
        area: a,
        armed: true,
        select_only: false,
        click: ClickAction::Open,
        carried: carried.clone(),
        carried_order: vec![ids[1]],
        item_count: 1,
        last: None,
        spring: None,
    });
    v.update_drop(Instant::now());
    let d = v.drag.as_ref().unwrap();
    assert_eq!(
        d.last,
        Some(DropTarget::Row {
            node: ids[0],
            zone: DropZone::Nest
        })
    );
    assert_eq!(d.spring.map(|(n, _)| n), Some(ids[0]));
}

/// Dropping past the end of the list lands at the top level, however deep the
/// last visible row is — so an open branch at the bottom still accepts a drop
/// after it at depth 0.
#[test]
fn dropping_below_the_list_lands_at_level_zero() {
    let mut v = ExploreView::new();
    let a = NodeId::new();
    let child = NodeId::new();
    let other = NodeId::new(); // carried, so neither A nor child is it
    let row = |node: NodeId, depth: u16| Row {
        node,
        depth,
        label: "d".to_string(),
        kind: Kind::Document,
        mime: None,
        icon: None,
        has_children: false,
        expanded: true,
        file_state: FileState::Present,
        note: None,
    };
    v.rows.push(row(a, 0));
    v.rows.push(row(child, 1)); // A is open; child is the last visible row
    let area = area();
    let mut carried = HashSet::new();
    carried.insert(other);
    let top = v.rows_top(area);
    // Well below the last row.
    let below = top + v.rows.len() as f32 * theme::NAV_ROW_H + 50.0;
    assert_eq!(
        v.drop_target_for(&carried, below, 0.0, area),
        Some(DropTarget::Row {
            node: a,
            zone: DropZone::After
        }),
        "the drop targets the last top-level row"
    );
}

/// A collapsed document still shows a caret when it has a document descendant —
/// the chevron must not vanish just because the children are hidden.
#[test]
fn a_collapsed_doc_with_doc_children_keeps_its_chevron() {
    let mut ws = doc_ws();
    let root = document(&mut ws, "root", 0);
    let child = ws
        .insert(Parent::Node(root), Node::document("child").unwrap())
        .unwrap();
    let _grand = ws
        .insert(Parent::Node(child), Node::document("grand").unwrap())
        .unwrap();
    let mut v = ExploreView::new();
    v.collapsed.insert(child);
    v.rebuild(&ws);
    let child_row = v
        .rows
        .iter()
        .find(|r| r.node == child)
        .expect("child has a row");
    assert!(
        child_row.has_children,
        "a collapsed doc with document children keeps its chevron"
    );
}

// ------------------------------------------------------ pinned and archived

/// The switcher is a list of places to go. An archived workspace is one you
/// said to stop showing you, so it is not there.
#[test]
fn an_archived_workspace_leaves_the_switcher() {
    let (mut v, profiles, mut list) = with_context(4);
    list[1].archived = true;
    v.set_context(profiles.clone(), profiles[1].id, list.clone(), list[0].id);
    v.toggle_popover(PopoverKind::Summary);

    let ids = workspace_ids(&v, area());
    assert_eq!(ids, vec![list[0].id], "only the unarchived one is listed");
}

/// Deleted is a recovery state, not another place to navigate. The manager's
/// opt-in Deleted section is the only place that offers it, even if a stale
/// current id still names the row for the frame in which deletion completed.
#[test]
fn a_deleted_workspace_never_appears_in_the_switcher() {
    let (mut v, profiles, mut list) = with_context(4);
    list[0].deleted_at = Some(123);
    list[0].pinned = true;
    v.set_context(profiles.clone(), profiles[1].id, list.clone(), list[0].id);
    v.toggle_popover(PopoverKind::Summary);

    assert_eq!(workspace_ids(&v, area()), vec![list[1].id]);
}

/// **Except the one you are in.** A workspace can be archived while this window
/// is showing it — another window did it, or a peer did — and dropping the row
/// would leave the switcher with no tick and nothing saying where you are.
#[test]
fn the_current_workspace_is_listed_even_when_it_is_archived() {
    let (mut v, profiles, mut list) = with_context(4);
    list[0].archived = true;
    v.set_context(profiles.clone(), profiles[1].id, list.clone(), list[0].id);
    v.toggle_popover(PopoverKind::Summary);

    let ids = workspace_ids(&v, area());
    assert!(
        ids.contains(&list[0].id),
        "the one you are in is still there"
    );
}

/// Pinned workspaces sort to the top, and the rest keep the order they had —
/// so pinning one thing does not shuffle everything else.
#[test]
fn pinned_workspaces_come_first_and_the_rest_keep_their_order() {
    let (mut v, profiles, mut list) = with_context(4);
    list.push(ws("Third"));
    list[2].pinned = true;
    v.set_context(profiles.clone(), profiles[1].id, list.clone(), list[0].id);
    v.toggle_popover(PopoverKind::Summary);

    assert_eq!(
        workspace_ids(&v, area()),
        vec![list[2].id, list[0].id, list[1].id]
    );
}

/// Whatever is filtered or reordered, a row still names the workspace it draws.
/// The rows carry indices into the list, so a filter that renumbered them would
/// send every click one workspace off.
#[test]
fn a_filtered_row_still_names_the_workspace_it_shows() {
    let (mut v, profiles, mut list) = with_context(4);
    list.push(ws("Third"));
    list[0].archived = true;
    list[2].pinned = true;
    v.set_context(profiles.clone(), profiles[1].id, list.clone(), list[1].id);
    v.toggle_popover(PopoverKind::Summary);

    let a = area();
    for (row, r) in v.popover_rects(a, VIEW) {
        let PopoverRow::Workspace(i) = row else {
            continue;
        };
        let (x, y) = r.centre();
        assert_eq!(
            workspace_at(&v, a, x, y),
            Some(list[i].id),
            "row {i} points at the workspace it draws"
        );
    }
}

/// A press on the name is a switch.
#[test]
fn a_press_on_the_name_still_switches() {
    let (mut v, _, list) = with_context(4);
    let a = area();
    v.toggle_popover(PopoverKind::Summary);
    let (row, r) = v
        .popover_rects(a, VIEW)
        .into_iter()
        .find(|(row, _)| matches!(row, PopoverRow::Workspace(_)))
        .expect("a workspace row");
    let (x, y) = v.workspace_row_parts(row, r).name.centre();
    assert_eq!(
        v.mouse_down(a, VIEW, x, y, false),
        Outcome::SwitchWorkspace(list[0].id)
    );
}

/// Which workspace the switcher row at `(x, y)` names — by id, through the
/// same hit test a press uses.
fn workspace_at(v: &ExploreView, a: Rect, x: f32, y: f32) -> Option<WorkspaceId> {
    match v.popover_at(a, VIEW, x, y)? {
        (_, PopoverRow::Workspace(i)) => v.workspaces.get(i).map(|w| w.id),
        _ => None,
    }
}

/// The ids of the workspace rows the popover is showing, in order.
fn workspace_ids(v: &ExploreView, a: Rect) -> Vec<WorkspaceId> {
    v.popover_rects(a, VIEW)
        .into_iter()
        .filter_map(|(row, r)| {
            matches!(row, PopoverRow::Workspace(_)).then(|| {
                let (x, y) = r.centre();
                workspace_at(v, a, x, y).unwrap()
            })
        })
        .collect()
}

// ---------------------------------------------------------------- the trash
//
// The pinned row and the reserved `…`: a row's right-hand end and the column's
// floor now belong to something, and everything else has to keep clear of them.

/// **Pinned means reserved out of the band, not drawn on top of it.** Scrolled
/// to the very end, the last library row still stops above the Trash row. The
/// draw clip uses the same floor (`rows_bottom`), so a straddling row is cut
/// rather than painted into the trash band.
#[test]
fn the_trash_row_is_pinned_to_the_floor_and_does_not_scroll() {
    let mut v = with_rows(60);
    let a = area();
    let before = v.trash_rect(a);
    let floor = v.rows_bottom(a);
    assert_eq!(floor, before.y, "library band ends where trash begins");

    v.scroll_by(a, -100_000.0);
    assert_eq!(v.trash_rect(a), before, "it does not move with the list");
    assert_eq!(v.rows_bottom(a), floor);

    for (_, r) in v.row_rects(a) {
        assert!(
            r.y < floor,
            "a library row's top ran under the pinned Trash row"
        );
    }
}

/// **A floating row, not a block.** It sits off the floor rather than on it,
/// and it is an ordinary nav row's height — no separator, no band. A row that
/// filled the bottom edge would read as a different kind of thing from the
/// library above it, which is the opposite of what it is.
#[test]
fn the_trash_row_floats_off_the_floor_rather_than_filling_it() {
    let v = with_rows(3);
    let a = area();
    let t = v.trash_rect(a);
    assert!(t.w > 0.0);
    assert_eq!(t.h, theme::NAV_ROW_H, "an ordinary nav row");
    assert!(t.y + t.h < a.y + a.h, "air below it");
    assert_eq!(v.rows_bottom(a), t.y, "and the library stops above it");
}

/// **A list, not a special case.** The footer is whatever `footer_rows` says,
/// bottom-anchored, so adding one grows the stack upward and the library's band
/// shrinks by exactly that much. This is the test that keeps that true when the
/// second one arrives.
#[test]
fn the_footer_is_a_bottom_anchored_stack() {
    let v = with_rows(3);
    let a = area();
    let rows = v.footer_rects(a);
    assert_eq!(rows.len(), 1, "one for now");
    assert_eq!(rows[0].0, FooterRow::Trash);

    // Each is a nav row tall, they are contiguous, and the topmost is where the
    // library band ends.
    for (_, r) in &rows {
        assert_eq!(r.h, theme::NAV_ROW_H);
        assert_eq!(r.w, a.w);
    }
    assert_eq!(v.rows_bottom(a), rows[0].1.y);
}

/// A pinned row lines up with the rows above it. Drifting a few pixels left
/// would make it read as a different list.
#[test]
fn a_footer_row_shares_the_library_rows_icon_lane() {
    let v = with_rows(3);
    let a = area();
    let (i, r) = v.row_rects(a).into_iter().next().unwrap();
    let doc_icon = v.row_parts(&v.rows[i], r).icon.x;
    assert_eq!(icon_lane(a.x, 0, false), doc_icon);
}

/// The caret lane is paid for only where a caret can appear in it: a
/// Documents row keeps a file row's padding — its chevron takes the icon's
/// slot on hover (the CTO's note) — a Files row never draws one, and an
/// Agents row reserves a lane only while it has children of its own.
#[test]
fn the_caret_lane_is_reserved_only_where_a_caret_can_appear() {
    let a = area();
    let v = with_rows(1);
    let r = v.row_band(a, 0);
    let mut leaf = v.rows[0].clone();
    leaf.depth = 0;
    leaf.has_children = false;
    assert_eq!(
        v.row_parts(&leaf, r).icon.x,
        a.x + theme::NAV_PAD,
        "a documents leaf has a file row's padding"
    );
    let mut parent = leaf.clone();
    parent.has_children = true;
    let parts = v.row_parts(&parent, r);
    assert_eq!(
        parts.icon.x,
        a.x + theme::NAV_PAD,
        "and so does a documents parent"
    );
    assert_eq!(parts.caret, parts.icon, "its chevron is the icon's slot");
    let mut v = with_rows(1);
    v.mode = NavigatorMode::Files;
    assert_eq!(
        v.row_parts(&leaf, r).icon.x,
        a.x + theme::NAV_PAD,
        "a files row never draws a caret and never reserves the lane"
    );
    v.mode = NavigatorMode::Agents;
    assert_eq!(
        v.row_parts(&leaf, r).icon.x,
        a.x + theme::NAV_PAD,
        "an agents leaf"
    );
    let mut parent = leaf.clone();
    parent.has_children = true;
    let parts = v.row_parts(&parent, r);
    assert_eq!(
        parts.icon.x,
        a.x + theme::NAV_PAD + theme::NAV_INDENT,
        "an agents row with children pays for the lane"
    );
    assert_eq!(
        parts.caret.x,
        a.x + theme::NAV_PAD,
        "and its caret sits in it"
    );
}

/// **The profile square's tile is the floor chip's tile's size** (the CTO's
/// note): the two sit beside each other and read as one kind of thing.
#[test]
fn the_profile_tile_is_the_chips_tile_size() {
    assert_eq!(
        apps_rail::PROFILE_TILE,
        crate::panes::sidebar::floor::AVATAR
    );
}

/// **A Documents parent's chevron is its icon's slot** (the CTO's note,
/// Notion's shape): a press there folds the row without opening it, and
/// the `+` beside the `…` asks for a document inside the row. A Files row
/// has no `+`.
#[test]
fn a_documents_rows_slot_folds_it_and_its_plus_asks_for_a_child() {
    let a = area();
    let mut v = with_rows(3);
    let (i, r) = v.row_rects(a).into_iter().next().unwrap();
    let row = v.rows[i].clone();
    assert!(row.has_children, "the fixture's first row is a parent");
    let parts = v.row_parts(&row, r);
    assert!(parts.plus.w > 0.0, "a documents row offers a `+`");
    assert!(
        parts.plus.x + parts.plus.w <= parts.dots.x,
        "left of the `…`: {:?} vs {:?}",
        parts.plus,
        parts.dots
    );
    assert!(
        parts.label.x + parts.label.w <= parts.plus.x,
        "the label stops before it"
    );
    let (x, y) = parts.plus.centre();
    assert_eq!(
        v.mouse_down(a, VIEW, x, y, false),
        Outcome::NewChildDocument(row.node)
    );
    // The slot folds.
    let (x, y) = parts.caret.centre();
    assert!(!v.collapsed.contains(&row.node));
    assert_eq!(v.mouse_down(a, VIEW, x, y, false), Outcome::Consumed);
    assert!(v.collapsed.contains(&row.node), "folded, not opened");
    assert!(v.expand(row.node), "and unfolded again by `expand`");
    // No `+` on a file row.
    v.mode = NavigatorMode::Files;
    let mut file = row.clone();
    file.kind = Kind::File;
    assert_eq!(v.row_parts(&file, r).plus.w, 0.0);
}

/// A collapsed column has no Trash row to press.
///
/// **The guard is `area.w > 0.0` at the call site, not the rect.**
/// `Rect::contains` is inclusive on every edge, so the zero-sized rect a shut
/// column returns *does* contain its own origin — which is why every hit test in
/// this file declines a zero-width column rather than trusting the rectangle.
#[test]
fn a_collapsed_column_offers_no_trash_row() {
    let mut v = with_rows(3);
    v.trash_count = 5;
    let shut = Rect::new(0.0, 0.0, 0.0, 700.0);
    let t = v.trash_rect(shut);
    assert_eq!((t.w, t.h), (0.0, 0.0));
    assert!(
        t.contains(0.0, 0.0),
        "the rect alone is not the guard — this is why"
    );
    assert_eq!(
        v.mouse_down(shut, VIEW, 0.0, 0.0, false),
        Outcome::Consumed,
        "and the press opens nothing"
    );
}

/// **The Trash row always opens**, empty or not.
///
/// It used to dim and refuse when the bin was empty, which made sense while a
/// count sat beside it saying why. With the count gone there is nothing to
/// explain a row that will not respond, and the Trash says "The Trash is empty"
/// perfectly well once you are in it — which is a better answer than silence.
#[test]
fn the_trash_row_opens_even_when_it_is_empty() {
    let mut v = with_rows(2);
    let a = area();
    let t = v.trash_rect(a);
    assert_eq!(v.trash_count(), 0);
    assert_eq!(
        v.mouse_down(a, VIEW, t.centre().0, t.centre().1, false),
        Outcome::OpenTrash
    );
    v.mouse_move(a, VIEW, t.centre().0, t.centre().1);
    assert_eq!(
        v.cursor_icon(),
        koru_ui::cursor::PRESS,
        "and it is the plain arrow: Koru does not use the hand cursor"
    );
}

#[test]
fn a_full_trash_row_opens_the_trash() {
    let mut v = with_rows(2);
    v.trash_count = 5;
    let a = area();
    let t = v.trash_rect(a);
    assert_eq!(
        v.mouse_down(a, VIEW, t.centre().0, t.centre().1, false),
        Outcome::OpenTrash
    );
    v.mouse_move(a, VIEW, t.centre().0, t.centre().1);
    assert_eq!(v.cursor_icon(), koru_ui::cursor::PRESS);
}

/// **The reflow guard.** The label's width must not depend on whether the
/// pointer is on the row, or the name shuffles sideways as you move down the
/// list — which reads as the list twitching rather than as a button arriving.
#[test]
fn the_dots_are_reserved_so_a_label_does_not_reflow_on_hover() {
    let mut v = with_rows(3);
    let a = area();
    let rects = v.row_rects(a);
    let widths: Vec<f32> = rects
        .iter()
        .map(|(i, r)| v.row_parts(&v.rows[*i], *r).label.w)
        .collect();

    let (_, r0) = rects[0];
    v.mouse_move(a, VIEW, on_label(r0), r0.centre().1);
    let hovered: Vec<f32> = rects
        .iter()
        .map(|(i, r)| v.row_parts(&v.rows[*i], *r).label.w)
        .collect();
    assert_eq!(widths, hovered);

    // And the label really does stop before the button.
    let parts = v.row_parts(&v.rows[0], r0);
    assert!(parts.label.x + parts.label.w <= parts.dots.x);
    assert_eq!(parts.dots.x + parts.dots.w + theme::NAV_PAD, r0.x + r0.w);
}

/// The `…` sits inside the row, so it outranks it.
#[test]
fn the_dots_outrank_the_row_they_sit_in() {
    let mut v = with_rows(2);
    let a = area();
    let (i, r) = v.row_rects(a).into_iter().next().unwrap();
    let node = v.rows[i].node;
    let d = v.row_parts(&v.rows[i], r).dots;

    assert_eq!(
        v.mouse_down(a, VIEW, d.centre().0, d.centre().1, false),
        Outcome::NodeMenu {
            nodes: vec![node],
            at: (d.x + d.w, d.y + d.h),
            from_button: true,
        }
    );
}

/// **The ordering is not load-bearing, because the lane is not draggable.** A
/// press on the `…` must not seed a latent drag — if it did, the menu would
/// never open and the row would come away in your hand instead.
#[test]
fn a_press_on_the_dots_does_not_seed_a_drag() {
    let mut v = with_rows(2);
    let a = area();
    let (i, r) = v.row_rects(a).into_iter().next().unwrap();
    let d = v.row_parts(&v.rows[i], r).dots;

    v.mouse_down(a, VIEW, d.centre().0, d.centre().1, false);
    assert!(!v.drag_active(), "no latent drag under the menu button");
}

/// A right-click on a row reports its menu at the cursor rather than under a
/// button. Empty library space offers import, while pinned chrome is swallowed
/// rather than falling through to the view underneath.
#[test]
fn right_clicking_a_row_reports_its_menu_at_the_cursor() {
    let mut v = with_rows(2);
    let a = area();
    let (i, r) = v.row_rects(a).into_iter().next().unwrap();
    let node = v.rows[i].node;
    let (x, y) = (on_label(r), r.centre().1);

    assert_eq!(
        v.context_menu(a, x, y),
        Outcome::NodeMenu {
            nodes: vec![node],
            at: (x, y),
            from_button: false,
        }
    );
    assert_eq!(
        v.context_menu(
            a,
            a.x + a.w * 0.5,
            v.rows_top(a) + theme::NAV_ROW_H * 2.0 + 10.0,
        ),
        Outcome::LibraryMenu {
            at: (
                a.x + a.w * 0.5,
                v.rows_top(a) + theme::NAV_ROW_H * 2.0 + 10.0,
            )
        },
        "empty space below the rows offers import"
    );
    assert_eq!(
        v.context_menu(a, a.x + 4.0, a.y + a.h - 2.0),
        Outcome::Consumed,
        "the pinned footer is swallowed, not fallen through"
    );
}

#[test]
fn a_menu_inside_a_multi_selection_reports_every_selected_node() {
    let (mut v, ids) = flat_rows(4);
    let a = area();
    for i in [0, 2] {
        v.mouse_down(a, VIEW, on_label(a), row_centre(&v, a, i), true);
        v.mouse_up();
    }
    let y = row_centre(&v, a, 2);
    assert_eq!(
        v.context_menu(a, on_label(a), y),
        Outcome::NodeMenu {
            nodes: vec![ids[0], ids[2]],
            at: (on_label(a), y),
            from_button: false,
        }
    );
}

/// Delete is not a destructive tree shortcut. Moving a node to Trash is an
/// explicit menu action, even while the tree holds the keyboard.
#[test]
fn delete_on_the_focused_row_is_not_claimed() {
    let (mut v, ids) = flat_rows(3);
    let a = area();
    v.focus = Some(ids[0]);
    assert_eq!(v.nav_key(a, &Key::Named(NamedKey::Delete)), None);
    assert_eq!(v.focus, Some(ids[0]));
}

/// Without a focused row, `nav_key` also answers nothing and the key reaches
/// whatever is typing.
#[test]
fn delete_does_nothing_when_the_tree_does_not_hold_the_keyboard() {
    let mut v = with_rows(3);
    let a = area();
    v.clear_focus();
    assert_eq!(v.nav_key(a, &Key::Named(NamedKey::Delete)), None);
}

// ------------------------------------------------------- the empty column

/// **The bug this pins:** an empty library used to `return` out of `draw`
/// before the pinned rows, so a Trash row with seven things in it vanished the
/// moment you deleted your last document — exactly when you most needed it.
#[test]
fn an_empty_library_still_shows_the_pinned_rows() {
    let mut v = with_rows(0);
    v.trash_count = 7;
    let a = area();

    assert!(v.row_rects(a).is_empty(), "the fixture is empty");
    let rows = v.footer_rects(a);
    assert_eq!(rows.len(), 1, "the footer does not go with the library");
    assert_eq!(
        v.mouse_down(a, VIEW, rows[0].1.centre().0, rows[0].1.centre().1, false),
        Outcome::OpenTrash,
        "and it still opens"
    );
}

/// A column that only explains why it is empty leaves you to find the way out
/// somewhere else. The button is laid out at draw time — its position depends on
/// how the sentence wraps — so it is not a target until the column has drawn.
#[test]
fn the_empty_column_offers_a_way_to_make_a_document() {
    let mut v = with_rows(0);
    v.current = Some(WorkspaceId::new());
    let a = area();
    assert_eq!(v.empty_cta, None, "nothing before the first frame");

    draw_once(&mut v, a);
    let cta = v.empty_cta.expect("a button was laid out");
    assert!(cta.w > 0.0 && cta.h > 0.0);
    assert_eq!(
        v.mouse_down(a, VIEW, cta.centre().0, cta.centre().1, false),
        Outcome::NewDocument
    );
}

#[test]
fn a_profile_without_a_workspace_offers_workspace_creation_first() {
    let mut v = with_rows(0);
    v.current = None;
    let a = area();

    assert_eq!(
        v.empty_state(),
        (
            "There isn't a workspace here yet. Create one to start adding content.",
            "Create workspace",
            Outcome::NewWorkspace,
        )
    );
    draw_once(&mut v, a);
    let cta = v.empty_cta.expect("a workspace button was laid out");
    assert_eq!(
        v.mouse_down(a, VIEW, cta.centre().0, cta.centre().1, false),
        Outcome::NewWorkspace
    );
}

/// A profile waiting on another device's workspaces is told it can also keep
/// them in Koru Cloud, so it gets the way there under Create workspace — and a
/// profile with nothing on its way does not.
#[test]
fn a_profile_awaiting_workspaces_offers_a_cloud_subscription() {
    let mut v = with_rows(0);
    v.current = None;
    let profile = ProfileId::new();
    v.current_profile = Some(profile);
    v.set_account_state(&koru_agent_wire::AccountState {
        enrollments: vec![koru_agent_wire::AccountProfileEnrollment {
            profile_id: profile.to_storage(),
            state: koru_agent_wire::EnrollmentState::Complete,
            failure_code: None,
            retry_at_millis: None,
        }],
        ..Default::default()
    });
    let a = area();
    draw_once(&mut v, a);
    let create = v.empty_cta.expect("Create workspace");
    let subscribe = v.empty_subscribe.expect("Subscribe to Koru Cloud");
    assert!(subscribe.y >= create.y + create.h, "under Create workspace");
    assert_eq!(
        v.mouse_down(a, VIEW, subscribe.centre().0, subscribe.centre().1, false),
        Outcome::SubscribeCloud
    );

    v.current_profile = Some(ProfileId::new());
    draw_once(&mut v, a);
    assert!(v.empty_cta.is_some());
    assert_eq!(
        v.empty_subscribe, None,
        "nothing on its way, no Cloud offer"
    );
}

/// ...and it is not there once there is something to show.
#[test]
fn a_column_with_rows_offers_no_call_to_action() {
    let mut v = with_rows(3);
    let a = area();
    draw_once(&mut v, a);
    assert_eq!(v.empty_cta, None);
}

// ------------------------------------------------- dragging into the trash

/// Dropping a row on the Trash row bins it. Same gesture as a reorder, different
/// landing — which is why it is answered before `drop_target_for` gets a look.
#[test]
fn dropping_a_row_on_the_trash_bins_it() {
    let (mut v, ids) = flat_rows(3);
    let a = area();
    let press_y = row_centre(&v, a, 0);
    v.mouse_down(a, VIEW, on_label(a), press_y, false);

    // Drag down onto the pinned row.
    let t = v.trash_rect(a);
    v.mouse_move(a, VIEW, on_label(a), t.centre().1);
    assert!(v.drag_armed(), "the move crossed the threshold");
    assert!(v.trash_is_drop_target(a), "and it reads as a target");

    assert_eq!(v.mouse_up(), Outcome::TrashNodes(vec![ids[0]]));
}

/// **The pinned row is below the library's band**, and `drop_target_for` only
/// knows about rows in it — left to itself it answers "after the last one" for a
/// pointer down here, which would quietly reorder instead of binning.
#[test]
fn a_drop_below_the_library_bins_rather_than_reorders() {
    let (mut v, ids) = flat_rows(3);
    let a = area();
    v.mouse_down(a, VIEW, on_label(a), row_centre(&v, a, 2), false);
    let t = v.trash_rect(a);
    v.mouse_move(a, VIEW, on_label(a), t.centre().1);

    match v.mouse_up() {
        Outcome::TrashNodes(nodes) => assert_eq!(nodes, vec![ids[2]]),
        other => panic!("reordered instead of binning: {other:?}"),
    }
}

/// A drag that ends anywhere else still reorders, so the new landing has not
/// swallowed the old one.
#[test]
fn a_drop_inside_the_library_still_moves() {
    let (mut v, _) = flat_rows(3);
    let a = area();
    v.mouse_down(a, VIEW, on_label(a), row_centre(&v, a, 2), false);
    let y = v.rows_top(a) + theme::NAV_ROW_H * 0.1;
    v.mouse_move(a, VIEW, on_label(a), y);
    assert!(matches!(v.mouse_up(), Outcome::MoveNodes { .. }));
}

/// It is only a target while a drag is actually in flight.
#[test]
fn the_trash_is_not_a_drop_target_at_rest() {
    let mut v = with_rows(3);
    let a = area();
    assert!(!v.trash_is_drop_target(a));
    // A latent press is not an armed drag either.
    let (_, r) = v.row_rects(a).into_iter().next().unwrap();
    v.mouse_down(a, VIEW, on_label(r), r.centre().1, false);
    assert!(!v.trash_is_drop_target(a));
}

/// The active pinned row wears the same card an open document does, because it
/// means the same thing — and both come from `row_chip`, so they cannot drift
/// into two shapes.
#[test]
fn the_active_pinned_row_is_told_which_one_it_is() {
    let mut v = with_rows(2);
    assert!(v.set_footer_active(Some(FooterRow::Trash)));
    assert!(!v.set_footer_active(Some(FooterRow::Trash)), "idempotent");
    assert!(v.set_footer_active(None));
}

/// Every row in the column is drawn on one shape.
#[test]
fn a_pinned_row_and_a_library_row_share_a_chip() {
    let v = with_rows(2);
    let a = area();
    let (_, lib) = v.row_rects(a).into_iter().next().unwrap();
    let trash = v.trash_rect(a);
    // Same inset and same width, whatever the row is about.
    assert_eq!(row_chip(lib).rect().x0, row_chip(trash).rect().x0);
    assert_eq!(row_chip(lib).rect().width(), row_chip(trash).rect().width());
}

#[test]
fn adjacent_node_chips_have_two_pixels_of_visible_air() {
    let first = Rect::new(20.0, 40.0, 220.0, theme::NAV_ROW_H);
    let second = Rect::new(20.0, 40.0 + theme::NAV_ROW_H, 220.0, theme::NAV_ROW_H);
    let a = row_chip(first).rect();
    let b = row_chip(second).rect();
    assert_eq!(b.y0 - a.y1, 2.0);
}

#[test]
fn trash_drop_copy_reports_the_carried_item_count() {
    assert_eq!(trash_drop_label(1), "Move 1 item to trash");
    assert_eq!(trash_drop_label(3), "Move 3 items to trash");
}

#[test]
fn a_collapsed_multi_drag_labels_its_single_representative() {
    assert_eq!(multi_drag_label("Paste Test", 6), "6 items · Paste Test");
}

// ------------------------------------------------- the manifest's rows

/// A workspace as the manifest lists it: `(title, parent)`, in Explore's order.
fn entries(rows: &[(&str, Option<usize>)]) -> Vec<ExploreEntry> {
    let ids: Vec<DocumentId> = rows.iter().map(|_| DocumentId::new()).collect();
    rows.iter()
        .enumerate()
        .map(|(i, (title, parent))| ExploreEntry {
            id: ids[i],
            parent: parent.map(|p| ids[p]),
            ordinal: i as u32,
            title: (*title).to_owned(),
            kind: koru_core::explore::KIND_DOCUMENT.to_owned(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        })
        .collect()
}

#[test]
fn navigator_modes_project_pages_and_files_from_one_manifest_cache() {
    let mut rows = entries(&[("Page", None), ("report.pdf", None)]);
    rows[1].kind = koru_core::explore::KIND_FILE.to_owned();
    let mut view = ExploreView::new();
    view.rebuild_from_entries(&rows);
    assert_eq!(labels(&view), ["Page"]);

    view.show_files();
    assert_eq!(labels(&view), ["report.pdf"]);
    assert_eq!(view.rows[0].kind, Kind::File);

    view.show_documents();
    assert_eq!(labels(&view), ["Page"]);
}

/// **Eight apps, one row of chips**, and each of them hit-tests back to
/// itself — the apps row (`docs/features/workspace-foundation/TECHNICAL.final.md`
/// §5.2), under the header and above the heading band.
///
/// **The apps are squares down the rail**, whatever the pane's width: each
/// inside the rail, each hit-testing to its own mode, *Manage apps* under
/// the last, and the heading's `…` untouched by any of them (the CTO's
/// artboard 11).
#[test]
fn every_navigator_mode_button_hit_tests_back_to_its_own_mode() {
    let view = ExploreView::new();
    for width in [
        crate::mainview::layout::SIDEBAR_MIN_W - theme::RAIL_W,
        theme::SIDEBAR_W,
        400.0,
    ] {
        let a = Rect::new(theme::RAIL_W, theme::CAPTION_H, width, 600.0);
        let rail = view.rail_of(a);
        assert_eq!(rail.x, 0.0, "the rail is before the pane");
        assert_eq!(rail.w, theme::RAIL_W);
        assert_eq!(rail.y, a.y + theme::CAPTION_H, "under the lane");
        let rects = view.navigator_mode_rects(rail);
        assert_eq!(rects.len(), NavigatorMode::ALL.len());
        for &(mode, r) in &rects {
            assert_eq!(r.w, apps_rail::SQUARE, "{mode:?} is a square");
            assert!(
                r.x >= rail.x && r.x + r.w <= rail.x + rail.w,
                "{mode:?} fits inside the rail"
            );
            let (x, y) = r.centre();
            assert_eq!(mode_at(&view, a, x, y), Some(mode), "{mode:?}");
            assert!(!view.actions_at(a, x, y), "{mode:?} is not the ... button");
        }
        let manage = view.apps_rail_rects(rail).manage;
        let last = rects.last().unwrap().1;
        assert_eq!(
            manage.y,
            last.y + last.h + apps_rail::GAP,
            "Manage apps under the last"
        );
        assert!(view.actions_button(a).w > 0.0, "the `…` shows at {width}");
    }
}

/// Tasks is a mode of this column, not a tab: pressing its icon swaps what the
/// column below shows and tells the container nothing.
///
/// **And the documents mode comes back as it was.** The rail owns no library
/// rows, so the manifest cache is what restores them — the same contract the
/// Files trip already keeps.
#[test]
fn switching_to_tasks_and_back_leaves_the_documents_mode_alone() {
    let rows = entries(&[("Parent", None), ("Child", Some(0)), ("Other", None)]);
    let parent = NodeId::from_uuid(rows[0].id.as_uuid());
    let mut view = ExploreView::new();
    view.set_collapsed([parent]);
    view.rebuild_from_entries(&rows);
    assert_eq!(labels(&view), ["Parent", "Other"]);
    view.scroll = 0.0;
    let a = area();

    let (_, tasks) = view
        .navigator_mode_rects(view.rail_of(a))
        .into_iter()
        .find(|(mode, _)| *mode == NavigatorMode::Tasks)
        .unwrap();
    let (x, y) = tasks.centre();
    assert_eq!(
        view.apps_rail_mouse_down(view.rail_of(a), x, y),
        Outcome::Consumed
    );
    assert_eq!(view.navigator_mode(), NavigatorMode::Tasks);
    assert!(view.showing_tasks());
    assert!(view.rows.is_empty(), "the rail is not the library");
    // The manifest rows are not rebuilt while it is showing, either.
    view.rebuild_from_entries(&rows);
    assert!(view.rows.is_empty());

    view.show_documents();
    assert_eq!(labels(&view), ["Parent", "Other"]);
    assert!(view.collapsed().contains(&parent));
}

/// A press in the rail is reported, never performed — chrome's standing
/// contract, and the seam the Tasks tab is opened through.
#[test]
fn a_press_on_a_rail_row_reports_the_bucket() {
    let mut view = ExploreView::new();
    view.show_tasks();
    let a = area();
    let top = view.rail_top(a);
    let (row, r) = view.rail().rows(a, top)[0];
    assert_eq!(row, rail::RailRow::Bucket(rail::Bucket::Today));
    let (x, y) = r.centre();
    assert_eq!(
        view.mouse_down(a, (1200.0, 800.0), x, y, false),
        Outcome::Tasks(rail::TasksRailRequest::OpenBucket {
            bucket: rail::Bucket::Today,
            new_tab: false
        })
    );
    // The column's background is still ours: a press on it must not fall
    // through to the view beside it.
    assert_eq!(
        view.mouse_down(a, (1200.0, 800.0), a.x + a.w * 0.5, a.y + a.h - 2.0, false),
        Outcome::Consumed
    );
}

/// The rail draws — every row of it, with a field open and with counts on —
/// and leaves the scene balanced.
///
/// A `push_clip_layer` without its `pop_layer` does not fail here: it corrupts
/// every frame *after* this one, which is the kind of bug a smoke test is
/// worth having for. It also exercises the rotated glyph and the two-part
/// count, neither of which any geometry test touches.
#[test]
fn drawing_the_rail_leaves_the_scene_balanced() {
    let mut v = ExploreView::new();
    v.show_tasks();
    v.rail_mut().set_counts(rail::TaskCounts {
        known: true,
        capped: true,
        today: 5,
        today_overdue: 3,
        next: 5,
        upcoming: 12,
        later: 28,
        completed: 122,
    });
    v.rail_mut().set_lists(vec![
        rail::RailList {
            id: koru_core::ids::ListId::new(),
            name: "Wedding".into(),
            color: Some("pink".into()),
            count: 14,
            overdue: 0,
        },
        rail::RailList {
            id: koru_core::ids::ListId::new(),
            name: "Bike Project".into(),
            color: Some("blue".into()),
            count: 6,
            overdue: 1,
        },
        rail::RailList {
            id: koru_core::ids::ListId::new(),
            name: "A list with no colour at all".into(),
            color: None,
            count: 0,
            overdue: 0,
        },
    ]);
    let a = area();
    draw_once(&mut v, a);
    v.rail_mut().begin_new_list();
    draw_once(&mut v, a);
    // A collapsed column draws nothing and must not panic doing it.
    draw_once(&mut v, Rect::new(0.0, theme::CAPTION_H, 0.0, 600.0));
}

/// Tasks has no library actions, so it draws no `…`: a zero-width rect, the
/// idiom a hidden control uses, rather than a button that does nothing.
#[test]
fn the_tasks_mode_offers_no_library_actions() {
    let mut view = ExploreView::new();
    let a = area();
    assert!(view.actions_button(a).w > 0.0);
    view.show_tasks();
    assert_eq!(view.actions_button(a).w, 0.0);
}

/// Files are appended as they arrive, so tree order is oldest first. The Files
/// projection reverses that so the latest addition is on top.
#[test]
fn files_are_listed_newest_first() {
    let mut rows = entries(&[("oldest", None), ("middle", None), ("newest", None)]);
    for row in &mut rows {
        row.kind = koru_core::explore::KIND_FILE.to_owned();
    }
    let mut v = ExploreView::new();
    v.show_files();
    v.rebuild_from_entries(&rows);
    assert_eq!(labels(&v), ["newest", "middle", "oldest"]);
}

/// Switching projections must not reinterpret hidden document rows as deleted.
/// The collapsed set belongs to the workspace tree, so a trip through Files
/// leaves the document tree exactly as somebody arranged it.
#[test]
fn switching_to_files_and_back_preserves_document_folds() {
    let mut rows = entries(&[("Parent", None), ("Child", Some(0)), ("report.pdf", None)]);
    rows[2].kind = koru_core::explore::KIND_FILE.to_owned();
    let parent = NodeId::from_uuid(rows[0].id.as_uuid());
    let mut view = ExploreView::new();
    view.set_collapsed([parent]);
    view.rebuild_from_entries(&rows);
    assert_eq!(labels(&view), ["Parent"]);

    view.show_files();
    assert_eq!(labels(&view), ["report.pdf"]);

    view.show_documents();
    assert_eq!(labels(&view), ["Parent"]);
    assert!(view.collapsed().contains(&parent));
}

/// **Collapse all and expand all work on the manifest's rows too.**
///
/// They were applied only in `rebuild`, which walks the *document* — so
/// store-backed, where the rows come from the manifest, both buttons set a flag
/// nothing ever read and pressing them did nothing at all.
#[test]
fn collapse_all_folds_the_manifests_rows() {
    let rows = entries(&[("root", None), ("child", Some(0)), ("grand", Some(1))]);
    let mut v = ExploreView::new();
    v.rebuild_from_entries(&rows);
    assert_eq!(labels(&v), ["root", "child", "grand"]);

    v.pending_fold = Some(fold::Fold::Collapse);
    v.rebuild_from_entries(&rows);
    assert_eq!(labels(&v), ["root"], "only the top level is left");

    v.pending_fold = Some(fold::Fold::Expand);
    v.rebuild_from_entries(&rows);
    assert_eq!(labels(&v), ["root", "child", "grand"]);
    assert!(v.collapsed().is_empty());
}

/// The tree's rule, kept: a row with nothing under it earns no fold, or
/// collapsing everything leaves a caret on a document with nothing beneath it.
#[test]
fn collapsing_the_manifest_leaves_childless_documents_alone() {
    let rows = entries(&[("lone", None), ("parent", None), ("kid", Some(1))]);
    let mut v = ExploreView::new();
    v.pending_fold = Some(fold::Fold::Collapse);
    v.rebuild_from_entries(&rows);

    let lone = NodeId::from_uuid(rows[0].id.as_uuid());
    let parent = NodeId::from_uuid(rows[1].id.as_uuid());
    assert!(!v.collapsed().contains(&lone));
    assert!(v.collapsed().contains(&parent));
}

/// **Opening a search result opens the tree it lives in.**
///
/// The hit names a document anywhere in the library; the branch above it is
/// very often shut, and before this the row was highlighted where nobody could
/// see it. Every ancestor is unfolded, not just the immediate parent — a
/// document three levels down is exactly the one hardest to find by hand.
#[test]
fn revealing_a_row_opens_every_fold_above_it() {
    let rows = entries(&[("root", None), ("child", Some(0)), ("grand", Some(1))]);
    let ids: Vec<NodeId> = rows
        .iter()
        .map(|e| NodeId::from_uuid(e.id.as_uuid()))
        .collect();
    let mut v = ExploreView::new();
    v.pending_fold = Some(fold::Fold::Collapse);
    v.rebuild_from_entries(&rows);
    assert_eq!(labels(&v), ["root"]);

    assert!(v.reveal(ids[2]), "two folds had to open");
    v.rebuild_from_entries(&rows);
    assert_eq!(labels(&v), ["root", "child", "grand"]);
}

/// The folds a reveal opens are the ones **above** it. Its own stays shut, so
/// finding a document does not spill its children over the tree.
#[test]
fn revealing_a_row_leaves_its_own_fold_shut() {
    let rows = entries(&[("root", None), ("child", Some(0)), ("grand", Some(1))]);
    let ids: Vec<NodeId> = rows
        .iter()
        .map(|e| NodeId::from_uuid(e.id.as_uuid()))
        .collect();
    let mut v = ExploreView::new();
    v.pending_fold = Some(fold::Fold::Collapse);
    v.rebuild_from_entries(&rows);

    v.reveal(ids[1]);
    v.rebuild_from_entries(&rows);
    assert_eq!(
        labels(&v),
        ["root", "child"],
        "'grand' is still folded away"
    );
}

/// A reveal of something already visible changes no fold — the caller uses that
/// answer to decide whether the rows need walking again, so a needless rebuild
/// on every result opened would be the cost of getting it wrong.
#[test]
fn revealing_an_already_visible_row_changes_nothing() {
    let rows = entries(&[("root", None), ("child", Some(0))]);
    let child = NodeId::from_uuid(rows[1].id.as_uuid());
    let mut v = ExploreView::new();
    v.rebuild_from_entries(&rows);

    assert!(!v.reveal(child));
}

/// The scroll half: a row below the band comes into it, and the request is
/// spent once it has.
#[test]
fn revealing_a_row_below_the_band_scrolls_to_it() {
    let titles: Vec<(String, Option<usize>)> =
        (0..200).map(|i| (format!("doc {i}"), None)).collect();
    let borrowed: Vec<(&str, Option<usize>)> =
        titles.iter().map(|(t, p)| (t.as_str(), *p)).collect();
    let rows = entries(&borrowed);
    let last = NodeId::from_uuid(rows[199].id.as_uuid());
    let mut v = ExploreView::new();
    v.rebuild_from_entries(&rows);
    assert_eq!(v.scroll, 0.0);

    v.reveal(last);
    assert!(v.apply_reveal(area()), "the band does not hold 200 rows");
    let y = 199.0 * theme::NAV_ROW_H - v.scroll;
    assert!(
        y >= 0.0 && y + theme::NAV_ROW_H <= v.rows_bottom(area()) - v.rows_top(area()),
        "row 199 is at {y} in a band of {}",
        v.rows_bottom(area()) - v.rows_top(area())
    );
    assert!(!v.apply_reveal(area()), "the request was spent");
}

/// **A reveal survives a miss.** The rows are rebuilt on the pane's own
/// schedule and the window may still be swapping to the workspace the document
/// is in, so asking before the row exists is "not yet" rather than "not here".
#[test]
fn a_reveal_waits_for_the_row_to_exist() {
    let rows = entries(&[("a", None), ("b", None)]);
    let b = NodeId::from_uuid(rows[1].id.as_uuid());
    let mut v = ExploreView::new();

    v.reveal(b);
    assert!(!v.apply_reveal(area()), "nothing is built yet");

    v.rebuild_from_entries(&rows);
    v.scroll = 400.0;
    assert!(v.apply_reveal(area()), "the row is here now");
}

/// What somebody shut is restored from `Settings` **before** the shell has
/// pushed anything, so an empty list must not be read as "none of these exist".
#[test]
fn an_empty_push_does_not_forget_what_was_collapsed() {
    let remembered = NodeId::new();
    let mut v = ExploreView::new();
    v.set_collapsed(vec![remembered]);

    v.rebuild_from_entries(&[]);
    assert!(
        v.collapsed().contains(&remembered),
        "the restored set was pruned against a list that had not arrived yet"
    );

    // A real list does prune it — that id names no document in this workspace.
    v.rebuild_from_entries(&entries(&[("a", None)]));
    assert!(!v.collapsed().contains(&remembered));
}

/// **A row unfolded by a reveal can be folded again.**
///
/// Reported from a running window: the tree opened to a search result and then
/// would not collapse. The suspicion was that something re-ran the reveal; this
/// drives the whole gesture — reveal, rebuild, click the caret, rebuild — and
/// says whether it does.
#[test]
fn a_revealed_row_can_be_collapsed_again() {
    let rows = entries(&[("root", None), ("child", Some(0)), ("grand", Some(1))]);
    let ids: Vec<NodeId> = rows
        .iter()
        .map(|e| NodeId::from_uuid(e.id.as_uuid()))
        .collect();
    let mut v = ExploreView::new();
    v.pending_fold = Some(fold::Fold::Collapse);
    v.rebuild_from_entries(&rows);
    assert_eq!(labels(&v), ["root"]);

    assert!(v.reveal(ids[2]));
    v.rebuild_from_entries(&rows);
    assert_eq!(labels(&v), ["root", "child", "grand"]);
    v.apply_reveal(area());

    // The caret of "root", clicked the way a person clicks it.
    let area = area();
    let (_, r) = v.row_rects(area).into_iter().next().expect("the root row");
    let row = v.rows[0].clone();
    let (cx, cy) = v.caret_rect(&row, r).centre();
    v.mouse_down_with_modifiers(area, (1200.0, 800.0), cx, cy, false, false);
    v.rebuild_from_entries(&rows);

    assert_eq!(labels(&v), ["root"], "the fold would not close again");
}

/// **A reveal works before the rows are walked.**
///
/// The order in the app is: push the listing, unfold the branch, mark the pane
/// stale, rebuild at the next draw. So `reveal` runs against a parent map that
/// has to be current *without* a rebuild — and on a workspace switch the map
/// from the previous rebuild describes the workspace you just left.
#[test]
fn folds_can_be_opened_from_a_listing_that_has_not_been_walked_yet() {
    let rows = entries(&[("root", None), ("child", Some(0)), ("grand", Some(1))]);
    let ids: Vec<NodeId> = rows
        .iter()
        .map(|e| NodeId::from_uuid(e.id.as_uuid()))
        .collect();
    let mut v = ExploreView::new();
    // Everything shut, and no rebuild has run for *this* listing — the state a
    // window is in between switching workspace and its next draw.
    v.set_collapsed(vec![ids[0], ids[1]]);
    v.note_parents(&rows);

    assert!(v.reveal(ids[2]), "the branch above it did not open");
    v.rebuild_from_entries(&rows);
    assert_eq!(labels(&v), ["root", "child", "grand"]);
}

// ------------------------------------------------------- files held elsewhere

/// A file's reference replicates with the manifest; its bytes do not follow. So
/// a row can be listed here and held only on another device, and the click that
/// lands on it is a request for those bytes rather than a selection.
#[test]
fn a_file_held_on_another_device_downloads_on_a_plain_click() {
    let mut rows = entries(&[("report.pdf", None), ("notes.txt", None)]);
    rows[0].kind = koru_core::explore::KIND_FILE.to_owned();
    rows[1].kind = koru_core::explore::KIND_FILE.to_owned();
    let remote = NodeId::from_uuid(rows[0].id.as_uuid());
    let present = NodeId::from_uuid(rows[1].id.as_uuid());
    let mut v = ExploreView::new();
    v.show_files();
    v.rebuild_from_entries(&rows);
    assert!(v.set_file_states(HashMap::from([(remote, FileState::Remote)])));
    let a = area();

    // Newest first: notes.txt (present) is on top, report.pdf (remote) below.
    v.mouse_down(a, VIEW, on_label(a), row_centre(&v, a, 1), false);
    assert_eq!(v.mouse_up(), Outcome::DownloadFile(remote));

    // The one whose bytes are already here keeps today's behaviour: selecting
    // only, so a stray click never launches another application.
    v.mouse_down(a, VIEW, on_label(a), row_centre(&v, a, 0), false);
    assert_eq!(v.mouse_up(), Outcome::Consumed);
    assert_eq!(v.selection, HashSet::from([present]));
}

/// Double-click is the default open: a present file launches in the associated
/// app, a remote file still downloads.
#[test]
fn double_clicking_a_file_opens_it_in_the_associated_app() {
    let mut rows = entries(&[("report.pdf", None), ("notes.txt", None)]);
    rows[0].kind = koru_core::explore::KIND_FILE.to_owned();
    rows[1].kind = koru_core::explore::KIND_FILE.to_owned();
    let remote = NodeId::from_uuid(rows[0].id.as_uuid());
    let present = NodeId::from_uuid(rows[1].id.as_uuid());
    let mut v = ExploreView::new();
    v.show_files();
    v.rebuild_from_entries(&rows);
    assert!(v.set_file_states(HashMap::from([(remote, FileState::Remote)])));
    let a = area();

    assert_eq!(
        v.mouse_double_click(a, VIEW, on_label(a), row_centre(&v, a, 0)),
        Outcome::OpenFile(present)
    );
    assert_eq!(
        v.mouse_double_click(a, VIEW, on_label(a), row_centre(&v, a, 1)),
        Outcome::DownloadFile(remote)
    );
}

/// `Enter` in the Files projection still means Save as. Reusing `OpenNode` for
/// the download would have changed that silently, which is why it is its own
/// outcome.
#[test]
fn downloading_is_a_click_and_never_what_enter_means() {
    let mut rows = entries(&[("report.pdf", None)]);
    rows[0].kind = koru_core::explore::KIND_FILE.to_owned();
    let file = NodeId::from_uuid(rows[0].id.as_uuid());
    let mut v = ExploreView::new();
    v.show_files();
    v.rebuild_from_entries(&rows);
    v.set_file_states(HashMap::from([(file, FileState::Remote)]));
    v.focus = Some(file);

    assert_eq!(
        v.nav_key(area(), &Key::Named(NamedKey::Enter)),
        Some(Outcome::OpenNode(file))
    );
}

/// Ctrl and Shift are for building a selection. A modified click on a
/// placeholder must not start a transfer as a side effect of picking rows.
#[test]
fn a_modified_click_selects_a_remote_file_without_fetching_it() {
    let mut rows = entries(&[("a.pdf", None), ("b.pdf", None)]);
    for row in &mut rows {
        row.kind = koru_core::explore::KIND_FILE.to_owned();
    }
    let ids: Vec<NodeId> = rows
        .iter()
        .map(|row| NodeId::from_uuid(row.id.as_uuid()))
        .collect();
    let mut v = ExploreView::new();
    v.show_files();
    v.rebuild_from_entries(&rows);
    v.set_file_states(ids.iter().map(|id| (*id, FileState::Remote)).collect());
    let a = area();

    v.mouse_down_with_modifiers(a, VIEW, on_label(a), row_centre(&v, a, 0), false, true);
    assert_eq!(v.mouse_up(), Outcome::Consumed);
    v.mouse_down_with_modifiers(a, VIEW, on_label(a), row_centre(&v, a, 1), false, true);
    assert_eq!(v.mouse_up(), Outcome::Consumed);
    assert_eq!(v.selection, ids.iter().copied().collect());
}

/// A rebuild must not lose what the device knows about its own files, or the
/// list would flash back to ordinary rows every time the manifest moved.
#[test]
fn file_states_survive_a_rebuild_and_a_trip_through_documents() {
    let mut rows = entries(&[("Page", None), ("report.pdf", None)]);
    rows[1].kind = koru_core::explore::KIND_FILE.to_owned();
    let file = NodeId::from_uuid(rows[1].id.as_uuid());
    let mut v = ExploreView::new();
    v.show_files();
    v.rebuild_from_entries(&rows);
    v.set_file_states(HashMap::from([(file, FileState::Remote)]));
    assert_eq!(v.rows[0].file_state, FileState::Remote);

    v.rebuild_from_entries(&rows);
    assert_eq!(v.rows[0].file_state, FileState::Remote);

    v.show_documents();
    v.show_files();
    assert_eq!(v.rows[0].file_state, FileState::Remote);
}

/// Pressing a placeholder has to change it immediately: the fetch may park on a
/// peer that is asleep, and a press with no visible answer reads as a miss.
#[test]
fn a_row_can_be_marked_fetching_without_a_whole_sweep() {
    let mut rows = entries(&[("report.pdf", None)]);
    rows[0].kind = koru_core::explore::KIND_FILE.to_owned();
    let file = NodeId::from_uuid(rows[0].id.as_uuid());
    let mut v = ExploreView::new();
    v.show_files();
    v.rebuild_from_entries(&rows);
    v.set_file_states(HashMap::from([(file, FileState::Remote)]));
    assert!(v.is_remote_file(file));

    v.set_file_state(file, FileState::Fetching);
    assert_eq!(v.rows[0].file_state, FileState::Fetching);
    // No longer offering the download: a second click must not start a second
    // transfer of the same bytes.
    assert!(!v.is_remote_file(file));
    let a = area();
    v.mouse_down(a, VIEW, on_label(a), row_centre(&v, a, 0), false);
    assert_eq!(v.mouse_up(), Outcome::Consumed);
}

/// An unswept file draws as an ordinary row. Guessing the other way would make
/// every list twitch through placeholders on its way to the truth.
#[test]
fn a_file_with_no_locality_answer_yet_is_treated_as_present() {
    let mut rows = entries(&[("report.pdf", None)]);
    rows[0].kind = koru_core::explore::KIND_FILE.to_owned();
    let file = NodeId::from_uuid(rows[0].id.as_uuid());
    let mut v = ExploreView::new();
    v.show_files();
    v.rebuild_from_entries(&rows);

    assert_eq!(v.rows[0].file_state, FileState::Present);
    assert!(!v.is_remote_file(file));
}

// ------------------------------------------------------------ the Agents mode
//
// The catalog projection (Q-E4): the device's sessions grouped by project,
// visibility by workspace scope (ADR-264's rule, leanly read), the virtual
// Unassigned bucket last, and a selection that reports `OpenSession` for the
// container to open or focus a tab — never to launch anything.

use koru_session_protocol::{
    AttentionState, BuiltInProfileKey, CapabilitySet, CatalogProfileId, CatalogWorkspaceId,
    ControllerState, ProcessState, SessionMode, SessionProfileKey, WorkspaceScope,
};

fn a_project(name: &str, scope: Option<WorkspaceScope>) -> ProjectSummary {
    ProjectSummary {
        id: ProjectId::mint(),
        name: name.to_owned(),
        root_directory: None,
        workspace_scope: scope,
        created_at_ms: 1,
    }
}

fn a_session(name: &str, project: Option<ProjectId>, created_at_ms: i64) -> SessionSummary {
    SessionSummary {
        id: SessionId::mint(),
        parent_session_id: None,
        profile: SessionProfileKey::BuiltIn("bash".parse::<BuiltInProfileKey>().unwrap()),
        display_name: name.to_owned(),
        working_directory: None,
        mode: SessionMode::RawPty,
        process: ProcessState::Running,
        controller: ControllerState::Detached,
        managed: None,
        active_turn_id: None,
        attention: AttentionState::default(),
        pending_attachment_count: 0,
        capabilities: CapabilitySet::default(),
        project_id: project,
        handover: Default::default(),
        model: None,
        origin: Default::default(),
        created_at_ms,
    }
}

/// What a run is called: the two words derived from its id (ADR-360). The
/// tests spell it through the derivation rather than pasting a literal, so
/// the assertion is *the navigator draws the run's name* and not *the
/// navigator draws `iron-umbrella`*.
fn run_name(run: koru_delegate_core::RunId) -> String {
    koru_delegate_core::RunName::of(run).to_string()
}

/// A pane in Agents mode whose window is a known `(profile, workspace)`, so a
/// scope can be written to match it or to miss it.
fn agents_pane() -> (ExploreView, WorkspaceScope) {
    let profile = CatalogProfileId::mint();
    let workspace = CatalogWorkspaceId::mint();
    let mut v = ExploreView::new();
    let mut here = a_profile("koru");
    here.id = ProfileId::from_uuid(profile.as_uuid());
    let entry = WorkspaceEntry::new(WorkspaceId::from_uuid(workspace.as_uuid()), "Example", 0);
    let current = entry.id;
    v.set_context(vec![here.clone()], here.id, vec![entry], current);
    v.show_agents();
    (v, WorkspaceScope { profile, workspace })
}

fn agent_labels(v: &ExploreView) -> Vec<(&str, u16)> {
    v.rows.iter().map(|r| (r.label.as_str(), r.depth)).collect()
}

/// The projection: the *Projects* heading first, then this workspace's
/// projects (and the unscoped ones) with their sessions beneath them, the
/// bucket last, and a project scoped to another workspace hidden **with its
/// sessions** — placed work never migrates into Unassigned because its shelf
/// is off screen.
#[test]
fn agents_mode_projects_the_catalog_to_rows() {
    let (mut v, here) = agents_pane();
    let elsewhere = WorkspaceScope {
        profile: here.profile,
        workspace: CatalogWorkspaceId::mint(),
    };
    let alpha = a_project("Alpha", Some(here));
    let beta = a_project("beta", None);
    let zed = a_project("Zed", Some(elsewhere));
    let sessions = vec![
        a_session("hidden with its shelf", Some(zed.id), 1),
        a_session("stray", Some(ProjectId::mint()), 3),
        a_session("one", Some(alpha.id), 1),
        a_session("loose", None, 2),
    ];
    v.set_agent_projects(vec![alpha, beta, zed]);
    assert!(v.set_agent_sessions(sessions));
    assert_eq!(
        agent_labels(&v),
        vec![
            ("Projects", 0),
            ("Alpha", 0),
            ("one", 1),
            // Case-folded name order, so "beta" sorts beside "Alpha" rather
            // than after "Zed".
            ("beta", 0),
            // The bucket is last, holding the unplaced and the stray — never
            // the hidden project's session.
            ("Unassigned", 0),
            ("loose", 1),
            ("stray", 1),
        ]
    );
}

/// An empty catalog still shows the heading (with its `+`, the way in) over
/// the bucket — Koru Terminal's rule: the bucket is drawn when it holds
/// something or when the tree would otherwise be blank.
#[test]
fn an_empty_agents_catalog_shows_the_heading_and_the_bucket() {
    let (mut v, _here) = agents_pane();
    v.set_agent_projects(Vec::new());
    v.rebuild_from_entries(&[]);
    assert_eq!(
        agent_labels(&v),
        vec![("Projects", 0), ("Unassigned", 0)],
        "a blank pane with nothing under its heading is worse"
    );
}

/// Selecting a session row reports `OpenSession` with the session's own id —
/// on release (the press seeds the latent drag) and on Enter — and a project
/// row folds rather than opening.
#[test]
fn selecting_a_session_reports_open_session() {
    let (mut v, here) = agents_pane();
    let project = a_project("Alpha", Some(here));
    let session = a_session("one", Some(project.id), 1);
    let id = session.id;
    v.set_agent_projects(vec![project]);
    v.set_agent_sessions(vec![session]);

    let a = area();
    // Rows: [Projects, Alpha, one].
    let (_, r) = v.row_rects(a)[2];
    assert_eq!(
        v.mouse_down(a, VIEW, on_label(r), r.centre().1, false),
        Outcome::Consumed,
        "the press seeds a latent drag; the release chooses"
    );
    assert_eq!(v.mouse_up(), Outcome::OpenSession(id));

    // Enter on the focused row answers the same thing.
    assert!(v.focus_tree(a));
    v.focus = Some(v.rows[2].node);
    assert_eq!(
        v.nav_key(a, &Key::Named(NamedKey::Enter)),
        Some(Outcome::OpenSession(id))
    );

    // A plain press on the project row folds it in place — its verbs are the
    // right-click menu's, so there is nothing else the press could mean.
    let (_, r) = v.row_rects(a)[1];
    assert_eq!(
        v.mouse_down(a, VIEW, on_label(r), r.centre().1, false),
        Outcome::Consumed
    );
    // The container rebuilds on any explore press; this is that rebuild.
    v.rebuild_from_entries(&[]);
    assert_eq!(
        agent_labels(&v),
        vec![("Projects", 0), ("Alpha", 0)],
        "folded shut"
    );
}

/// The Agents folds live in their own set: folding a project survives a round
/// trip through Documents mode, and the manifest prune cannot forget it.
#[test]
fn agents_folds_survive_a_mode_round_trip() {
    let (mut v, here) = agents_pane();
    let project = a_project("Alpha", Some(here));
    let group = NodeId::from_uuid(project.id.as_uuid());
    v.set_agent_projects(vec![project.clone()]);
    v.set_agent_sessions(vec![a_session("one", Some(project.id), 1)]);

    let a = area();
    let (_, r) = v.row_rects(a)[1];
    v.mouse_down(a, VIEW, on_label(r), r.centre().1, false);

    // Documents mode rebuilds from a manifest that knows nothing of the
    // project id — the prune that used to be the leak.
    v.show_documents();
    v.rebuild_from_entries(&entries(&[("Doc", None)]));
    v.show_agents();
    assert_eq!(
        agent_labels(&v),
        vec![("Projects", 0), ("Alpha", 0)],
        "still folded"
    );
    assert!(v.agent_collapsed.contains(&group));
}

/// The heading's `+` is the create affordance: a press in its reserved
/// trailing lane reports `NewAgentProject`, and a press on the label is
/// furniture — consumed, focusing nothing.
#[test]
fn the_projects_heading_carries_the_create_affordance() {
    let (mut v, _here) = agents_pane();
    v.rebuild_from_entries(&[]);

    let a = area();
    let (_, r) = v.row_rects(a)[0];
    let parts = v.row_parts(&v.rows[0].clone(), r);
    assert_eq!(
        v.mouse_down(a, VIEW, parts.dots.centre().0, r.centre().1, false),
        Outcome::NewAgentProject
    );
    assert_eq!(
        v.mouse_down(a, VIEW, on_label(r), r.centre().1, false),
        Outcome::Consumed,
        "the label is furniture"
    );
    assert_eq!(v.focused(), None, "a heading never takes the keyboard");
}

/// The right-click menus, Koru Terminal's shapes: a project row raises its
/// menu, the bucket and empty tree space raise the launch-only menu, and a
/// session row's menu is not offered here.
#[test]
fn agents_right_click_raises_the_tree_menus() {
    let (mut v, here) = agents_pane();
    let project = a_project("Alpha", Some(here));
    let id = project.id;
    let one = a_session("one", Some(id), 1);
    let session = one.id;
    v.set_agent_projects(vec![project]);
    v.set_agent_sessions(vec![one, a_session("loose", None, 2)]);

    let a = area();
    // Rows: [Projects, Alpha, one, Unassigned, loose].
    let row_y: Vec<f32> = (0..6).map(|i| row_centre(&v, a, i)).collect();
    assert_eq!(
        v.context_menu(a, on_label(a), row_y[1]),
        Outcome::AgentTreeMenu {
            project: Some(id),
            at: (on_label(a), row_y[1]),
        },
        "a project row raises its own menu"
    );
    assert_eq!(
        v.context_menu(a, on_label(a), row_y[3]),
        Outcome::AgentTreeMenu {
            project: None,
            at: (on_label(a), row_y[3]),
        },
        "the bucket offers only New session — it is not a project"
    );
    assert_eq!(
        v.context_menu(a, on_label(a), row_y[2]),
        Outcome::AgentSessionMenu {
            session,
            at: (on_label(a), row_y[2]),
        },
        "a session row raises its own menu (Q-E7 P8)"
    );
    let below = row_y[5] + theme::NAV_ROW_H;
    assert_eq!(
        v.context_menu(a, on_label(a), below),
        Outcome::AgentTreeMenu {
            project: None,
            at: (on_label(a), below),
        },
        "empty tree space launches into Unassigned"
    );
}

/// P6: a session row wears its product's mark — the same `for_profile`
/// projection the tab draws — not the generic terminal glyph. A built-in with
/// a vector logo answers `Brand`; the platform shell answers a `Glyph` (the
/// OS logo), and neither is the `TERMINAL` fallback.
#[test]
fn a_session_row_wears_its_product_mark() {
    let (mut v, here) = agents_pane();
    let project = a_project("Alpha", Some(here));
    let bash = a_session("bash one", Some(project.id), 1);
    let mut shell = a_session("system", Some(project.id), 2);
    shell.profile =
        SessionProfileKey::BuiltIn("platform-shell".parse::<BuiltInProfileKey>().unwrap());
    let (bash_id, shell_id) = (bash.id, shell.id);
    v.set_agent_projects(vec![project]);
    v.set_agent_sessions(vec![bash, shell]);

    assert_eq!(
        v.agent_session_mark(NodeId::from_uuid(bash_id.as_uuid())),
        Some(koru_draw::brand::ProfileMark::Brand(
            &koru_draw::brand::BASH
        )),
        "bash wears the GNU Bash mark, not the terminal glyph"
    );
    assert!(
        matches!(
            v.agent_session_mark(NodeId::from_uuid(shell_id.as_uuid())),
            Some(koru_draw::brand::ProfileMark::Glyph(_))
        ),
        "the system shell wears the OS glyph"
    );
}

/// **A run that is working shows movement, not a folder.**
///
/// Six runs under *Runs*, every one of them wearing the folder glyph, said
/// nothing about which of them was still going. A working run turns an open
/// ring instead; a finished, failed or cancelled one keeps the folder. The
/// answers come from one predicate — `koru_runs_ui::motion` — whose
/// *unfinished* half is also what the run index polls by, so a ring can never
/// turn on a list that nothing is refreshing.
#[test]
fn a_working_run_turns_a_ring_and_a_finished_one_keeps_the_folder() {
    let (mut v, _here) = agents_pane();
    let working = koru_delegate_core::RunId::from_uuid(uuid::Uuid::from_u128(0x51));
    let done = koru_delegate_core::RunId::from_uuid(uuid::Uuid::from_u128(0x52));
    let row = |run, motion| koru_runs_ui::RunRow {
        run,
        name: run_name(run),
        objective: "Write the quarterly summary".into(),
        status: "Working".into(),
        needs_attention: false,
        session: None,
        created_at_ms: 10,
        motion,
        results: 0,
    };
    use koru_runs_ui::Motion;
    assert!(v.set_agent_runs(vec![
        row(working, Motion::Working),
        row(done, Motion::Still)
    ]));

    let node = |run: koru_delegate_core::RunId| NodeId::from_uuid(run.as_uuid());
    assert!(
        v.agent_runs_working.contains(&node(working)),
        "the working run turns"
    );
    assert!(
        !v.agent_runs_working.contains(&node(done)),
        "the finished one keeps the folder"
    );
    assert_ne!(
        koru_draw::icons::CIRCLE_NOTCH,
        koru_draw::icons::FOLDER,
        "the two states are two glyphs"
    );

    // While one is working the pane asks for frames, and the ring goes round
    // once per progress loop from one anchor shared by every row.
    assert!(v.animating(), "something on this list is moving");
    let started = v.agent_runs_spin_started.expect("the ring's clock");
    assert_eq!(v.runs_spin_angle(started), 0.0);
    let half = started + std::time::Duration::from_millis(600);
    assert!((v.runs_spin_angle(half) - std::f64::consts::PI).abs() < 1e-4);
    let round = started + std::time::Duration::from_millis(1200);
    assert_eq!(v.runs_spin_angle(round), 0.0, "and it comes back round");

    // The anchor survives the three-second listing arriving again, or the
    // rings would jump back to noon every poll.
    assert!(!v.set_agent_runs(vec![
        row(working, Motion::Working),
        row(done, Motion::Still)
    ]));
    assert_eq!(v.agent_runs_spin_started, Some(started));

    // **And it stops on its own.** Once every run has finished the pane asks
    // for no more frames — standing requirement 3, and the same point at
    // which the run index stops polling.
    assert!(v.set_agent_runs(vec![row(working, Motion::Still), row(done, Motion::Still)]));
    assert!(v.agent_runs_working.is_empty());
    assert_eq!(v.agent_runs_spin_started, None);
    assert!(!v.animating(), "a finished list is an idle surface");
    assert_eq!(v.runs_spin_angle(std::time::Instant::now()), 0.0);
}

/// **An interrupted run does not spin.**
///
/// The turning ring's rule was *every non-terminal state turns*, which swept
/// in `Interrupted` — a run whose process is gone after a restart, sitting
/// still until somebody presses Resume. A ring going round beside it said
/// work was happening when nothing was (the maintainers found this in acceptance).
///
/// It wears a **pause** mark instead, and — the part that is easy to get
/// wrong — it contributes no frames: the mark does not move, so a list whose
/// only unfinished run is interrupted is an idle surface. The index goes on
/// watching it, which is a different question and is asserted in
/// `koru_runs_ui`.
#[test]
fn an_interrupted_run_pauses_rather_than_turning_and_asks_for_no_frames() {
    let (mut v, _here) = agents_pane();
    let stopped = koru_delegate_core::RunId::from_uuid(uuid::Uuid::from_u128(0x53));
    assert!(v.set_agent_runs(vec![koru_runs_ui::RunRow {
        run: stopped,
        name: run_name(stopped),
        objective: "Write the quarterly summary".into(),
        status: "Interrupted".into(),
        needs_attention: false,
        session: None,
        created_at_ms: 10,
        motion: koru_runs_ui::Motion::Paused,
        results: 0,
    }]));

    let node = NodeId::from_uuid(stopped.as_uuid());
    assert!(
        v.agent_runs_paused.contains(&node),
        "an interrupted run is paused"
    );
    assert!(
        !v.agent_runs_working.contains(&node),
        "and it is not working"
    );
    assert!(
        v.agent_runs_spin_started.is_none(),
        "nothing on this list turns"
    );
    assert!(!v.animating(), "a paused mark is still, so no frames");

    // Three states, three glyphs — the pause is not the ring and not the
    // folder, or the fix would be invisible.
    for (a, b) in [
        (koru_draw::icons::PAUSE, koru_draw::icons::CIRCLE_NOTCH),
        (koru_draw::icons::PAUSE, koru_draw::icons::FOLDER),
    ] {
        assert_ne!(a, b);
    }
}

/// ...and a navigator that is not showing runs does no timer work for them,
/// however many of them are still going.
#[test]
fn a_navigator_showing_documents_does_not_turn_for_a_working_run() {
    let (mut v, _here) = agents_pane();
    let run = koru_delegate_core::RunId::from_uuid(uuid::Uuid::from_u128(0x53));
    assert!(v.set_agent_runs(vec![koru_runs_ui::RunRow {
        run,
        name: run_name(run),
        objective: "Write the quarterly summary".into(),
        status: "Working".into(),
        needs_attention: false,
        session: None,
        created_at_ms: 10,
        motion: koru_runs_ui::Motion::Working,
        results: 0,
    }]));
    assert!(v.animating());

    v.show_documents();
    assert!(
        !v.animating(),
        "a projection with no runs on it draws no rings"
    );
}

/// P7: the mode switcher's Agents button is a robot, distinct from the terminal
/// glyph the per-row marks fall back to.
#[test]
fn the_agents_mode_button_is_a_robot() {
    assert_eq!(
        ExploreView::navigator_mode_icon(NavigatorMode::Agents),
        koru_draw::icons::ROBOT
    );
    assert_ne!(koru_draw::icons::ROBOT, koru_draw::icons::TERMINAL);
}

/// Dragging a session between projects: past the arm threshold the release on
/// a project group reports the move; on the bucket it reports `None` (the one
/// road back to Unassigned); on its own group it is a no-op.
#[test]
fn dragging_a_session_onto_a_project_reports_the_move() {
    let (mut v, here) = agents_pane();
    let alpha = a_project("Alpha", Some(here));
    let beta = a_project("Beta", Some(here));
    let (alpha_id, beta_id) = (alpha.id, beta.id);
    let one = a_session("one", Some(alpha_id), 1);
    let session = one.id;
    v.set_agent_projects(vec![alpha, beta]);
    v.set_agent_sessions(vec![one, a_session("loose", None, 2)]);

    let a = area();
    // Rows: [Projects, Alpha, one, Beta, Unassigned, loose].
    let row_y: Vec<f32> = (0..6).map(|i| row_centre(&v, a, i)).collect();
    let grab_y = row_y[2];
    let drag_to = |v: &mut ExploreView, to: f32| {
        let (_, r) = v.row_rects(a)[2];
        v.mouse_down(a, VIEW, on_label(r), grab_y, false);
        v.mouse_move(a, VIEW, on_label(r), to);
        v.mouse_up()
    };

    assert_eq!(
        drag_to(&mut v, row_y[3]),
        Outcome::MoveSessionToProject {
            session,
            project: Some(beta_id),
        },
        "dropped on the Beta group row"
    );
    assert_eq!(
        drag_to(&mut v, row_y[5]),
        Outcome::MoveSessionToProject {
            session,
            project: None,
        },
        "dropped inside the bucket: assigned to no project"
    );
    assert_eq!(
        drag_to(&mut v, row_y[1]),
        Outcome::Consumed,
        "dropped where it already is: a no-op, not a verb"
    );
    assert_eq!(
        drag_to(&mut v, row_y[0]),
        Outcome::Consumed,
        "the heading is not a destination"
    );
    // The cache was never reordered: the catalog's collation stands.
    assert_eq!(
        agent_labels(&v),
        vec![
            ("Projects", 0),
            ("Alpha", 0),
            ("one", 1),
            ("Beta", 0),
            ("Unassigned", 0),
            ("loose", 1),
        ]
    );
}

/// The pane's local answers the container's menu reads: which project a row
/// names, its current name and folder, whether it still has running sessions,
/// and the scope a create carries — the window's own `(profile, workspace)`.
#[test]
fn agents_catalog_helpers_answer_the_menu_and_the_create() {
    let (mut v, here) = agents_pane();
    let mut project = a_project("Alpha", Some(here));
    project.root_directory = Some("C:/work/alpha".to_owned());
    let id = project.id;
    let group = NodeId::from_uuid(id.as_uuid());
    v.set_agent_projects(vec![project]);
    v.set_agent_sessions(vec![a_session("one", Some(id), 1)]);

    assert_eq!(v.agent_project_at(group), Some(id));
    assert_eq!(
        v.agent_project_at(NodeId::from_uuid(uuid::Uuid::nil())),
        None
    );
    assert_eq!(v.agent_project_name(id), Some("Alpha"));
    assert_eq!(v.agent_project_root(id), Some("C:/work/alpha"));
    assert!(v.agent_project_has_running(id), "one is Running");

    let mut ended = a_session("one", Some(id), 1);
    ended.process = ProcessState::Exited { code: Some(0) };
    v.set_agent_sessions(vec![ended]);
    assert!(
        !v.agent_project_has_running(id),
        "an exited session no longer refuses the delete"
    );

    assert_eq!(
        v.agent_creation_scope(),
        Some(here),
        "a created project is scoped to the window's own selection"
    );
}

// ------------------------------------------------------------ a file's note

/// The note link is a manifest fact, so it rides the entry rather than a side
/// table: a rebuild from entries is all a row needs to carry it.
#[test]
fn a_file_with_a_note_carries_it_on_the_row() {
    let mut rows = entries(&[("report.pdf", None)]);
    rows[0].kind = koru_core::explore::KIND_FILE.to_owned();
    let note = DocumentId::new();
    rows[0].note = Some(note);
    let mut v = ExploreView::new();
    v.show_files();
    v.rebuild_from_entries(&rows);

    assert_eq!(v.rows[0].note, Some(NodeId::from_uuid(note.as_uuid())));
    let a = area();
    let (_, r) = v.row_rects(a)[0];
    let parts = v.row_parts(&v.rows[0].clone(), r);
    assert!(parts.note.w > 0.0, "a note earns its lane");
    assert!(
        parts.note.x + parts.note.w <= parts.dots.x + f32::EPSILON,
        "the note lane sits left of the `…` lane"
    );
}

/// A noteless file has no lane at all — zero-sized, so it answers for nothing,
/// and its label keeps the width.
#[test]
fn a_file_without_a_note_has_no_note_lane() {
    let mut rows = entries(&[("report.pdf", None)]);
    rows[0].kind = koru_core::explore::KIND_FILE.to_owned();
    let mut v = ExploreView::new();
    v.show_files();
    v.rebuild_from_entries(&rows);

    let a = area();
    let (_, r) = v.row_rects(a)[0];
    let parts = v.row_parts(&v.rows[0].clone(), r);
    assert_eq!(parts.note.w, 0.0);
    assert!(
        !parts
            .note
            .contains(parts.dots.centre().0, parts.dots.centre().1),
        "an absent note must not shadow the `…` beside it"
    );
}

/// Pressing the glyph opens the note — a document id in a row's clothing, so
/// the container routes it like any open. It is answered on the press, before
/// a drag could arm: the icon is never how a file starts moving. And it is its
/// own outcome, because in this projection `OpenNode` already means *Save as*
/// for the row's own file.
#[test]
fn clicking_the_note_glyph_opens_the_note() {
    let mut rows = entries(&[("report.pdf", None)]);
    rows[0].kind = koru_core::explore::KIND_FILE.to_owned();
    let note = DocumentId::new();
    rows[0].note = Some(note);
    let note_node = NodeId::from_uuid(note.as_uuid());
    let mut v = ExploreView::new();
    v.show_files();
    v.rebuild_from_entries(&rows);
    let a = area();

    let (_, r) = v.row_rects(a)[0];
    let parts = v.row_parts(&v.rows[0].clone(), r);
    let (x, _) = parts.note.centre();
    assert_eq!(
        v.mouse_down(a, VIEW, x, row_centre(&v, a, 0), false),
        Outcome::OpenNote(note_node)
    );
    assert!(!v.drag_active(), "the icon press never seeds a drag");

    // Beside it, the row keeps today's behaviour: a plain click on a present
    // file selects, and opens nothing.
    v.mouse_down(a, VIEW, on_label(a), row_centre(&v, a, 0), false);
    assert_eq!(v.mouse_up(), Outcome::Consumed);
}

/// A note is listed for search and the store, but it is not a row in either
/// projection — the pane lists pages and files, and a note is neither.
#[test]
fn a_note_earns_no_row_in_either_projection() {
    let mut rows = entries(&[
        ("Page", None),
        ("report.pdf", None),
        ("report.pdf", Some(1)),
    ]);
    rows[1].kind = koru_core::explore::KIND_FILE.to_owned();
    rows[2].kind = koru_core::explore::KIND_NOTE.to_owned();
    let mut v = ExploreView::new();
    v.rebuild_from_entries(&rows);
    assert_eq!(labels(&v), ["Page"]);

    v.show_files();
    assert_eq!(labels(&v), ["report.pdf"]);
}

/// **A run is its own group, and choosing one asks for its views** (P1.16).
///
/// A run outlives every session it used and exists on devices that never ran
/// one, so it sits under its own heading rather than among the live
/// processes — and pressing it opens the run, never starts one.
#[test]
fn a_run_row_reports_open_run_and_lives_under_its_own_heading() {
    let (mut v, here) = agents_pane();
    let project = a_project("Alpha", Some(here));
    let session = a_session("one", Some(project.id), 1);
    v.set_agent_projects(vec![project]);
    v.set_agent_sessions(vec![session]);

    let run = koru_delegate_core::RunId::from_uuid(uuid::Uuid::from_u128(0x51));
    assert!(v.set_agent_runs(vec![koru_runs_ui::RunRow {
        run,
        name: run_name(run),
        objective: "Write the quarterly summary".into(),
        status: "Working".into(),
        needs_attention: false,
        session: None,
        created_at_ms: 10,
        motion: koru_runs_ui::Motion::Working,
        results: 0,
    }]));
    assert_eq!(
        agent_labels(&v),
        vec![
            ("Projects", 0),
            ("Alpha", 0),
            ("one", 1),
            ("Runs", 0),
            (run_name(run).as_str(), 1)
        ],
        "the runs are their own group, below the sessions"
    );

    let a = area();
    let (_, r) = v.row_rects(a)[4];
    // **On the release, as a session row is.** The press seeds a latent
    // drag: a run can be dragged, to exactly one place — the pinned Trash
    // row (P7a) — and a press that opened the run immediately would open it
    // on the way to binning it.
    assert_eq!(
        v.mouse_down(a, VIEW, on_label(r), r.centre().1, false),
        Outcome::Consumed
    );
    assert_eq!(v.mouse_up(), Outcome::OpenRun(run));
    assert_eq!(v.agent_run(v.rows[4].node), Some(run));

    // Enter on the focused row answers the same thing.
    assert!(v.focus_tree(a));
    v.focus = Some(v.rows[4].node);
    assert_eq!(
        v.nav_key(a, &Key::Named(NamedKey::Enter)),
        Some(Outcome::OpenRun(run))
    );

    // An unchanged listing rebuilds nothing.
    assert!(!v.set_agent_runs(vec![koru_runs_ui::RunRow {
        run,
        name: run_name(run),
        objective: "Write the quarterly summary".into(),
        status: "Working".into(),
        needs_attention: false,
        session: None,
        created_at_ms: 10,
        motion: koru_runs_ui::Motion::Working,
        results: 0,
    }]));

    // And with no runs the heading is gone: an empty group beside real
    // content is noise.
    assert!(v.set_agent_runs(Vec::new()));
    assert_eq!(
        agent_labels(&v),
        vec![("Projects", 0), ("Alpha", 0), ("one", 1)]
    );
}
/// **A run's session sits under its run, and an unrelated one stays in the
/// bucket** (ADR-361).
///
/// A delegated run works in a temporary folder, so it has no project and its
/// session landed in *Unassigned* — three rows reading *Codex (Embedded)*
/// beside three runs, with nothing joining the two lists. The listing now
/// says which session each run is being driven in, and the tree draws it
/// where it belongs. A session no run in the listing claims is untouched:
/// the listing is one device's answer about one workspace, and a session it
/// cannot account for is not a session to hide.
#[test]
fn a_runs_session_is_drawn_under_the_run_and_leaves_the_others_alone() {
    let (mut v, here) = agents_pane();
    let project = a_project("Alpha", Some(here));
    let mine = a_session("Codex (Embedded)", None, 1);
    let other = a_session("Codex (Embedded)", None, 2);
    let inside = a_session("bash", Some(project.id), 3);
    let (mine_id, other_id) = (mine.id, other.id);
    v.set_agent_projects(vec![project]);
    v.set_agent_sessions(vec![mine, other, inside]);

    let run = koru_delegate_core::RunId::from_uuid(uuid::Uuid::from_u128(0x51));
    assert!(v.set_agent_runs(vec![koru_runs_ui::RunRow {
        run,
        name: run_name(run),
        objective: "Write the quarterly summary".into(),
        status: "Working".into(),
        needs_attention: false,
        session: Some(mine_id),
        created_at_ms: 10,
        motion: koru_runs_ui::Motion::Working,
        results: 0,
    }]));

    assert_eq!(
        agent_labels(&v),
        vec![
            ("Projects", 0),
            ("Alpha", 0),
            ("bash", 1),
            ("Unassigned", 0),
            ("Codex (Embedded)", 1),
            ("Runs", 0),
            (run_name(run).as_str(), 1),
            ("Codex (Embedded)", 2),
        ],
        "the run's session hangs under the run; the other one is still Unassigned"
    );

    // The bucket keeps the session no run claimed, and the nested row is the
    // one the run named.
    let nested = v.rows[7].node;
    assert_eq!(v.agent_session_id(nested), Some(mine_id));
    let bucket = v.rows[4].node;
    assert_eq!(v.agent_session_id(bucket), Some(other_id));

    // Pressing the nested row asks for that session, exactly as a row in a
    // project does — the tree says which session, never how it is opened.
    let a = area();
    let (_, r) = v.row_rects(a)[7];
    assert_eq!(
        v.mouse_down(a, VIEW, on_label(r), r.centre().1, false),
        Outcome::Consumed,
        "a session row seeds a latent drag on the press"
    );
    assert_eq!(v.mouse_up(), Outcome::OpenSession(mine_id));

    // The run folds like the group it has become, and its session goes with
    // it. Pressing the caret, not the row: the row itself still opens the
    // run.
    let (_, run_rect) = v.row_rects(a)[6];
    let row = v.rows[6].clone();
    let (cx, cy) = v.caret_rect(&row, run_rect).centre();
    assert_eq!(v.mouse_down(a, VIEW, cx, cy, false), Outcome::Consumed);
    v.show_documents();
    v.show_agents();
    assert_eq!(
        agent_labels(&v).last(),
        Some(&(run_name(run).as_str(), 1)),
        "the folded run keeps its own row and loses its child"
    );
    assert!(v.agent_collapsed.contains(&row.node));
    // Open it again, so the last assertion is about the listing rather than
    // about a fold left behind.
    let (_, run_rect) = v.row_rects(a)[6];
    let (cx, cy) = v.caret_rect(&v.rows[6].clone(), run_rect).centre();
    assert_eq!(v.mouse_down(a, VIEW, cx, cy, false), Outcome::Consumed);

    // A run this device is not driving has no child at all.
    assert!(v.set_agent_runs(vec![koru_runs_ui::RunRow {
        run,
        name: run_name(run),
        objective: "Write the quarterly summary".into(),
        status: "Finished".into(),
        needs_attention: false,
        session: None,
        created_at_ms: 10,
        motion: koru_runs_ui::Motion::Working,
        results: 0,
    }]));
    assert_eq!(
        agent_labels(&v),
        vec![
            ("Projects", 0),
            ("Alpha", 0),
            ("bash", 1),
            ("Unassigned", 0),
            ("Codex (Embedded)", 1),
            ("Codex (Embedded)", 1),
            ("Runs", 0),
            (run_name(run).as_str(), 1),
        ],
        "the session the run has let go of is back in the bucket"
    );
}

/// **An empty bucket does not survive the runs taking their sessions.**
///
/// The maintainers' own case: three delegated runs, three sessions, no projects.
/// Every session now hangs under its run, and *Unassigned* is drawn only
/// when it holds something or when the tree would otherwise be blank — with
/// a *Runs* group below it, it is neither.
#[test]
fn the_bucket_goes_when_every_session_it_held_is_under_a_run() {
    let (mut v, _here) = agents_pane();
    let sessions: Vec<_> = (0..3)
        .map(|i| a_session("Codex (Embedded)", None, i))
        .collect();
    let ids: Vec<_> = sessions.iter().map(|s| s.id).collect();
    v.set_agent_projects(Vec::new());
    v.set_agent_sessions(sessions);
    assert_eq!(
        agent_labels(&v),
        vec![
            ("Projects", 0),
            ("Unassigned", 0),
            ("Codex (Embedded)", 1),
            ("Codex (Embedded)", 1),
            ("Codex (Embedded)", 1),
        ],
        "before the runs are known, the bucket is where they are"
    );

    let runs: Vec<_> = ids
        .iter()
        .enumerate()
        .map(|(i, session)| {
            let run = koru_delegate_core::RunId::from_uuid(uuid::Uuid::from_u128(0x60 + i as u128));
            koru_runs_ui::RunRow {
                run,
                name: run_name(run),
                objective: "Research something".into(),
                status: "Working".into(),
                needs_attention: false,
                session: Some(*session),
                created_at_ms: 10 - i as i64,
                motion: koru_runs_ui::Motion::Working,
                results: 0,
            }
        })
        .collect();
    let names: Vec<String> = runs.iter().map(|row| row.name.clone()).collect();
    assert!(v.set_agent_runs(runs));

    let labels = agent_labels(&v);
    assert!(
        !labels.iter().any(|(label, _)| *label == "Unassigned"),
        "nothing is unassigned any more: {labels:?}"
    );
    assert_eq!(
        labels,
        vec![
            ("Projects", 0),
            ("Runs", 0),
            (names[0].as_str(), 1),
            ("Codex (Embedded)", 2),
            (names[1].as_str(), 1),
            ("Codex (Embedded)", 2),
            (names[2].as_str(), 1),
            ("Codex (Embedded)", 2),
        ]
    );
}

// ------------------------------------------------------- the artifacts mode

/// The fifth mode is reached the way the other four are, and the column
/// below the heading becomes the Artifacts surface's — not the library's.
#[test]
fn the_artifacts_mode_is_reached_by_its_icon_and_owns_the_column() {
    let mut view = ExploreView::new();
    let a = area();
    let (_, artifacts) = view
        .navigator_mode_rects(view.rail_of(a))
        .into_iter()
        .find(|(mode, _)| *mode == NavigatorMode::Artifacts)
        .unwrap();
    let (x, y) = artifacts.centre();
    assert_eq!(
        view.apps_rail_mouse_down(view.rail_of(a), x, y),
        Outcome::Consumed
    );
    assert_eq!(view.navigator_mode(), NavigatorMode::Artifacts);
    assert!(view.showing_artifacts());
    assert!(view.rows.is_empty(), "the surface is not the library");
    assert_eq!(NavigatorMode::Artifacts.token(), "artifacts");
    assert_eq!(
        NavigatorMode::parse("artifacts"),
        Some(NavigatorMode::Artifacts)
    );
    assert_eq!(
        NavigatorMode::Artifacts.shortcut(),
        Some(koru_keys::ShortcutId::ShowArtifacts)
    );
}

/// **A press below the heading belongs to the surface, and is never
/// reported.** Unlike the tasks rail it performs what it offers, so the
/// container routes nothing — and the press is still swallowed rather than
/// falling through to the view beside the column.
#[test]
fn a_press_in_the_artifacts_column_is_the_surfaces_and_reports_nothing() {
    let mut view = ExploreView::new();
    view.show_artifacts();
    let a = area();
    let y = view.rows_top(a) + theme::NAV_ROW_H * 0.5;
    assert_eq!(
        view.mouse_down(a, (1200.0, 800.0), a.x + 40.0, y, false),
        Outcome::Consumed
    );
    // And a right-click raises no menu of nothing.
    assert_eq!(view.context_menu(a, a.x + 40.0, y), Outcome::Consumed);
}

/// The surface is built without a reader under test, so nothing in this
/// suite dials the gateway a developer has running.
#[test]
fn the_artifacts_surface_dials_nothing_under_test() {
    let mut view = ExploreView::new();
    view.show_artifacts();
    view.start_artifacts(None);
    assert!(!view.artifacts_mut().poll());
    assert_eq!(view.artifacts().load(), koru_artifacts_ui::Load::Cold);
}

/// The keyboard: the column takes it, the arrows are its, and Escape gives
/// it back — the tree's own rule, in the mode whose rows are not the tree's.
#[test]
fn the_artifacts_column_takes_the_keyboard_and_gives_it_back() {
    let mut view = ExploreView::new();
    view.show_artifacts();
    let a = area();
    assert!(!view.has_focus());
    // **A column with nothing in it swallows nothing**, arrows included:
    // there is no row to land on, so the keys reach whatever is behind.
    // (The populated walk — Down, Right, Enter, Left, Escape — is asserted
    // in `koru-artifacts-ui`'s own suite, where a catalog can be pushed in.)
    assert!(!view.focus_tree(a));
    assert!(view.nav_key(a, &Key::Named(NamedKey::ArrowDown)).is_none());
    // And a key the column never claims falls through whatever is in it.
    assert!(view.nav_key(a, &Key::Named(NamedKey::Tab)).is_none());
    assert!(view.nav_key(a, &Key::Character("a".into())).is_none());
}

/// A run row for the Agents listing, still and with no results — the shape
/// every selection test below wants, because what a run *did* is beside the
/// point of choosing it.
fn a_finished_run(run: koru_delegate_core::RunId, created_at_ms: i64) -> koru_runs_ui::RunRow {
    koru_runs_ui::RunRow {
        run,
        name: run_name(run),
        objective: "Write the quarterly summary".into(),
        status: "Finished".into(),
        needs_attention: false,
        session: None,
        created_at_ms,
        motion: koru_runs_ui::Motion::Still,
        results: 0,
    }
}

/// A pane in Agents mode listing `n` finished runs, and their ids.
fn agents_pane_with_runs(n: u128) -> (ExploreView, Vec<koru_delegate_core::RunId>) {
    let (mut v, _here) = agents_pane();
    let runs: Vec<koru_delegate_core::RunId> = (0..n)
        .map(|i| koru_delegate_core::RunId::from_uuid(uuid::Uuid::from_u128(0x900 + i)))
        .collect();
    v.set_agent_runs(
        runs.iter()
            .enumerate()
            .map(|(i, run)| a_finished_run(*run, i as i64))
            .collect(),
    );
    (v, runs)
}

/// The row index of a run in the cache, by identity — the tree's shape is a
/// listing another process writes, so a position is a guess.
fn run_row_index(v: &ExploreView, run: koru_delegate_core::RunId) -> usize {
    v.rows
        .iter()
        .position(|row| v.agent_run(row.node) == Some(run))
        .expect("the run has a row")
}

fn run_node(run: koru_delegate_core::RunId) -> NodeId {
    NodeId::from_uuid(run.as_uuid())
}

/// **Ctrl builds a run selection and opens nothing** (ADR-379, the maintainers'
/// *ctrl select*). The same machinery the library has: the press toggles, the
/// release that never armed leaves the toggle standing, and a Ctrl press must
/// not also open the run — a run row opens on release (ADR-374), so this is
/// the half that has to compose with the latent drag rather than undo it.
#[test]
fn ctrl_clicking_run_rows_builds_a_selection_without_opening_any_of_them() {
    let (mut v, runs) = agents_pane_with_runs(3);
    let a = area();
    for run in [runs[0], runs[2]] {
        let y = row_centre(&v, a, run_row_index(&v, run));
        assert_eq!(
            v.mouse_down(a, VIEW, on_label(a), y, true),
            Outcome::Consumed,
            "Ctrl does not open"
        );
        assert_eq!(v.mouse_up(), Outcome::Consumed);
    }
    assert_eq!(
        v.selection,
        HashSet::from([run_node(runs[0]), run_node(runs[2])])
    );

    // …and toggles back off, exactly as a document row does.
    let y = row_centre(&v, a, run_row_index(&v, runs[0]));
    v.mouse_down(a, VIEW, on_label(a), y, true);
    assert_eq!(v.mouse_up(), Outcome::Consumed);
    assert_eq!(v.selection, HashSet::from([run_node(runs[2])]));

    // A plain click collapses the selection back to the row and still opens
    // the run on the release — the behaviour ADR-374 put there.
    let y = row_centre(&v, a, run_row_index(&v, runs[1]));
    v.mouse_down(a, VIEW, on_label(a), y, false);
    assert_eq!(v.mouse_up(), Outcome::OpenRun(runs[1]));
    assert_eq!(v.selection, HashSet::from([run_node(runs[1])]));
}

/// **Shift extends from the anchor, in the order the rows are shown** — and
/// takes only the runs. The range spans the *Runs* heading and a session
/// sitting under one of them, and neither joins: their verbs are elsewhere,
/// and a *Delete 3 runs* menu handed a project would be a promise nothing
/// could keep.
#[test]
fn shift_clicking_extends_the_run_selection_and_takes_only_runs() {
    let (mut v, here) = agents_pane();
    let project = a_project("Alpha", Some(here));
    let attached = a_session("one", None, 1);
    let driving = attached.id;
    v.set_agent_projects(vec![project]);
    v.set_agent_sessions(vec![attached]);
    let runs: Vec<koru_delegate_core::RunId> = (0..3)
        .map(|i| koru_delegate_core::RunId::from_uuid(uuid::Uuid::from_u128(0x910 + i)))
        .collect();
    let mut rows: Vec<koru_runs_ui::RunRow> = runs
        .iter()
        .enumerate()
        .map(|(i, run)| a_finished_run(*run, i as i64))
        .collect();
    // The middle run is driving a session, so the range has a non-run row
    // inside it as well as above it.
    rows[1].session = Some(driving);
    v.set_agent_runs(rows);

    let a = area();
    let first = row_centre(&v, a, run_row_index(&v, runs[0]));
    v.mouse_down(a, VIEW, on_label(a), first, false);
    assert_eq!(v.mouse_up(), Outcome::OpenRun(runs[0]));

    let last = row_centre(&v, a, run_row_index(&v, runs[2]));
    assert_eq!(
        v.mouse_down_with_modifiers(a, VIEW, on_label(a), last, false, true),
        Outcome::Consumed,
        "Shift does not open either"
    );
    assert_eq!(v.mouse_up(), Outcome::Consumed);
    assert_eq!(
        v.selection,
        runs.iter().copied().map(run_node).collect::<HashSet<_>>(),
        "the whole range, and nothing in it that is not a run"
    );
}

/// **A press on a row that is not a run puts the selection down.** A session
/// is a process and a group is a place; leaving three runs lit while the
/// column is acting on one of those is a selection whose subject nobody can
/// see — and it is what would otherwise let `arm_drag` carry three runs off
/// behind a session dragged between projects.
#[test]
fn pressing_a_session_or_a_group_drops_the_run_selection() {
    let (mut v, here) = agents_pane();
    let project = a_project("Alpha", Some(here));
    let one = a_session("one", Some(project.id), 1);
    let group = NodeId::from_uuid(project.id.as_uuid());
    v.set_agent_projects(vec![project]);
    v.set_agent_sessions(vec![one.clone()]);
    let run = koru_delegate_core::RunId::from_uuid(uuid::Uuid::from_u128(0x920));
    v.set_agent_runs(vec![a_finished_run(run, 1)]);

    let a = area();
    let select_the_run = |v: &mut ExploreView| {
        let y = row_centre(v, a, run_row_index(v, run));
        v.mouse_down(a, VIEW, on_label(a), y, true);
        v.mouse_up();
    };
    let index_of = |v: &ExploreView, want: NodeId| {
        v.rows
            .iter()
            .position(|row| row.node == want)
            .expect("the row is there")
    };

    select_the_run(&mut v);
    assert!(!v.selection.is_empty());
    let session_row = index_of(&v, NodeId::from_uuid(one.id.as_uuid()));
    let y = row_centre(&v, a, session_row);
    v.mouse_down(a, VIEW, on_label(a), y, false);
    v.mouse_up();
    assert!(v.selection.is_empty(), "a session is not part of the set");

    select_the_run(&mut v);
    let group_row = index_of(&v, group);
    let y = row_centre(&v, a, group_row);
    v.mouse_down(a, VIEW, on_label(a), y, false);
    v.mouse_up();
    assert!(v.selection.is_empty(), "a group is a place, not a subject");
}

/// **A right-click inside a run selection is about all of it, and one
/// outside makes that row the selection first** — `select_for_menu`'s rule,
/// which is the library's, applied to the Agents tree (ADR-379).
#[test]
fn a_right_click_reports_the_whole_run_selection() {
    let (mut v, runs) = agents_pane_with_runs(3);
    let a = area();
    for run in [runs[0], runs[1]] {
        let y = row_centre(&v, a, run_row_index(&v, run));
        v.mouse_down(a, VIEW, on_label(a), y, true);
        v.mouse_up();
    }

    let inside = row_centre(&v, a, run_row_index(&v, runs[1]));
    assert_eq!(
        v.context_menu(a, on_label(a), inside),
        Outcome::AgentRunMenu {
            runs: vec![runs[0], runs[1]],
            at: (on_label(a), inside),
        },
        "in row order, not in the order they were picked"
    );

    let outside = row_centre(&v, a, run_row_index(&v, runs[2]));
    assert_eq!(
        v.context_menu(a, on_label(a), outside),
        Outcome::AgentRunMenu {
            runs: vec![runs[2]],
            at: (on_label(a), outside),
        },
        "a menu on an unselected row is about that row alone"
    );
    assert_eq!(v.selection, HashSet::from([run_node(runs[2])]));
}

/// **Dragging one of several selected runs onto the bin takes them all.**
/// `arm_drag` lifts the set and the ghost counts it; a drop that binned only
/// the row under the pointer would be the gesture doing less than it showed.
#[test]
fn dragging_a_selected_run_onto_the_trash_row_takes_the_whole_selection() {
    let (mut v, runs) = agents_pane_with_runs(3);
    let a = area();
    for run in [runs[0], runs[2]] {
        let y = row_centre(&v, a, run_row_index(&v, run));
        v.mouse_down(a, VIEW, on_label(a), y, true);
        v.mouse_up();
    }

    let trash = v.trash_rect(a);
    let onto_trash = trash.y + trash.h * 0.5;
    let grab = row_centre(&v, a, run_row_index(&v, runs[2]));
    // A plain press inside the visible selection keeps it, exactly as it does
    // in the library.
    v.mouse_down(a, VIEW, on_label(a), grab, false);
    v.mouse_move(a, VIEW, on_label(a), onto_trash);
    assert!(v.drag_armed());
    assert_eq!(v.mouse_up(), Outcome::TrashRuns(vec![runs[0], runs[2]]));
    assert!(v.selection.is_empty(), "the set went; nothing stays lit");
}

/// **Leaving the Agents column puts the run selection down.** A set of runs
/// is not a set of documents, and a `NodeId` that survived the switch would
/// tint whatever row it happened to name.
#[test]
fn changing_mode_forgets_the_run_selection() {
    let (mut v, runs) = agents_pane_with_runs(2);
    let a = area();
    let y = row_centre(&v, a, run_row_index(&v, runs[0]));
    v.mouse_down(a, VIEW, on_label(a), y, true);
    v.mouse_up();
    assert!(!v.selection.is_empty());
    v.show_documents();
    assert!(v.selection.is_empty());
    assert!(v.selection_anchor.is_none());
}

/// **A run row raises its own menu.** Its one verb is Delete, and which run
/// it is about is what the pane reports; whether the run may be deleted, and
/// what should happen to what it produced, are the container's (P7a).
#[test]
fn a_run_row_raises_its_own_menu() {
    let (mut v, _here) = agents_pane();
    let run = koru_delegate_core::RunId::from_uuid(uuid::Uuid::from_u128(0x71));
    v.set_agent_runs(vec![koru_runs_ui::RunRow {
        run,
        name: run_name(run),
        objective: "Write the quarterly summary".into(),
        status: "Finished".into(),
        needs_attention: false,
        session: None,
        created_at_ms: 10,
        motion: koru_runs_ui::Motion::Still,
        results: 0,
    }]);

    let a = area();
    // Found by identity rather than by position: the tree's shape is the
    // catalog's, and a row index is a guess about a listing another process
    // writes.
    let index = v
        .row_rects(a)
        .into_iter()
        .map(|(i, _)| i)
        .find(|i| v.row_node(*i).and_then(|node| v.agent_run(node)) == Some(run))
        .expect("the run has a row");
    let run_row = row_centre(&v, a, index);
    assert_eq!(
        v.context_menu(a, on_label(a), run_row),
        Outcome::AgentRunMenu {
            runs: vec![run],
            at: (on_label(a), run_row),
        }
    );
    // The *Runs* heading above it is a place, not a thing: it keeps the
    // tree menu.
    let heading = row_centre(&v, a, index - 1);
    assert!(matches!(
        v.context_menu(a, on_label(a), heading),
        Outcome::AgentTreeMenu { .. }
    ));
}

/// **Dragging a run onto the Trash row is the menu's Delete.** The same
/// gesture the document tree already has, on the one row in this tree that
/// can be binned: a session is a process and a project is a registry record,
/// and each has its own verb elsewhere.
#[test]
fn dragging_a_run_onto_the_trash_row_reports_it() {
    let (mut v, here) = agents_pane();
    let run = koru_delegate_core::RunId::from_uuid(uuid::Uuid::from_u128(0x72));
    let project = a_project("Alpha", Some(here));
    let one = a_session("one", Some(project.id), 1);
    let session = one.id;
    v.set_agent_projects(vec![project]);
    v.set_agent_sessions(vec![one]);
    v.set_agent_runs(vec![koru_runs_ui::RunRow {
        run,
        name: run_name(run),
        objective: "Write the quarterly summary".into(),
        status: "Finished".into(),
        needs_attention: false,
        session: None,
        created_at_ms: 10,
        motion: koru_runs_ui::Motion::Still,
        results: 0,
    }]);

    let a = area();
    let trash = v.trash_rect(a);
    let onto_trash = trash.y + trash.h * 0.5;
    let placed: Vec<(NodeId, Rect)> = v
        .row_rects(a)
        .into_iter()
        .filter_map(|(i, r)| v.row_node(i).map(|node| (node, r)))
        .collect();
    let rect_of = |find: &dyn Fn(NodeId) -> bool| {
        placed
            .iter()
            .find(|(node, _)| find(*node))
            .map(|(_, r)| *r)
            .expect("the row is there")
    };
    let run_rect = rect_of(&|node| v.agent_run(node) == Some(run));
    v.mouse_down(a, VIEW, on_label(run_rect), run_rect.centre().1, false);
    v.mouse_move(a, VIEW, on_label(run_rect), onto_trash);
    assert_eq!(v.mouse_up(), Outcome::TrashRuns(vec![run]));

    // A session dropped there is not binned: it is a process, and the tree
    // says so by doing nothing rather than by reporting a trash of nothing.
    let session_rect = rect_of(&|node| v.agent_session_id(node) == Some(session));
    v.mouse_down(
        a,
        VIEW,
        on_label(session_rect),
        session_rect.centre().1,
        false,
    );
    v.mouse_move(a, VIEW, on_label(session_rect), onto_trash);
    assert_eq!(v.mouse_up(), Outcome::Consumed);
}

// --------------------------------------------------- what a row is wearing

/// **A picked row wears the open row's card, not the hover's tint.**
///
/// It wore the tint for a while, which meant a multi-selection looked exactly
/// like the pointer resting on a row — and then arming a drag promoted the very
/// same set to cards, so choosing rows and moving them read as two features.
/// This is the test that was missing: the treatment used to be decided inside
/// the draw pass, where nothing could ask it.
#[test]
fn a_picked_row_wears_the_same_card_as_the_open_one() {
    let (mut v, ids) = flat_rows(4);
    v.set_selected(Some(ids[0]));
    v.selection.insert(ids[2]);

    assert_eq!(v.row_treatment(&v.rows[0], 0), RowTreatment::Card, "open");
    assert_eq!(v.row_treatment(&v.rows[2], 2), RowTreatment::Card, "picked");
    assert_eq!(v.row_treatment(&v.rows[1], 1), RowTreatment::Idle);
}

/// The pointer merely being somewhere must never overwrite what you chose, and
/// a row nobody has chosen still lights under it.
#[test]
fn a_hover_lights_an_idle_row_and_never_downgrades_a_chosen_one() {
    let (mut v, ids) = flat_rows(4);
    v.selection.insert(ids[2]);
    v.hover = Some(2);
    assert_eq!(
        v.row_treatment(&v.rows[2], 2),
        RowTreatment::Card,
        "hovering a picked row does not take its card away"
    );

    v.hover = Some(1);
    assert_eq!(v.row_treatment(&v.rows[1], 1), RowTreatment::Hover);
    assert_eq!(v.row_treatment(&v.rows[3], 3), RowTreatment::Idle);
}

/// Every row of a multi-selection reads the same, whichever one of them the
/// active tab happens to be showing — a set is a set.
#[test]
fn every_row_of_a_selection_reads_the_same() {
    let (mut v, ids) = flat_rows(4);
    v.set_selected(Some(ids[1]));
    for id in &ids {
        v.selection.insert(*id);
    }
    for i in 0..4 {
        assert_eq!(
            v.row_treatment(&v.rows[i], i),
            RowTreatment::Card,
            "row {i}"
        );
    }
}

// ------------------------------------------------ scheduled work in Agents

fn a_schedule_row(
    schedule: koru_delegate_core::ScheduleId,
    runs: Vec<crate::schedule_index::ScheduleRunRow>,
) -> crate::schedule_index::ScheduleRow {
    crate::schedule_index::ScheduleRow {
        schedule,
        name: "Daily review".into(),
        paused: false,
        rule: "Every weekday at 09:00".into(),
        next: "Next: Sep 14, 09:00".into(),
        owner: "Owner: this device".into(),
        state: "Active".into(),
        runs,
        created_at_ms: 10,
    }
}

#[test]
fn paused_schedule_rows_keep_semantic_icon_state() {
    let (mut view, _here) = agents_pane();
    let active = koru_delegate_core::ScheduleId::mint();
    let paused = koru_delegate_core::ScheduleId::mint();
    let mut paused_row = a_schedule_row(paused, Vec::new());
    paused_row.paused = true;
    view.set_agent_schedules(
        vec![a_schedule_row(active, Vec::new()), paused_row],
        crate::schedule_index::CatalogState::Ready,
    );

    let active_node = view.agent_schedule_node(active).unwrap();
    let paused_node = view.agent_schedule_node(paused).unwrap();
    assert!(!view.agent_schedules_paused.contains(&active_node));
    assert!(view.agent_schedules_paused.contains(&paused_node));
}

#[test]
fn ctrl_selected_schedule_parents_report_one_homogeneous_menu_subject() {
    let (mut view, _here) = agents_pane();
    let schedules = [
        koru_delegate_core::ScheduleId::mint(),
        koru_delegate_core::ScheduleId::mint(),
        koru_delegate_core::ScheduleId::mint(),
    ];
    view.set_agent_schedules(
        schedules
            .iter()
            .copied()
            .map(|schedule| a_schedule_row(schedule, Vec::new()))
            .collect(),
        crate::schedule_index::CatalogState::Ready,
    );
    let a = area();
    for schedule in [schedules[0], schedules[2]] {
        let node = view.agent_schedule_node(schedule).unwrap();
        let index = view.rows.iter().position(|row| row.node == node).unwrap();
        let y = row_centre(&view, a, index);
        assert_eq!(
            view.mouse_down(a, VIEW, on_label(a), y, true),
            Outcome::Consumed
        );
    }

    let selected = schedules
        .iter()
        .copied()
        .filter(|schedule| *schedule != schedules[1])
        .map(|schedule| view.agent_schedule_node(schedule).unwrap())
        .collect::<HashSet<_>>();
    assert_eq!(view.selection, selected);
    let node = view.agent_schedule_node(schedules[2]).unwrap();
    let index = view.rows.iter().position(|row| row.node == node).unwrap();
    let y = row_centre(&view, a, index);
    assert_eq!(
        view.context_menu(a, on_label(a), y),
        Outcome::AgentScheduleMenu {
            schedules: vec![schedules[0], schedules[2]],
            at: (on_label(a), y),
        }
    );
}

#[test]
fn shift_selected_schedule_range_contains_only_schedule_parents() {
    let (mut view, _here) = agents_pane();
    let schedules = [
        koru_delegate_core::ScheduleId::mint(),
        koru_delegate_core::ScheduleId::mint(),
        koru_delegate_core::ScheduleId::mint(),
    ];
    view.set_agent_schedules(
        schedules
            .iter()
            .copied()
            .map(|schedule| a_schedule_row(schedule, Vec::new()))
            .collect(),
        crate::schedule_index::CatalogState::Ready,
    );
    let a = area();
    let first = view.agent_schedule_node(schedules[0]).unwrap();
    let first_index = view.rows.iter().position(|row| row.node == first).unwrap();
    assert_eq!(
        view.mouse_down(
            a,
            VIEW,
            on_label(a),
            row_centre(&view, a, first_index),
            true
        ),
        Outcome::Consumed
    );

    let last = view.agent_schedule_node(schedules[2]).unwrap();
    let last_index = view.rows.iter().position(|row| row.node == last).unwrap();
    assert_eq!(
        view.mouse_down_with_modifiers(
            a,
            VIEW,
            on_label(a),
            row_centre(&view, a, last_index),
            false,
            true,
        ),
        Outcome::Consumed
    );

    assert_eq!(
        view.selection,
        schedules
            .iter()
            .map(|schedule| view.agent_schedule_node(*schedule).unwrap())
            .collect(),
        "metadata rows never join a schedule range"
    );
}

/// The schedule is its own foldable, fixed-pitch group. A partial listing
/// remains visible and says that it is partial; hiding usable rows would turn
/// a recoverable catalog answer into a blank surface.
#[test]
fn schedules_are_a_foldable_agents_group_and_partial_rows_stay_visible() {
    let (mut view, _here) = agents_pane();
    let schedule = koru_delegate_core::ScheduleId::mint();
    let run = koru_delegate_core::RunId::from_uuid(uuid::Uuid::from_u128(0xa01));
    view.set_agent_schedules(
        vec![a_schedule_row(
            schedule,
            vec![crate::schedule_index::ScheduleRunRow {
                run,
                label: run_name(run),
            }],
        )],
        crate::schedule_index::CatalogState::Incomplete,
    );

    let labels = agent_labels(&view);
    assert!(labels.contains(&("Schedules", 0)));
    assert!(labels.contains(&("Daily review", 1)));
    assert!(labels.contains(&("Every weekday at 09:00", 2)));
    assert!(labels.contains(&("Next: Sep 14, 09:00", 2)));
    assert!(labels.contains(&("Owner: this device", 2)));
    assert!(labels.contains(&("Active", 2)));
    assert!(labels.iter().any(|(label, depth)| {
        *depth == 1
            && *label
                == koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::ScheduleNavigatorIncomplete,
                )
    }));
    assert!(
        view.rows
            .iter()
            .any(|row| { view.agent_schedule_run(row.node) == Some(run) && row.depth == 2 })
    );
    assert!(
        view.row_rects(area())
            .iter()
            .all(|(_, rect)| rect.h == theme::NAV_ROW_H),
        "schedule definitions and children keep the navigator's fixed pitch"
    );

    let schedule_index = view
        .rows
        .iter()
        .position(|row| row.label == "Daily review")
        .unwrap();
    let (_, rect) = view.row_rects(area())[schedule_index];
    let (x, y) = view
        .caret_rect(&view.rows[schedule_index].clone(), rect)
        .centre();
    assert_eq!(
        view.mouse_down(area(), VIEW, x, y, false),
        Outcome::Consumed
    );
    // The container's normal rebuild boundary applies the fold.
    view.show_documents();
    view.show_agents();
    assert!(agent_labels(&view).contains(&("Daily review", 1)));
    assert!(!agent_labels(&view).contains(&("Next: Sep 14, 09:00", 2)));
}

#[test]
fn every_schedule_catalog_state_is_named_in_the_agents_tree() {
    let cases = [
        (
            crate::schedule_index::CatalogState::Loading,
            koru_i18n::Key::ScheduleNavigatorLoading,
        ),
        (
            crate::schedule_index::CatalogState::Ready,
            koru_i18n::Key::ScheduleNavigatorEmpty,
        ),
        (
            crate::schedule_index::CatalogState::Incomplete,
            koru_i18n::Key::ScheduleNavigatorIncomplete,
        ),
        (
            crate::schedule_index::CatalogState::Error,
            koru_i18n::Key::ScheduleNavigatorError,
        ),
    ];
    for (state, key) in cases {
        let (mut view, _here) = agents_pane();
        view.set_agent_schedules(Vec::new(), state);
        let expected = koru_i18n::t(crate::format::lang(), key);
        assert!(
            agent_labels(&view).contains(&(expected, 1)),
            "{state:?} must not look like another catalog state"
        );
    }
}

/// Fired Runs open independently; schedule parents and metadata resolve to the
/// stable schedule menu without turning a Run child into its parent.
#[test]
fn schedule_metadata_is_inert_and_a_fired_run_opens_through_open_run() {
    let (mut view, _here) = agents_pane();
    let run = koru_delegate_core::RunId::from_uuid(uuid::Uuid::from_u128(0xa02));
    let schedule = koru_delegate_core::ScheduleId::mint();
    view.set_agent_schedules(
        vec![a_schedule_row(
            schedule,
            vec![crate::schedule_index::ScheduleRunRow {
                run,
                label: run_name(run),
            }],
        )],
        crate::schedule_index::CatalogState::Ready,
    );
    let a = area();
    let placed: Vec<(usize, Rect)> = view.row_rects(a);
    let run_index = view
        .rows
        .iter()
        .position(|row| view.agent_schedule_run(row.node) == Some(run))
        .unwrap();
    let run_rect = placed.iter().find(|(i, _)| *i == run_index).unwrap().1;
    assert_eq!(
        view.mouse_down(a, VIEW, on_label(run_rect), run_rect.centre().1, false),
        Outcome::OpenRun(run)
    );

    let metadata_index = view
        .rows
        .iter()
        .position(|row| view.agent_schedule_metadata_row(row.node) && row.depth == 2)
        .unwrap();
    let metadata_rect = placed.iter().find(|(i, _)| *i == metadata_index).unwrap().1;
    assert_eq!(
        view.mouse_down(
            a,
            VIEW,
            on_label(metadata_rect),
            metadata_rect.centre().1,
            false,
        ),
        Outcome::Consumed
    );
    view.mouse_move(
        a,
        VIEW,
        on_label(metadata_rect),
        metadata_rect.centre().1 + theme::NAV_ROW_H * 3.0,
    );
    assert!(!view.drag_armed());
    assert_eq!(view.mouse_up(), Outcome::Ignored);
    assert_eq!(
        view.context_menu(a, on_label(metadata_rect), metadata_rect.centre().1,),
        Outcome::AgentScheduleMenu {
            schedules: vec![schedule],
            at: (on_label(metadata_rect), metadata_rect.centre().1),
        }
    );
    assert_eq!(
        view.context_menu(a, on_label(run_rect), run_rect.centre().1),
        Outcome::Consumed
    );
    view.focus = Some(view.rows[metadata_index].node);
    assert_eq!(
        view.nav_key(a, &Key::Named(NamedKey::Enter)),
        Some(Outcome::Consumed)
    );
}

/// Long right-to-left rule and metadata strings are elided into their label
/// lane. This exercises the actual fixed-height row renderer, not a surrogate
/// width calculation.
#[test]
fn long_arabic_schedule_words_stay_inside_the_agents_column() {
    let (mut view, _here) = agents_pane();
    let long = "كل يوم اثنين في الساعة التاسعة صباحا حسب توقيت الجهاز الحالي ".repeat(12);
    let mut schedule = a_schedule_row(koru_delegate_core::ScheduleId::mint(), Vec::new());
    schedule.rule = long.clone();
    schedule.next = long.clone();
    schedule.owner = long.clone();
    schedule.state = long;
    view.set_agent_schedules(vec![schedule], crate::schedule_index::CatalogState::Ready);

    let a = area();
    let mut engine = TextEngine::new();
    for (index, rect) in view.row_rects(a) {
        let row = &view.rows[index];
        if row.depth < 1 || !row.label.starts_with('ك') {
            continue;
        }
        let lane = view.row_parts(row, rect).label;
        let fitted = engine.elided(&row.label, theme::role::row(), lane.w);
        assert!(engine.simple_layout(&fitted, theme::role::row()).width() <= lane.w);
        assert!(lane.x >= rect.x && lane.x + lane.w <= rect.x + rect.w);
    }
    draw_once(&mut view, a);
}

/// The shell is one section structure for every tab: the profile/workspace
/// row, the tab row, and the nodes band — named in the inspector so the pane
/// reads as three rows around the mode's content, not one flat container.
/// The gaps between the rows are the stack's own geometry, visible as
/// ExploreSpacer parts.
#[test]
fn the_shell_captures_its_three_sections_around_the_nodes_band() {
    for mode in [
        NavigatorMode::Documents,
        NavigatorMode::Tasks,
        NavigatorMode::Chats,
        NavigatorMode::Artifacts,
    ] {
        let mut v = with_rows(3);
        v.mode = mode;
        if mode == NavigatorMode::Documents {
            v.selected = Some(v.rows[0].node);
        }
        let a = area();
        let mut scene = vello::Scene::new();
        let mut engine = TextEngine::new();
        let root = vello::kurbo::Affine::IDENTITY;
        let capture =
            koru_draw::inspect::Frame::begin(1, root, koru_draw::scene::rect(a.x, a.y, a.w, a.h));
        v.draw(&mut scene, root, &mut engine, a);
        let records = capture.finish().records;

        for name in ["KoruLane", "ExploreTabs", "ExploreNodes", "KoruFloor"] {
            assert_eq!(
                records.iter().filter(|r| r.name == name).count(),
                1,
                "exactly one {name} section in {mode:?}"
            );
        }
        let at = |name| records.iter().position(|r| r.name == name).unwrap();
        let header = records[at("KoruLane")].rect();
        let tabs = records[at("ExploreTabs")].rect();
        let nodes = at("ExploreNodes");
        let band = records[nodes].rect();
        assert!(
            header.y1 <= tabs.y0 && tabs.y1 <= band.y0 + 0.5,
            "header, tabs and nodes stack in order in {mode:?}"
        );
        assert!(
            !records.iter().any(|r| r.name == "KoruAppsRow"),
            "the apps are in the rail, not a band of the pane"
        );
        assert!(
            records[nodes].properties.iter().any(|p| p.name == "mode"),
            "the band says which tab's view it holds"
        );

        // The section gaps are named geometry, and both are the shell's
        // NAV_PAD: header-to-tabs and tabs-to-nodes alike.
        let spacers: Vec<_> = records
            .iter()
            .filter(|r| r.name == "ExploreSpacer")
            .collect();
        assert_eq!(spacers.len(), 2, "one gap per seam in {mode:?}");
        for spacer in &spacers {
            assert_eq!(spacer.rect().height(), theme::NAV_PAD as f64);
        }
        let (first, second) = (spacers[0].rect(), spacers[1].rect());
        assert_eq!(first.y0, header.y1, "the first gap follows the header");
        assert_eq!(first.y1, tabs.y0, "the first gap meets the tab row");
        assert_eq!(second.y0, tabs.y1, "the second gap follows the tab row");
        assert_eq!(second.y1, band.y0, "the second gap meets the nodes band");

        // Every row answers to a named owner: the nodes band for the mode's
        // rows, the footer for the pinned Trash.
        let inside = |records: &[koru_draw::inspect::Record], ancestor: usize, at: usize| {
            let mut parent = records[at].parent;
            while let Some(p) = parent {
                if p == ancestor {
                    return true;
                }
                parent = records[p].parent;
            }
            false
        };
        let footer = records.iter().position(|r| r.name == "ExploreFooter");
        if mode == NavigatorMode::Documents {
            assert!(footer.is_some(), "the pinned footer shows in {mode:?}");
        } else {
            assert!(footer.is_none(), "the rail has no footer rows in {mode:?}");
        }
        let row_indices: Vec<usize> = records
            .iter()
            .enumerate()
            .filter_map(|(i, r)| (r.name == "ExploreRow").then_some(i))
            .collect();
        let min_rows = match mode {
            // The library's rows and the Trash row.
            NavigatorMode::Documents => 2,
            // The rail's and the artifacts column's own rows, no Trash.
            _ => 1,
        };
        assert!(
            row_indices.len() >= min_rows,
            "expected at least {min_rows} ExploreRow(s) in {mode:?}, got {}",
            row_indices.len()
        );
        for i in row_indices {
            assert!(
                inside(&records, nodes, i) || footer.is_some_and(|f| inside(&records, f, i)),
                "an ExploreRow answers to neither the nodes band nor the footer in {mode:?}"
            );
        }
        if mode == NavigatorMode::Documents {
            // The picked row wears the card: the fill that gives the active
            // look is the first thing inside its ExploreRow.
            let carded = records
                .iter()
                .enumerate()
                .find(|(_, r)| {
                    r.name == "ExploreRow"
                        && r.properties
                            .iter()
                            .any(|p| p.name == "treatment" && p.value == "Card")
                })
                .map(|(i, _)| i)
                .expect("a selected row is captured");
            let first_child = records
                .iter()
                .enumerate()
                .find(|(_, r)| r.parent == Some(carded))
                .map(|(_, r)| r.name);
            assert_eq!(first_child, Some("fill"));
        }
    }
}

// --------------------------------------- the one selected file (D259 §4)

/// **The context pane explains one thing**, so this answers for one file and
/// for nothing else.
///
/// A document row has its own Cloud context — the one the pane already
/// showed — and a multi-selection is a question nobody has asked: *the
/// context of these four files* is not a sentence the pane can write.
#[test]
fn only_a_single_file_row_counts_as_the_selected_file() {
    let mut rows = entries(&[("Page", None), ("report.pdf", None), ("archive.zip", None)]);
    rows[1].kind = koru_core::explore::KIND_FILE.to_owned();
    rows[1].mime = Some("application/pdf".to_owned());
    rows[2].kind = koru_core::explore::KIND_FILE.to_owned();
    rows[2].mime = Some("application/octet-stream".to_owned());
    let mut view = ExploreView::new();
    view.rebuild_from_entries(&rows);
    view.show_files();

    // Nothing selected: nothing to explain.
    assert!(view.selected_file().is_none());

    let node_of = |label: &str| {
        view.rows
            .iter()
            .find(|row| row.label == label)
            .expect("the row")
            .node
    };
    let (pdf, zip) = (node_of("report.pdf"), node_of("archive.zip"));
    view.select_only(pdf);
    let chosen = view.selected_file().expect("one file is selected");
    assert_eq!(chosen.label, "report.pdf");
    assert_eq!(chosen.mime.as_deref(), Some("application/pdf"));

    // A second file is a second question.
    view.selection.insert(zip);
    assert!(
        view.selected_file().is_none(),
        "a multi-selection is not one file",
    );

    // And a document row is not a file: it has its own context, which is the
    // one the pane shows when no file is selected.
    view.show_documents();
    let page = view.rows[0].node;
    view.select_only(page);
    assert!(view.selected_file().is_none());
}

// ------------------------------------------------------------ the chats mode

fn a_chat(n: u128, title: &str, updated_ms: i64) -> koru_ai_core::Chat {
    koru_ai_core::Chat {
        id: koru_chats_ui::ChatId::from_uuid(uuid::Uuid::from_u128(n)),
        title: title.into(),
        private: false,
        current_model: koru_ai_core::ModelSelection {
            provider: koru_ai_core::AiProviderId::parse("openai").unwrap(),
            requested_model: "gpt-test".into(),
            default_reasoning: koru_ai_core::ReasoningChoice::Default,
        },
        branched_from: None,
        agent_working_folder: None,
        created_at: koru_ai_core::Timestamp::from_millis(updated_ms),
        updated_at: koru_ai_core::Timestamp::from_millis(updated_ms),
    }
}

fn showing_chats(chats: Vec<koru_ai_core::Chat>) -> ExploreView {
    let mut view = ExploreView::new();
    view.show_chats();
    view.chats_mut().take_listing(Ok(koru_chats_ui::Listing {
        chats,
        agents: Vec::new(),
    }));
    view
}

/// **Chats sits right of Tasks** (R3): the fourth icon, `Alt+4`, with Agents
/// and Artifacts one step further along — and the column below the heading
/// becomes the Chats surface's.
#[test]
fn the_chats_mode_is_the_fourth_icon_and_owns_the_column() {
    assert_eq!(
        NavigatorMode::ALL,
        [
            NavigatorMode::Home,
            NavigatorMode::Documents,
            NavigatorMode::Files,
            NavigatorMode::Tasks,
            NavigatorMode::Chats,
            NavigatorMode::Agents,
            NavigatorMode::Artifacts,
            NavigatorMode::Canvases,
            NavigatorMode::Socials,
        ]
    );
    let mut view = ExploreView::new();
    let a = area();
    let (_, chats) = view.navigator_mode_rects(view.rail_of(a))[4];
    let (x, y) = chats.centre();
    assert_eq!(
        view.apps_rail_mouse_down(view.rail_of(a), x, y),
        Outcome::Consumed
    );
    assert_eq!(view.navigator_mode(), NavigatorMode::Chats);
    assert!(view.showing_chats());
    assert!(view.rows.is_empty(), "the surface is not the library");
    assert_eq!(NavigatorMode::Chats.token(), "chats");
    assert_eq!(NavigatorMode::parse("chats"), Some(NavigatorMode::Chats));
    assert_eq!(
        NavigatorMode::Chats.shortcut(),
        Some(koru_keys::ShortcutId::ShowChats)
    );
    assert_eq!(
        ExploreView::navigator_mode_icon(NavigatorMode::Chats),
        koru_chats_ui::MODE_ICON
    );
    let chord = koru_keys::registry::get(koru_keys::ShortcutId::ShowChats).chord;
    assert!(chord.matches(&Key::Character("4".into()), Some("4"), koru_keys::VIEW));
}

#[test]
fn socials_mode_requests_its_shared_tab_and_has_no_library_footer() {
    let mut view = ExploreView::new();
    let a = area();
    let (_, socials) = view
        .navigator_mode_rects(view.rail_of(a))
        .into_iter()
        .find(|(mode, _)| *mode == NavigatorMode::Socials)
        .expect("Socials shows in every build until ENABLE_SOCIALS");
    let (x, y) = socials.centre();
    assert_eq!(
        view.apps_rail_mouse_down(view.rail_of(a), x, y),
        Outcome::OpenSocials
    );
    assert!(view.showing_socials());
    assert!(view.rows.is_empty());
    assert!(view.footer_rects(a).is_empty());
    assert_eq!(
        NavigatorMode::parse(NavigatorMode::Socials.token()),
        Some(NavigatorMode::Socials)
    );
    assert_eq!(
        NavigatorMode::Socials.shortcut(),
        Some(koru_keys::ShortcutId::ShowSocials)
    );
    assert_eq!(
        ExploreView::navigator_mode_icon(NavigatorMode::Socials),
        icons::MEGAPHONE
    );
    let shared = view.ensure_socials(None);
    assert!(std::rc::Rc::ptr_eq(&shared, &view.ensure_socials(None)));
}

/// **Agent tabs ship without the views over them** (maintainers,
/// 2026-10-16): agents on alone keeps Agents and Artifacts hidden, and a
/// request for either lands on Documents; agent views bring both back.
#[test]
fn agents_on_alone_keeps_the_agents_and_artifacts_views_hidden() {
    use koru_features::Features;
    let tabs_only = Features {
        agents: true,
        ..Features::ALL_OFF
    };
    koru_features::testing::with(tabs_only, || {
        assert!(!NavigatorMode::Agents.shown_in_product());
        assert!(!NavigatorMode::Artifacts.shown_in_product());
        let mut view = ExploreView::new();
        view.show_agents();
        assert_eq!(view.navigator_mode(), NavigatorMode::Documents);
    });
    let with_views = Features {
        agents: true,
        agent_views: true,
        ..Features::ALL_OFF
    };
    koru_features::testing::with(with_views, || {
        assert!(NavigatorMode::Agents.shown_in_product());
        assert!(NavigatorMode::Artifacts.shown_in_product());
        let mut view = ExploreView::new();
        view.show_agents();
        assert_eq!(view.navigator_mode(), NavigatorMode::Agents);
    });
}

/// **Chips are sized from the modes this build shows**: a hidden mode has
/// no chip, Files is always second, and every chip is a square before the
/// labels are measured — whatever the build shows, nothing shrinks.
#[test]
fn mode_icons_are_sized_from_the_modes_the_build_shows() {
    use koru_features::Features;
    let a = area();
    let four = Features {
        tasks: true,
        ai: true,
        ..Features::ALL_OFF
    };
    for (features, shown) in [(Features::ALL_OFF, 3), (four, 5)] {
        koru_features::testing::with(features, || {
            let view = ExploreView::new();
            let rects = view.navigator_mode_rects(view.rail_of(a));
            assert_eq!(rects.len(), shown);
            for &(mode, r) in &rects {
                assert_eq!(r.w, apps_rail::SQUARE, "{mode:?} at {shown} shown");
            }
            assert_eq!(rects[2].0, NavigatorMode::Files);
            assert!(view.actions_button(a).w > 0.);
        });
    }
    let view = ExploreView::new();
    let rects = view.navigator_mode_rects(view.rail_of(a));
    assert_eq!(rects.len(), NavigatorMode::ALL.len());
    assert!(rects.iter().all(|(_, r)| r.w == apps_rail::SQUARE));
}

/// **The rail does not change with the pane's width**: the squares sit where
/// they sit, the foot's three controls stand on the band's floor, and the
/// heading's `…` has its band in the pane whatever the width.
#[test]
fn the_rail_keeps_its_squares_at_every_pane_width() {
    let view = ExploreView::new();
    let mut seen: Option<Vec<Rect>> = None;
    for width in [256., 300., 400., 600.] {
        let a = Rect::new(theme::RAIL_W, 0., width, 800.);
        let rail = view.rail_of(a);
        let r = view.apps_rail_rects(rail);
        let squares: Vec<Rect> = r.apps.iter().map(|(_, r)| *r).collect();
        if let Some(seen) = &seen {
            assert_eq!(&squares, seen, "the rail moved with the pane at {width}");
        }
        seen = Some(squares);
        assert_eq!(r.profile.y + r.profile.h, rail.y + rail.h - 8.0);
        assert!(r.settings.y + r.settings.h <= r.help.y);
        assert!(r.help.y + r.help.h <= r.profile.y);
        assert!(view.actions_button(a).w > 0.);
    }
}

/// The surface is built without a reader under test.
#[test]
fn the_chats_surface_dials_nothing_under_test() {
    let mut view = ExploreView::new();
    view.show_chats();
    view.start_chats(None);
    assert!(!view.chats_mut().poll());
    assert_eq!(view.chats().load(), koru_chats_ui::Load::Cold);
    assert!(!view.chats_awaiting());
}

/// A click on a chat row asks the container to open it; *New chat* asks for
/// a fresh one; a right-click names the chat for the container's menu.
#[test]
fn a_press_in_the_chats_view_is_reported_as_what_it_asks_for() {
    let mut view = showing_chats(vec![a_chat(1, "Older", 1_000), a_chat(2, "Newer", 2_000)]);
    let a = area();
    let top = view.rows_top(a);
    let rows = view.chats_mut().layout(a, top);
    let (_, new_rect) = rows[0];
    let (_, newest) = rows
        .iter()
        .copied()
        .find(|(row, _)| *row == koru_chats_ui::Row::Chat(0))
        .unwrap();
    let newer = koru_chats_ui::ChatId::from_uuid(uuid::Uuid::from_u128(2));
    assert_eq!(
        view.mouse_down(a, (1200.0, 800.0), a.x + 60.0, newest.y + 6.0, false),
        Outcome::OpenAiChat(newer)
    );
    assert_eq!(
        view.mouse_down(a, (1200.0, 800.0), a.x + 60.0, new_rect.y + 6.0, false),
        Outcome::NewAiChat
    );
    assert_eq!(
        view.context_menu(a, a.x + 60.0, newest.y + 6.0),
        Outcome::AiChatMenu {
            chat: newer,
            at: (a.x + 60.0, newest.y + 6.0)
        }
    );
}

/// The keyboard: focus enters on open, the arrows and Home/End move, Enter
/// opens, Delete asks, Escape gives the keyboard back — and nothing else is
/// claimed.
#[test]
fn the_chats_view_keeps_the_interaction_contract() {
    let mut view = showing_chats(vec![a_chat(1, "Older", 1_000), a_chat(2, "Newer", 2_000)]);
    let a = area();
    assert!(view.focus_tree(a), "focus enters on open");
    assert!(view.has_focus());
    let older = koru_chats_ui::ChatId::from_uuid(uuid::Uuid::from_u128(1));
    assert_eq!(
        view.nav_key(a, &Key::Named(NamedKey::End)),
        Some(Outcome::Consumed)
    );
    assert_eq!(
        view.nav_key(a, &Key::Named(NamedKey::Enter)),
        Some(Outcome::OpenAiChat(older))
    );
    assert_eq!(
        view.nav_key(a, &Key::Named(NamedKey::Delete)),
        Some(Outcome::DeleteAiChat(older))
    );
    assert!(view.nav_key(a, &Key::Character("a".into())).is_none());
    assert!(view.nav_key(a, &Key::Named(NamedKey::Tab)).is_none());
    assert_eq!(
        view.nav_key(a, &Key::Named(NamedKey::Escape)),
        Some(Outcome::Consumed)
    );
    assert!(!view.has_focus(), "focus returns on close");
}

/// `Ctrl+Shift+E`'s half in the pane: show the view on one chat, with the
/// keyboard on it.
#[test]
fn reveal_chat_shows_the_view_with_the_chat_focused() {
    let mut view = showing_chats(vec![a_chat(1, "Older", 1_000), a_chat(2, "Newer", 2_000)]);
    view.show_documents();
    let older = koru_chats_ui::ChatId::from_uuid(uuid::Uuid::from_u128(1));
    assert!(view.reveal_chat(older, area()));
    assert!(view.showing_chats());
    assert_eq!(view.chats().focused_chat(), Some(older));
    assert!(view.has_focus());
}

/// The accessibility projection: a list named *Chats*, *New chat* first, and
/// one row per chat named by title, who answered and time, with a Delete
/// button under it.
#[test]
fn the_chats_view_publishes_a_row_per_chat_with_open_and_delete() {
    let view = showing_chats(vec![a_chat(1, "Release notes", 1_000)]);
    let cx = super::accessibility::Context {
        window: koru_workspace_host::accessibility::WindowSlot::from_raw(1),
        view: koru_workspace_host::accessibility::ViewSlot::from_raw(1),
        area: area(),
        rail: view.rail_of(area()),
        viewport: (1200.0, 800.0),
        lang: koru_i18n::Lang::En,
    };
    let column = view.semantics(&cx);
    let list = column
        .children
        .iter()
        .find(|node| node.role == accesskit::Role::List)
        .expect("the Chats view publishes a list");
    assert_eq!(list.label.as_deref(), Some("Chats"));
    assert_eq!(list.children.len(), 2, "New chat, then the one chat");
    assert_eq!(list.children[0].label.as_deref(), Some("New chat"));
    let row = &list.children[1];
    assert_eq!(row.role, accesskit::Role::ListItem);
    let name = row.label.as_deref().unwrap_or_default();
    assert!(name.starts_with("Release notes, gpt-test, "), "{name}");
    assert!(row.actions.contains(&accesskit::Action::Click));
    assert_eq!(row.children.len(), 1);
    assert_eq!(row.children[0].label.as_deref(), Some("Delete chat"));
}

/// **The simplified product's mode row** (koru-features all off): only
/// Documents and Files have icons, packed left with no gap, each hit-tests to
/// itself, and asking for a hidden mode — a restored token, a reveal, an
/// accessibility action — lands on Documents.
#[test]
fn hidden_modes_have_no_icon_and_cannot_be_shown() {
    koru_features::testing::with(koru_features::Features::ALL_OFF, || {
        let mut view = ExploreView::new();
        let a = area();
        let rects = view.navigator_mode_rects(view.rail_of(a));
        let modes: Vec<NavigatorMode> = rects.iter().map(|(mode, _)| *mode).collect();
        assert_eq!(
            modes,
            [
                NavigatorMode::Home,
                NavigatorMode::Documents,
                NavigatorMode::Files
            ]
        );
        // Files keeps its place within the build: third, one gap under
        // Documents, on every call.
        assert_eq!(
            rects,
            view.navigator_mode_rects(view.rail_of(a)),
            "Files keeps its place"
        );
        assert_eq!(rects[2].1.y, rects[1].1.y + rects[1].1.h + apps_rail::GAP);
        for &(mode, r) in &rects {
            let (x, y) = r.centre();
            assert_eq!(mode_at(&view, a, x, y), Some(mode));
        }
        // Past Files there is nothing: the hidden icons leave no gap and no
        // target where Tasks' icon would be.
        let files = rects[2].1;
        let (x, y) = (
            files.x + files.w / 2.,
            files.y + files.h + apps_rail::GAP + files.h / 2.,
        );
        assert_eq!(mode_at(&view, a, x, y), None);
        for hidden in [
            NavigatorMode::Tasks,
            NavigatorMode::Chats,
            NavigatorMode::Agents,
            NavigatorMode::Artifacts,
            NavigatorMode::Canvases,
            NavigatorMode::Socials,
        ] {
            view.set_navigator_mode(NavigatorMode::Files);
            view.set_navigator_mode(hidden);
            assert_eq!(view.mode, NavigatorMode::Documents, "{hidden:?}");
        }
    });
}
