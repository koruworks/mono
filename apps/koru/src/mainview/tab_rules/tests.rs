// SPDX-License-Identifier: MPL-2.0

//! Koru's tab rules, against the real `Tab` and a real `WorkspaceDoc`.
//!
//! **These stayed behind when the machinery left.** Every one of them names a
//! `NodeId`, a `DocumentId` or a `WorkspaceDoc`, which is exactly what
//! `koru-app-shell` may not — so the generic invariants (the active index, the
//! floor, the bulk closes, the reorder) are tested there against a double, and
//! what a Koru tab *means* is tested here.

use super::*;
use crate::leaf::Leaf;
use koru_app_shell::TabStrip;
use koru_core::WorkspaceId;

fn strip(labels: &[&str]) -> TabStrip<Tab> {
    let mut s = TabStrip::new(Tab::welcome(), Tab::welcome);
    let tabs: Vec<Tab> = labels
        .iter()
        .map(|name| {
            let mut t = Tab::welcome();
            t.label = (*name).to_string();
            t
        })
        .collect();
    s.restore_main(tabs, 0);
    s
}

// ------------------------------------------------------ the four predicates

/// A welcome tab is the floor — a placeholder — but a dedicated one: nothing
/// opened replaces it. Like every tab it may be pinned.
#[test]
fn a_welcome_tab_is_the_placeholder_and_may_be_pinned() {
    let tab = Tab::welcome();
    assert!(TabElement::is_placeholder(&tab));
    assert!(!TabElement::is_reusable(&tab));
    assert!(TabElement::pinnable(&tab));
}

/// A draft is not a placeholder — it is content somebody has started — and,
/// like every tab, it may be pinned.
#[test]
fn a_draft_is_not_a_placeholder_and_may_be_pinned() {
    let tab = Tab::draft(None).expect("a draft");
    assert!(!TabElement::is_placeholder(&tab));
    assert!(TabElement::pinnable(&tab));
    assert!(!TabElement::is_reusable(&tab));
}

/// **Every tab may be pinned** — a task list, the Trash, Settings — which is
/// the rule the tab menu's *Pin tab* row reads; it used to be documents and
/// chats only, and a task list had no way to stay at the left of the strip.
#[test]
fn every_kind_of_tab_may_be_pinned() {
    use crate::panes::explore::rail::{Bucket, RailTarget};
    for tab in [
        Tab::tasks(RailTarget::Bucket(Bucket::Today)),
        Tab::tasks(RailTarget::List(koru_core::ids::ListId::new())),
        Tab::trash(),
        Tab::settings(),
        Tab::manage(),
    ] {
        assert!(TabElement::pinnable(&tab), "{}", tab.label);
    }
}

/// A document tab is what a pin is made of.
#[test]
fn a_document_tab_is_pinnable_and_not_a_placeholder() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").expect("create");
    let doc = koru_workspace::document(&mut ws, "Brief").expect("document");
    let tab = Tab::document_of(Leaf::document(&ws, doc), "Brief", Some(DocumentId::new()));
    assert!(TabElement::pinnable(&tab));
    assert!(!TabElement::is_placeholder(&tab));
}

/// The scope crosses the boundary as sixteen bytes and comes back a
/// `WorkspaceId` — the `koru-link` precedent, and the reason no shell type is
/// generic over an id.
#[test]
fn a_workspace_scope_survives_the_round_trip_through_the_shell() {
    let home = WorkspaceId::new();
    let mut s = strip(&[]);
    let mut tab = Tab::welcome();
    tab.document = Some(DocumentId::new());
    tab.label = "Notes".into();
    s.push(tab);
    assert!(s.pin(1, home.as_uuid(), false));
    assert_eq!(s.get(0).expect("the pin").pinned, Some(home));
    assert_eq!(s.get(0).expect("the pin").home, Some(home));
}

/// A pinned Welcome tab is one somebody asked to keep, so it stops being the
/// placeholder the strip drops or lets opened content replace.
#[test]
fn a_pinned_welcome_is_not_a_placeholder() {
    let mut s = strip(&[]);
    assert!(s.pin(0, WorkspaceId::new().as_uuid(), false));
    assert_eq!(s.pinned_count(), 1);
    assert!(!s.has_welcome(), "the pin is not the floor");
}

// ------------------------------------------------------------------ lookups

#[test]
fn a_document_is_found_by_the_node_it_shows() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").expect("create");
    let doc = koru_workspace::document(&mut ws, "Brief").expect("document");
    let mut s = strip(&["a", "b"]);
    assert_eq!(s.showing(doc), None);

    s.push(Tab::document(Leaf::document(&ws, doc), "Brief"));
    assert_eq!(s.showing(doc), Some(2));
}

/// A tab whose node another peer deleted must not survive.
#[test]
fn a_tab_whose_node_is_gone_is_dropped() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").expect("create");
    let doc = koru_workspace::document(&mut ws, "Brief").expect("document");
    let mut s = strip(&[]);
    s.push(Tab::document(Leaf::document(&ws, doc), "Brief"));
    assert_eq!(s.len(), 2);

    assert!(!s.retain_live(&ws, None), "nothing to drop yet");
    ws.delete(doc).expect("delete");
    assert!(s.retain_live(&ws, None));
    assert_eq!(s.len(), 1);
    assert_eq!(s.active_tab().label, "Welcome");
}

/// **A tab over another document is not judged by this one.**
///
/// `ws` is the *active* document, and a `NodeId` is meaningful only inside the
/// document that minted it — so asking it about a tab over a different document
/// answers "gone" for every row, and this closed all of them. Opening a second
/// document reset the window to one tab, and the strip that reached disk had one
/// tab in it, so the next launch reopened one. Two reports, one line.
#[test]
fn a_tab_over_another_document_is_left_alone() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").expect("create");
    let doc = koru_workspace::document(&mut ws, "Brief").expect("document");
    let here = DocumentId::new();
    let elsewhere = DocumentId::new();

    let mut s = TabStrip::new(Tab::welcome(), Tab::welcome);
    // Both leaves are built over the document in hand — a test cannot open two
    // `LoroDoc`s through a strip — so what tells them apart is exactly what
    // tells them apart in the app: the `DocumentId` the tab carries.
    s.push(Tab::document_of(
        Leaf::document(&ws, doc),
        "Brief",
        Some(here),
    ));
    s.push(Tab::document_of(
        Leaf::document(&ws, doc),
        "Elsewhere",
        Some(elsewhere),
    ));
    assert_eq!(s.len(), 3);

    // The node is gone from *this* document. The tab over this document goes
    // with it; the one over another does not.
    ws.delete(doc).expect("delete");
    assert!(s.retain_live(&ws, Some(here)));
    assert_eq!(s.len(), 2, "it took the other document's tab with it");
    assert!(
        s.iter().any(|t| t.document == Some(elsewhere)),
        "the tab over the other document was closed"
    );
}

/// …and when the *only* tab is the dead one, the floor still holds.
#[test]
fn the_last_tab_going_dead_leaves_a_welcome_tab() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").expect("create");
    let doc = koru_workspace::document(&mut ws, "Brief").expect("document");
    let mut s = TabStrip::new(Tab::welcome(), Tab::welcome);
    s.restore_main(vec![Tab::document(Leaf::document(&ws, doc), "Brief")], 0);

    ws.delete(doc).expect("delete");
    assert!(s.retain_live(&ws, None));
    assert_eq!(s.len(), 1);
    assert_eq!(s.active_tab().label, "Welcome");
}

/// A pin whose document has left the library goes with it; a pin belonging to
/// another workspace is not this library's business and is kept by the caller's
/// own predicate.
#[test]
fn retain_pinned_drops_a_pin_whose_document_is_gone() {
    let home = WorkspaceId::new();
    let kept = DocumentId::new();
    let gone = DocumentId::new();
    let mut s = strip(&[]);
    for id in [kept, gone] {
        let mut tab = Tab::welcome();
        tab.document = Some(id);
        tab.label = "Doc".into();
        let index = s.push(tab);
        assert!(s.pin(index, home.as_uuid(), false));
    }
    assert_eq!(s.pinned_count(), 2);
    assert!(s.retain_pinned(|id| id == kept));
    assert_eq!(s.pinned_count(), 1);
    assert_eq!(s.get(0).expect("the survivor").document, Some(kept));
}

/// The floor and the placeholder are the same tab, under the two names the
/// container and the shell each use for it.
#[test]
fn reset_to_welcome_leaves_exactly_one_welcome_tab() {
    let mut s = strip(&["a", "b", "c"]);
    s.select(2);
    s.reset_to_welcome();
    assert_eq!(s.len(), 1);
    assert_eq!(s.active(), 0);
    assert!(s.has_welcome());
}
