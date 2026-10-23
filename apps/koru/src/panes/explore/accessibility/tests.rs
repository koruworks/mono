// SPDX-License-Identifier: MPL-2.0

//! What the column publishes, and what it does when asked.
//!
//! The shell's own tests (`mainview/accessibility/tests.rs`) drive a whole
//! window; these drive the pane alone, which is what makes the **language**
//! assertion possible at all: [`Context`] carries a `Lang`, so two real runs
//! can be compared. The window's language comes from the OS through a
//! `OnceLock` and cannot be changed inside a process.

use accesskit::{Action, Role};
use koru_core::explore::{ExploreEntry, KIND_DOCUMENT};
use koru_core::{DocumentId, NodeId};
use koru_draw::geometry::Rect;
use koru_i18n::Lang;
use koru_workspace_host::accessibility::{ControlPart, Occurrence, ViewSlot, WindowSlot};

use super::{Context, SemanticNode};
use crate::panes::explore::{ExploreView, NavigatorMode};

const WINDOW: WindowSlot = WindowSlot::from_raw(1);
const VIEW: ViewSlot = ViewSlot::from_raw(1);

fn cx(lang: Lang) -> Context {
    Context {
        window: WINDOW,
        view: VIEW,
        area: Rect::new(0.0, 0.0, 260.0, 700.0),
        viewport: (1200.0, 700.0),
        lang,
    }
}

fn entry(title: &str, ordinal: u32) -> ExploreEntry {
    ExploreEntry {
        id: DocumentId::new(),
        parent: None,
        ordinal,
        title: title.to_string(),
        kind: KIND_DOCUMENT.to_string(),
        mime: None,
        note: None,
        trashed_at: None,
        trashed_by: None,
    }
}

/// A pane over a listing, the way a store-backed workspace fills it.
fn pane(titles: &[&str]) -> (ExploreView, Vec<NodeId>) {
    let entries: Vec<ExploreEntry> = titles
        .iter()
        .enumerate()
        .map(|(i, title)| entry(title, i as u32))
        .collect();
    let ids = entries
        .iter()
        .map(|entry| NodeId::from_uuid(entry.id.as_uuid()))
        .collect();
    let mut view = ExploreView::default();
    view.rebuild_from_entries(&entries);
    (view, ids)
}

fn flatten(node: &SemanticNode, out: &mut Vec<SemanticNode>) {
    out.push(node.clone());
    for child in &node.children {
        flatten(child, out);
    }
}

fn nodes(view: &ExploreView, cx: &Context) -> Vec<SemanticNode> {
    let mut out = Vec::new();
    flatten(&view.semantics(cx), &mut out);
    out
}

fn locators(view: &ExploreView, cx: &Context) -> Vec<String> {
    nodes(view, cx)
        .iter()
        .map(|node| node.occurrence.locator())
        .collect()
}

fn row(id: NodeId) -> Occurrence {
    Occurrence::model(WINDOW, VIEW, id.as_uuid(), ControlPart::ROOT)
}

// ------------------------------------------------------------------ identity

/// **The property the roadmap gates on**, run rather than argued: the same
/// pane, projected in all eight languages, publishes byte-identical locators
/// and different labels.
#[test]
fn a_locator_is_the_same_in_every_language() {
    let (view, _) = pane(&["Brief", "Minutes"]);
    let english = locators(&view, &cx(Lang::En));
    let mut labels = Vec::new();
    for lang in Lang::ALL {
        assert_eq!(
            locators(&view, &cx(lang)),
            english,
            "a locator changed in {lang:?}"
        );
        labels.push(
            nodes(&view, &cx(lang))
                .into_iter()
                .filter_map(|node| node.label)
                .collect::<Vec<_>>(),
        );
    }
    // …and the labels really did move, or the assertion above proves nothing.
    assert_ne!(
        labels[0], labels[3],
        "English and German published the same words, so nothing was translated"
    );
}

#[test]
fn a_row_keeps_its_identity_through_a_rename_and_a_reorder() {
    let mut entries = vec![entry("Brief", 0), entry("Minutes", 1)];
    let watched = NodeId::from_uuid(entries[0].id.as_uuid());
    let mut view = ExploreView::default();
    view.rebuild_from_entries(&entries);
    let before = row(watched).locator();
    assert!(locators(&view, &cx(Lang::En)).contains(&before));

    entries[0].title = "Brief (final)".to_string();
    entries.swap(0, 1);
    entries[0].ordinal = 0;
    entries[1].ordinal = 1;
    view.rebuild_from_entries(&entries);

    let after = locators(&view, &cx(Lang::En));
    assert!(
        after.contains(&before),
        "a rename and a reorder moved a surviving row's identity"
    );
}

// --------------------------------------------------------------- structure

#[test]
fn a_row_says_where_it_is_and_whether_it_is_open() {
    let (mut view, ids) = pane(&["Brief", "Minutes", "Notes"]);
    view.set_selected(Some(ids[1]));
    let published = nodes(&view, &cx(Lang::En));
    let rows: Vec<&SemanticNode> = published
        .iter()
        .filter(|node| node.role == Role::TreeItem)
        .collect();
    assert_eq!(rows.len(), 3);
    for (i, node) in rows.iter().enumerate() {
        // Zero-based: the Windows adapter adds the one a reader hears.
        assert_eq!(node.position, Some(i), "row {i} miscounted its place");
        assert_eq!(node.level, Some(1), "a top-level row is not at level 1");
    }
    // …and the total is on the container, which is where an item's total is
    // read from.
    let tree = published
        .iter()
        .find(|node| node.role == Role::Tree)
        .expect("the tree is published");
    assert_eq!(tree.size_of_set, Some(3));

    // **Only the open row carries `selected`.** A node that carries it loses
    // its Invoke pattern, so publishing `false` on every row would leave a
    // client unable to open any of them.
    assert_eq!(
        rows[1].selected,
        Some(true),
        "the open document did not say it was open"
    );
    assert_eq!(rows[0].selected, None, "a closed row claimed the concept");
    for (i, node) in rows.iter().enumerate() {
        if i != 1 {
            assert!(
                node.actions.contains(&Action::Click),
                "row {i} offers no way to be opened"
            );
        }
    }
}

#[test]
fn a_folded_row_says_so_and_offers_the_way_open() {
    let parent = entry("Section", 0);
    let mut child = entry("Inside", 1);
    child.parent = Some(parent.id);
    let parent_node = NodeId::from_uuid(parent.id.as_uuid());
    let mut view = ExploreView::default();
    view.rebuild_from_entries(&[parent.clone(), child.clone()]);

    let expanded = nodes(&view, &cx(Lang::En))
        .into_iter()
        .find(|node| node.occurrence == row(parent_node))
        .expect("the parent is published");
    assert!(expanded.actions.contains(&Action::Collapse));
    // **No `expanded` flag.** Setting it would cost the row its Invoke
    // pattern, and `accesskit_windows` 0.32.1 has no ExpandCollapse pattern to
    // replace it with — so a folded document would be readable and
    // unopenable. The fold *actions* are still published and routed.
    assert!(
        expanded.actions.contains(&Action::Click),
        "a row with children cannot be opened"
    );

    // Fold it the way the left arrow does, and the projection follows.
    let area = cx(Lang::En).area;
    assert!(
        view.accessibility_action(area, row(parent_node), Action::Collapse, (1200.0, 700.0))
            .is_some()
    );
    // The container rebuilds the row cache after a fold — `MainView::key`'s
    // explore-nav arm sets `explore_stale`, and so does the accessibility
    // route. Standing the pane up alone, that step is this line.
    view.rebuild_from_entries(&[parent.clone(), child.clone()]);
    let folded = nodes(&view, &cx(Lang::En))
        .into_iter()
        .find(|node| node.occurrence == row(parent_node))
        .expect("the parent is still published");
    assert!(folded.actions.contains(&Action::Expand));
    assert!(folded.actions.contains(&Action::Click));
    // …and the child it was hiding is gone from the tree, because it is gone
    // from the rows.
    assert!(
        !locators(&view, &cx(Lang::En))
            .contains(&row(NodeId::from_uuid(child.id.as_uuid())).locator())
    );
}

#[test]
fn the_projection_buttons_are_named_and_one_is_selected() {
    let (view, _) = pane(&["Brief"]);
    let published = nodes(&view, &cx(Lang::En));
    let modes: Vec<&SemanticNode> = published
        .iter()
        .filter(|node| node.role == Role::Tab)
        .collect();
    assert_eq!(modes.len(), NavigatorMode::ALL.len());
    assert_eq!(
        modes
            .iter()
            .filter(|node| node.selected == Some(true))
            .count(),
        1,
        "the strip of projections has no single current one"
    );
    // The identity is the stored token, which is neither translated nor a
    // position.
    for mode in NavigatorMode::ALL {
        assert!(
            published
                .iter()
                .any(|node| node.occurrence.part.name() == mode.token()),
            "no button for {mode:?}"
        );
    }
}

#[test]
fn narrow_heading_publishes_only_controls_with_pointer_geometry() {
    let (view, _) = pane(&["Brief"]);
    // The modes left the heading band for the apps row, so at every width
    // the column can take both heading controls have their room.
    for width in [256., 300., 400.] {
        let mut context = cx(Lang::En);
        context.area.w = width;
        let heading = view.heading_semantics(&context);
        assert!(
            heading
                .iter()
                .all(|node| node.bounds.is_some_and(|r| r.w > 0. && r.h > 0.))
        );
        assert_eq!(heading.len(), 2);
    }
    // Sized from the shown modes (QUESTIONS #5): with only Documents and
    // Files, the narrowest column still has room for both lanes.
    koru_features::testing::with(koru_features::Features::ALL_OFF, || {
        let (view, _) = pane(&["Brief"]);
        for width in [180., 232., 280.] {
            let mut context = cx(Lang::En);
            context.area.w = width;
            let heading = view.heading_semantics(&context);
            assert!(
                heading
                    .iter()
                    .all(|node| node.bounds.is_some_and(|r| r.w > 0. && r.h > 0.))
            );
            assert_eq!(heading.len(), 2, "at {width}");
        }
    });
}

// -------------------------------------------------------------------- focus

#[test]
fn focus_is_the_row_the_ring_is_on() {
    let (mut view, ids) = pane(&["Brief", "Minutes"]);
    let cx = cx(Lang::En);
    assert_eq!(
        view.accessible_focus(&cx),
        None,
        "an unfocused tree claimed focus"
    );

    assert!(view.focus_tree(cx.area));
    assert_eq!(
        view.accessible_focus(&cx),
        Some(row(view.focused().expect("focused"))),
    );

    // A client's focus request moves the same ring.
    assert!(
        view.accessibility_action(cx.area, row(ids[1]), Action::Focus, cx.viewport)
            .is_some()
    );
    assert_eq!(view.focused(), Some(ids[1]));
    assert_eq!(view.accessible_focus(&cx), Some(row(ids[1])));
}

// ---------------------------------------------------------------- geometry

#[test]
fn every_row_is_published_where_the_pane_puts_it() {
    let titles: Vec<String> = (0..80).map(|i| format!("doc {i}")).collect();
    let refs: Vec<&str> = titles.iter().map(String::as_str).collect();
    let (view, ids) = pane(&refs);
    let cx = cx(Lang::En);
    let published = nodes(&view, &cx);
    for (i, id) in ids.iter().enumerate() {
        let node = published
            .iter()
            .find(|node| node.occurrence == row(*id))
            .unwrap_or_else(|| panic!("row {i} was not published"));
        assert_eq!(
            node.bounds,
            Some(view.row_band(cx.area, i)),
            "row {i} is published somewhere other than where it is drawn"
        );
    }
    // The culled view and the published one agree wherever they overlap, which
    // is the property that stops the two arithmetics drifting.
    for (i, rect) in view.row_rects(cx.area) {
        assert_eq!(view.row_band(cx.area, i), rect);
    }
}

// --------------------------------------------------------------------- cost

#[test]
fn scrolling_changes_the_geometry_and_not_the_revision() {
    let titles: Vec<String> = (0..80).map(|i| format!("doc {i}")).collect();
    let refs: Vec<&str> = titles.iter().map(String::as_str).collect();
    let (mut view, _) = pane(&refs);
    let cx = cx(Lang::En);

    let mut before = koru_workspace_host::accessibility::Revision::new();
    view.revise(&mut before);
    let first = view.row_band(cx.area, 0);

    view.scroll_by(cx.area, -300.0);

    let mut after = koru_workspace_host::accessibility::Revision::new();
    view.revise(&mut after);
    assert_eq!(
        before.finish(),
        after.finish(),
        "scrolling asked for a new tree"
    );
    assert_ne!(
        first,
        view.row_band(cx.area, 0),
        "the fixture did not actually scroll"
    );
}
