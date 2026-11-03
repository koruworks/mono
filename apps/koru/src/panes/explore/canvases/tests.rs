// SPDX-License-Identifier: MPL-2.0

use super::*;

use crate::panes::explore::{FileState, icon_lane};
use koru_core::explore::{ExploreEntry, KIND_DOCUMENT};
use koru_core::schema::WorkspaceDoc;
use koru_core::{DocumentId, WorkspaceId};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme;

const VIEW: (f32, f32) = (1200.0, 700.0);

fn area() -> Rect {
    Rect::new(0.0, theme::CAPTION_H, theme::SIDEBAR_W, 600.0)
}

fn entry(title: &str, kind: &str, parent: Option<DocumentId>) -> ExploreEntry {
    ExploreEntry {
        id: DocumentId::new(),
        parent,
        ordinal: 0,
        title: title.to_owned(),
        kind: kind.to_owned(),
        mime: None,
        note: None,
        trashed_at: None,
        trashed_by: None,
    }
}

fn node(entry: &ExploreEntry) -> NodeId {
    NodeId::from_uuid(entry.id.as_uuid())
}

fn labels(v: &ExploreView) -> Vec<&str> {
    v.rows.iter().map(|r| r.label.as_str()).collect()
}

fn row_centre(v: &ExploreView, a: Rect, i: usize) -> f32 {
    v.rows_top(a) + i as f32 * theme::NAV_ROW_H + theme::NAV_ROW_H * 0.5
}

/// A workspace of two pages and two canvases — one of them nested under a page
/// by somebody, somewhere, which the flat list must still show.
fn a_workspace() -> Vec<ExploreEntry> {
    let brief = entry("Brief", KIND_DOCUMENT, None);
    let board = entry("Board", KIND_CANVAS, None);
    let notes = entry("Notes", KIND_DOCUMENT, Some(brief.id));
    let sketch = entry("Sketch", KIND_CANVAS, Some(brief.id));
    vec![brief, notes, sketch, board]
}

/// **The canvases mode is reached the way the others are** — its icon (the
/// seventh, Home hidden for now), its token, its chord — and says its name
/// in the person's language.
#[test]
fn the_canvases_mode_is_the_seventh_icon_and_answers_to_alt_7() {
    let mut view = ExploreView::new();
    let a = area();
    let (mode, icon) = view.navigator_mode_rects(view.rail_of(a))[6];
    assert_eq!(mode, NavigatorMode::Canvases);
    let (x, y) = icon.centre();
    assert_eq!(
        view.apps_rail_mouse_down(view.rail_of(a), x, y),
        Outcome::Consumed,
        "the column's own business"
    );
    assert!(view.showing_canvases());
    assert_eq!(NavigatorMode::Canvases.token(), "canvases");
    assert_eq!(
        NavigatorMode::parse("canvases"),
        Some(NavigatorMode::Canvases)
    );
    assert_eq!(
        NavigatorMode::Canvases.shortcut(),
        Some(koru_keys::ShortcutId::ShowCanvases)
    );
    assert_eq!(
        NavigatorMode::Canvases.label(),
        koru_i18n::t(crate::format::lang(), koru_i18n::Key::CanvasesTitle)
    );
    assert_eq!(
        ExploreView::navigator_mode_icon(NavigatorMode::Canvases),
        koru_draw::icons::CANVAS
    );
    let chord = koru_keys::registry::get(koru_keys::ShortcutId::ShowCanvases).chord;
    assert!(chord.matches(
        &winit::keyboard::Key::Character("7".into()),
        Some("7"),
        koru_keys::VIEW
    ));
}

/// **Canvases lists every canvas, flat, and Documents lists none.** A canvas
/// nested under a page is at the top level here — the section is the one place
/// a person looks for canvases — and every row is a document row that opens,
/// with no caret and nothing under it.
#[test]
fn canvases_are_one_flat_list_and_documents_does_not_show_them() {
    let rows = a_workspace();
    let mut view = ExploreView::new();
    view.rebuild_from_entries(&rows);
    assert_eq!(labels(&view), ["Brief", "Notes"], "Documents is pages only");

    view.show_canvases();
    assert_eq!(labels(&view), ["Sketch", "Board"], "manifest order");
    for row in &view.rows {
        assert_eq!(row.depth, 0, "{} is flat", row.label);
        assert!(!row.has_children);
        assert_eq!(row.kind, Kind::Document, "a canvas row opens like a page");
        assert_eq!(row.file_state, FileState::Present);
    }
    // No caret lane is paid for in a list that has no carets.
    let (_, r) = view.row_rects(area()).into_iter().next().unwrap();
    assert_eq!(
        view.row_parts(&view.rows[0], r).icon.x,
        icon_lane(r.x, 0, false)
    );

    view.show_documents();
    assert_eq!(labels(&view), ["Brief", "Notes"]);
}

/// A trip through Canvases leaves the document tree's folds exactly as they
/// were — the section prunes nothing it does not own.
#[test]
fn a_trip_through_canvases_keeps_the_document_folds() {
    let rows = a_workspace();
    let brief = node(&rows[0]);
    let mut view = ExploreView::new();
    view.set_collapsed([brief]);
    view.rebuild_from_entries(&rows);
    assert_eq!(labels(&view), ["Brief"]);
    view.show_canvases();
    view.rebuild_from_entries(&rows);
    view.show_documents();
    assert_eq!(labels(&view), ["Brief"], "still folded");
    assert!(view.collapsed().contains(&brief));
}

/// An untitled canvas reads *Untitled*, localized, and a leading emoji is its
/// icon rather than part of its name.
#[test]
fn a_canvas_row_is_named_the_way_a_page_row_is() {
    let rows = vec![
        entry("", KIND_CANVAS, None),
        entry("🎯 Goals", KIND_CANVAS, None),
    ];
    let mut view = ExploreView::new();
    view.show_canvases();
    view.rebuild_from_entries(&rows);
    assert_eq!(
        labels(&view),
        [
            koru_i18n::t(crate::format::lang(), koru_i18n::Key::DocUntitled),
            "Goals"
        ]
    );
    assert_eq!(view.rows[1].icon.as_deref(), Some("🎯"));
}

/// **A click opens the canvas** — the release of an unarmed press on its row,
/// the same outcome a page's row reports.
#[test]
fn a_click_on_a_canvas_row_opens_it() {
    let rows = a_workspace();
    let mut view = ExploreView::new();
    view.show_canvases();
    view.rebuild_from_entries(&rows);
    let a = area();
    let (_, r) = view.row_rects(a).into_iter().next().unwrap();
    view.mouse_down(a, VIEW, r.x + r.w * 0.5, r.centre().1, false);
    assert_eq!(view.mouse_up(), Outcome::OpenNode(node(&rows[2])));
}

/// **Never reordered, never nested.** A drag released over the list is no
/// move at all. (The Trash row it could once be dropped on left the nav's
/// foot for the floor menu, E63.)
#[test]
fn a_canvas_drag_moves_nothing_in_the_list() {
    let rows = a_workspace();
    let mut view = ExploreView::new();
    view.show_canvases();
    view.rebuild_from_entries(&rows);
    let a = area();
    let x = a.x + a.w * 0.5;

    // Board, dragged up over Sketch — where a page would reorder or nest.
    view.mouse_down(a, VIEW, x, row_centre(&view, a, 1), false);
    view.mouse_move(a, VIEW, x, view.rows_top(a) + 2.0);
    assert!(view.drag_armed());
    assert_eq!(view.mouse_up(), Outcome::Consumed, "no MoveNodes");
}

/// The empty section says what would put something in it, and its button
/// makes a canvas — in the person's language, on screen and to a screen reader.
#[test]
fn the_empty_section_offers_new_canvas() {
    let mut view = ExploreView::new();
    view.current = Some(WorkspaceId::new());
    view.show_canvases();
    view.rebuild_from_entries(&[entry("Brief", KIND_DOCUMENT, None)]);
    assert!(view.rows.is_empty(), "a page is not a canvas");
    let lang = crate::format::lang();
    let (hint, label, action) = view.empty_state();
    assert_eq!(hint, koru_i18n::t(lang, koru_i18n::Key::CanvasesEmpty));
    assert_eq!(label, koru_i18n::t(lang, koru_i18n::Key::CanvasNew));
    assert_eq!(action, Outcome::NewCanvas);

    let a = area();
    let mut scene = vello::Scene::new();
    let mut engine = TextEngine::new();
    view.draw(&mut scene, vello::kurbo::Affine::IDENTITY, &mut engine, a);
    let cta = view.empty_cta.expect("a button was laid out");
    assert_eq!(
        view.mouse_down(a, VIEW, cta.centre().0, cta.centre().1, false),
        Outcome::NewCanvas
    );
}

/// No library actions — nothing imports as a canvas and a flat list has no
/// folds — so there is no `…`, and empty space offers no import menu.
#[test]
fn the_canvases_section_has_no_library_actions() {
    let mut view = ExploreView::new();
    view.show_canvases();
    let a = area();
    assert_eq!(view.actions_button(a).w, 0.0);
    let below = view.rows_top(a) + theme::NAV_ROW_H * 3.0;
    assert_eq!(
        view.context_menu(a, a.x + a.w * 0.5, below),
        Outcome::Consumed
    );
}

/// A workspace with no manifest has no canvases: its tree projection is pages,
/// and none of them may claim to be one.
#[test]
fn a_tree_projection_lists_no_canvases() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    ws.insert(
        koru_core::schema::Parent::Root,
        koru_core::node::Node::document("Page").unwrap(),
    )
    .unwrap();
    let mut view = ExploreView::new();
    view.show_canvases();
    view.rebuild(&ws);
    assert!(view.rows.is_empty());
}

/// **Fixed on the way: a page under a filtered-out parent.** Documents does not
/// list canvases, so a page whose manifest parent is a canvas used to be drawn
/// one level in, under nothing. It is placed under its nearest *listed*
/// ancestor now — at the top when there is none, under the page above the
/// canvas when there is one.
#[test]
fn a_page_under_a_canvas_keeps_a_sensible_depth_in_documents() {
    let brief = entry("Brief", KIND_DOCUMENT, None);
    let board = entry("Board", KIND_CANVAS, Some(brief.id));
    let pinned = entry("Pinned to the board", KIND_DOCUMENT, Some(board.id));
    let loose_canvas = entry("Loose", KIND_CANVAS, None);
    let orphan = entry(
        "Under a top-level canvas",
        KIND_DOCUMENT,
        Some(loose_canvas.id),
    );
    let rows = vec![brief, board, pinned, loose_canvas, orphan];

    let mut view = ExploreView::new();
    view.rebuild_from_entries(&rows);
    let depth: Vec<(&str, u16)> = view
        .rows
        .iter()
        .map(|row| (row.label.as_str(), row.depth))
        .collect();
    assert_eq!(
        depth,
        [
            ("Brief", 0),
            ("Pinned to the board", 1),
            ("Under a top-level canvas", 0),
        ]
    );
    assert!(
        view.rows[0].has_children,
        "Brief carries the caret it earns"
    );
    assert!(!view.rows[2].has_children);

    // And folding Brief hides the page that sits under it through the canvas.
    view.set_collapsed([node(&rows[0])]);
    view.rebuild_from_entries(&rows);
    assert_eq!(labels(&view), ["Brief", "Under a top-level canvas"]);
}
