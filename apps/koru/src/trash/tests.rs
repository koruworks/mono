// SPDX-License-Identifier: MPL-2.0

//! The Trash tab's geometry and what a press on it asks for.
//!
//! Pure view: it never mutates a document, so every test here builds a view,
//! hands it a listing, and checks that what it draws and what it hits agree.

use super::view::{Hit, LANE_W, TrashView};
use koru_core::NodeId;
use koru_ui::table::Band;
use koru_workspace_host::View;
use winit::keyboard::{Key, ModifiersState, NamedKey};

fn view(n: usize) -> TrashView {
    let mut v = TrashView::new();
    v.set_viewport(900.0, 600.0);
    v.entries = (0..n)
        .map(|i| super::Entry {
            node: NodeId::new(),
            name: format!("Page {i}"),
            origin: "Library".into(),
            trashed_at_ms: Some(1_700_000_000_000),
            nested: if i == 1 { 3 } else { 0 },
            icon: None,
            is_file: false,
            mime: None,
            deleted_by: "You".into(),
            run: None,
        })
        .collect();
    v
}

#[test]
fn trash_lists_newest_delete_first() {
    let mut v = TrashView::new();
    assert!(v.set_documents(vec![
        koru_core::explore::ExploreEntry {
            id: koru_core::DocumentId::new(),
            parent: None,
            ordinal: 0,
            title: "Older".into(),
            kind: koru_core::explore::KIND_DOCUMENT.into(),
            mime: None,
            note: None,
            trashed_at: Some(100),
            trashed_by: None,
        },
        koru_core::explore::ExploreEntry {
            id: koru_core::DocumentId::new(),
            parent: None,
            ordinal: 1,
            title: "Newest".into(),
            kind: koru_core::explore::KIND_DOCUMENT.into(),
            mime: None,
            note: None,
            trashed_at: Some(300),
            trashed_by: None,
        },
        koru_core::explore::ExploreEntry {
            id: koru_core::DocumentId::new(),
            parent: None,
            ordinal: 2,
            title: "Middle".into(),
            kind: koru_core::explore::KIND_DOCUMENT.into(),
            mime: None,
            note: None,
            trashed_at: Some(200),
            trashed_by: None,
        },
    ]));
    // set_documents only marks stale; catch_up builds and sorts the rows.
    let ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").unwrap();
    assert!(v.catch_up(&ws));
    let names: Vec<_> = v.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["Newest", "Middle", "Older"]);
}

fn centre(r: koru_draw::geometry::Rect) -> (f32, f32) {
    (r.x + r.w * 0.5, r.y + r.h * 0.5)
}

/// The round trip. Every target hits back where it is drawn.
#[test]
fn every_target_hit_tests_back_to_where_it_is_drawn() {
    let v = view(4);
    let (t, bands) = (v.table(), v.bands());
    assert_eq!(bands.len(), 4);
    assert!(bands.iter().all(|b| *b == Band::Row), "no sections here");

    for i in 0..4 {
        let r = t.row_rect(&bands, i);
        let c = v.controls(r);
        assert_eq!(
            v.hit_at(centre(c[0]).0, centre(c[0]).1),
            Some(Hit::Restore(i))
        );
        assert_eq!(v.hit_at(centre(c[1]).0, centre(c[1]).1), Some(Hit::Dots(i)));
        // The row itself, left of the lane.
        assert_eq!(v.hit_at(r.x + 40.0, r.y + r.h * 0.5), Some(Hit::Row(i)));
    }
    let (hx, hy) = centre(v.header_dots());
    assert_eq!(v.hit_at(hx, hy), Some(Hit::HeaderDots));
}

/// The controls sit *in* the row, so they outrank it — exactly as a tab's close
/// box outranks the tab it is drawn in.
#[test]
fn restore_outranks_the_row_it_sits_in() {
    let v = view(2);
    let r = v.table().row_rect(&v.bands(), 0);
    let (x, y) = centre(v.controls(r)[0]);
    assert_eq!(v.hit_at(x, y), Some(Hit::Restore(0)));
    assert_ne!(v.hit_at(x, y), Some(Hit::Row(0)));
}

/// **The reflow guard**, from the view's side: the cells are the same width
/// whether the pointer is on the row or not, because the lane is reserved on
/// every row and only what is drawn in it changes.
#[test]
fn the_lane_is_reserved_on_every_row() {
    let v = view(3);
    let (t, bands) = (v.table(), v.bands());
    let cols = v.columns();
    let widths: Vec<f32> = (0..3)
        .map(|i| t.cell(&cols, LANE_W, 0, t.row_rect(&bands, i)).w)
        .collect();
    assert!(widths.windows(2).all(|w| w[0] == w[1]), "{widths:?}");
}

/// Restoring asks the container and mutates nothing.
#[test]
fn restoring_asks_the_container_and_mutates_nothing() {
    let mut v = view(2);
    let node = v.entries[0].node;
    let r = v.table().row_rect(&v.bands(), 0);
    let (x, y) = centre(v.controls(r)[0]);

    let mut doc =
        koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").unwrap();
    assert!(View::mouse_down(&mut v, &mut doc, x, y, false));
    assert_eq!(v.take_request(), Some(super::Request::Restore(vec![node])));
    // The listing is untouched: the container performs, the view reports.
    assert_eq!(v.entries.len(), 2);
}

#[test]
fn ctrl_click_toggles_individual_trash_rows() {
    let mut v = view(4);
    let ids: Vec<_> = v.entries.iter().map(|e| e.node).collect();
    let mut doc =
        koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").unwrap();
    for i in [0, 2] {
        let r = v.table().row_rect(&v.bands(), i);
        assert!(View::mouse_down(
            &mut v,
            &mut doc,
            r.x + 40.0,
            r.centre().1,
            true,
        ));
    }
    assert_eq!(v.selection, [ids[0], ids[2]].into_iter().collect());

    let r = v.table().row_rect(&v.bands(), 0);
    View::mouse_down(&mut v, &mut doc, r.x + 40.0, r.centre().1, true);
    assert_eq!(v.selection, [ids[2]].into_iter().collect());
}

#[test]
fn shift_click_selects_the_full_trash_range() {
    let mut v = view(5);
    let ids: Vec<_> = v.entries.iter().map(|e| e.node).collect();
    let mut doc =
        koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").unwrap();
    let first = v.table().row_rect(&v.bands(), 1);
    View::mouse_down(&mut v, &mut doc, first.x + 40.0, first.centre().1, false);
    View::set_mods(&mut v, ModifiersState::SHIFT);
    let last = v.table().row_rect(&v.bands(), 4);
    View::mouse_down(&mut v, &mut doc, last.x + 40.0, last.centre().1, false);
    assert_eq!(v.selection, ids[1..=4].iter().copied().collect());
}

#[test]
fn a_selected_rows_restore_button_acts_on_the_ordered_selection() {
    let mut v = view(4);
    let ids: Vec<_> = v.entries.iter().map(|e| e.node).collect();
    v.selection.extend([ids[0], ids[2]]);
    let r = v.table().row_rect(&v.bands(), 2);
    let (x, y) = centre(v.controls(r)[0]);
    let mut doc =
        koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").unwrap();
    View::mouse_down(&mut v, &mut doc, x, y, false);
    assert_eq!(
        v.take_request(),
        Some(super::Request::Restore(vec![ids[0], ids[2]]))
    );
}

#[test]
fn delete_is_not_a_permanent_delete_shortcut_in_trash() {
    let mut v = view(1);
    v.selection.insert(v.entries[0].node);
    let mut doc =
        koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").unwrap();
    assert!(!View::key(
        &mut v,
        &mut doc,
        &Key::Named(NamedKey::Delete),
        None,
        ModifiersState::empty(),
    ));
    assert_eq!(v.take_request(), None);
}

#[test]
fn the_header_dots_open_a_menu_under_the_button() {
    let mut v = view(1);
    let d = v.header_dots();
    let (x, y) = centre(d);
    let mut doc =
        koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").unwrap();
    assert!(View::mouse_down(&mut v, &mut doc, x, y, false));
    assert_eq!(
        v.take_request(),
        Some(super::Request::HeaderMenu {
            at: (d.x + d.w, d.y + d.h)
        }),
        "anchored at the button's bottom-right, so the menu hangs back under it"
    );
}

/// **Nothing.** Opening a document that is in the bin is a state the tab strip
/// has no story for, and "restore, then open" is two decisions worn as one.
#[test]
fn a_double_click_opens_nothing() {
    let mut v = view(2);
    let r = v.table().row_rect(&v.bands(), 0);
    let doc = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").unwrap();
    assert!(!View::mouse_double_click(
        &mut v,
        &doc,
        r.x + 40.0,
        r.y + r.h * 0.5
    ));
    assert_eq!(v.take_request(), None);
}

/// A listing read from the document, rather than made up here: this is what
/// `catch_up` actually produces, including the "Untitled" substitution and the
/// origin path.
#[test]
fn catch_up_reads_the_listing_once_and_then_stops() {
    use koru_core::node::Node;
    use koru_core::schema::Parent;

    let mut doc =
        koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").unwrap();
    let home = doc
        .insert(Parent::Root, Node::document("Projects").unwrap())
        .unwrap();
    let d = doc
        .insert(Parent::Node(home), Node::document("").unwrap())
        .unwrap();
    doc.trash(d, 1_700_000_000_000, Some("Sam")).unwrap();

    let mut v = TrashView::new();
    assert!(v.catch_up(&doc), "stale on arrival");
    assert!(!v.catch_up(&doc), "and not again until something changes");
    assert_eq!(v.entries.len(), 1);
    assert_eq!(v.entries[0].name, "Untitled", "an empty title still reads");
    assert_eq!(v.entries[0].origin, "Projects");

    v.mark_stale();
    assert!(v.catch_up(&doc), "and again once it is told to");
}

#[test]
fn an_empty_trash_has_no_rows_and_no_targets() {
    let v = view(0);
    assert!(v.is_empty());
    assert!(v.bands().is_empty());
    assert_eq!(v.hit_at(100.0, 300.0), None);
}

// -------------------------------------------------------------- the rows

/// **The same height as an explore row.** These are the same kind of thing —
/// one line naming a document — and a table row half again as tall made the
/// list read as a spreadsheet of records rather than a list of pages.
#[test]
fn a_row_is_as_tall_as_an_explore_row() {
    let v = view(2);
    assert_eq!(v.table().metrics.row_h, koru_draw::theme::NAV_ROW_H);
}

/// A page that starts with an emoji wears it, on the same rule the explore
/// column and the tab strip follow — and the emoji comes **off** the name, so
/// it is not drawn twice.
#[test]
fn a_leading_emoji_becomes_the_icon_and_leaves_the_name() {
    use koru_core::node::Node;
    use koru_core::schema::Parent;

    let mut doc =
        koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").unwrap();
    let a = doc
        .insert(Parent::Root, Node::document("\u{1F680} Ship it").unwrap())
        .unwrap();
    let b = doc
        .insert(Parent::Root, Node::document("Plain page").unwrap())
        .unwrap();
    let c = doc
        .insert(Parent::Root, Node::document("\u{1F525}").unwrap())
        .unwrap();
    for id in [a, b, c] {
        doc.trash(id, 1_700_000_000_000, None).unwrap();
    }

    let mut v = TrashView::new();
    v.catch_up(&doc);
    let by_node = |id| v.entries.iter().find(|e| e.node == id).unwrap().clone();

    let ship = by_node(a);
    assert_eq!(ship.icon.as_deref(), Some("\u{1F680}"));
    assert_eq!(ship.name, "Ship it", "the emoji left the name");

    let plain = by_node(b);
    assert_eq!(plain.icon, None, "no emoji, so the document glyph");
    assert_eq!(plain.name, "Plain page");

    // Emoji-only: it is the icon, and the name falls back rather than showing
    // the emoji a second time.
    let only = by_node(c);
    assert_eq!(only.icon.as_deref(), Some("\u{1F525}"));
    assert_eq!(only.name, "Untitled");
}

/// A mid-title emoji is not a page icon — the rule is *leading* only, and it
/// lives in one function so three surfaces cannot disagree about it.
#[test]
fn a_mid_title_emoji_stays_in_the_name() {
    use koru_core::node::Node;
    use koru_core::schema::Parent;

    let mut doc =
        koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").unwrap();
    let d = doc
        .insert(Parent::Root, Node::document("Ship it \u{1F680}").unwrap())
        .unwrap();
    doc.trash(d, 1, None).unwrap();

    let mut v = TrashView::new();
    v.catch_up(&doc);
    assert_eq!(v.entries[0].icon, None);
    assert_eq!(v.entries[0].name, "Ship it \u{1F680}");
}

/// A peer-to-peer workspace's trash is emptied only by hand, so its rows say
/// when they were deleted and nothing about when they go; a workspace whose
/// trash empties by itself says both.
#[test]
fn only_a_trash_that_empties_by_itself_says_when_it_removes() {
    let mut v = view(1);
    let day = koru_core::trash_expiry::DAY_MS;
    v.now_ms = 1_700_000_000_000 + 3 * day;
    let stamp = Some(1_700_000_000_000);
    assert_eq!(v.when(stamp), "Deleted 3 days ago");
    assert!(v.set_empties(true));
    assert!(!v.set_empties(true), "an unchanged push costs no frame");
    assert_eq!(v.when(stamp), "Deleted 3 days ago · Removed in 27 days");
}
