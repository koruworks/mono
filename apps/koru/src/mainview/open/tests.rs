// SPDX-License-Identifier: MPL-2.0

//! Explore reuses a working tab; a pin's document does not change.

use crate::panes::Outcome;
use koru_editor::{Editing, OpenWhere};
use koru_workspace_host::View;

use crate::mainview::harness::*;
use crate::mainview::tab::{Tab, TabView};
use crate::mainview::tab_rules::KoruTabs;

#[test]
fn explore_with_active_ai_chat_tab_attaches_context_without_replacing() {
    let (mut v, mut w) = container();
    v.tabs_mut().push(Tab::ai_chat());
    let last = v.tabs().len() - 1;
    v.tabs_mut().select(last);
    let doc = a_document(&mut w.ws, "Notes");
    let before = v.tabs().len();

    assert!(v.apply(&w.ws, Outcome::OpenNode(doc)));

    assert_eq!(v.tabs().len(), before);
    assert!(
        matches!(v.tabs_mut().active_view(), TabView::AiChat(_)),
        "the AI chat tab stayed active"
    );
}

/// **Welcome is a dedicated tab.** A document opened while it is showing
/// opens in a new tab beside it; the Welcome page is still there.
#[test]
fn explore_from_welcome_opens_a_new_tab_and_keeps_welcome() {
    let (mut v, mut w) = container();
    let doc = a_document(&mut w.ws, "Notes");
    assert_eq!(v.tabs().len(), 1);

    assert!(v.apply(&w.ws, Outcome::OpenNode(doc)));

    assert_eq!(v.tabs().len(), 2);
    assert_eq!(v.document_root(), Some(doc));
    assert!(
        (0..v.tabs().len())
            .any(|i| matches!(v.tabs().get(i).map(|t| &t.view), Some(TabView::Welcome(_)))),
        "the Welcome tab survived"
    );
}

#[test]
fn store_explore_from_cold_welcome_opens_the_loaded_document() {
    let (mut v, mut w) = container();
    let root = a_document(&mut w.ws, "Notes");
    let document = koru_core::DocumentId::new();
    let row = koru_core::NodeId::from_uuid(document.as_uuid());
    View::set_entries(
        &mut v,
        vec![koru_core::explore::ExploreEntry {
            id: document,
            parent: None,
            ordinal: 0,
            title: "Notes".into(),
            kind: koru_core::explore::KIND_DOCUMENT.into(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        }],
        Some(document),
    );

    assert!(v.apply(&w.ws, Outcome::OpenNode(row)));
    assert!(matches!(
        View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::Navigate { .. })
    ));

    // The shell does not reopen or re-announce a document it loaded before
    // restoring Welcome. It resumes the request directly in this view.
    assert!(View::open_search_target(
        &mut v,
        &w.ws,
        Some(row),
        None,
        None,
        None,
    ));

    assert_eq!(
        v.tabs().len(),
        2,
        "Welcome stays; the document opens beside it"
    );
    assert_eq!(v.document_root(), Some(root));
    assert!(v.pending_lands.is_empty());
}

#[test]
fn explore_replaces_the_active_document_tab() {
    let (mut v, mut w) = on_document();
    let brief = a_document(&mut w.ws, "Brief");
    let before = v.tabs().len();

    assert!(v.apply(&w.ws, Outcome::OpenNode(brief)));

    assert_eq!(v.tabs().len(), before);
    assert_eq!(v.document_root(), Some(brief));
}

#[test]
fn explore_focuses_a_tab_that_already_shows_the_document() {
    let (mut v, mut w) = on_document();
    let brief = a_document(&mut w.ws, "Brief");
    v.open_node(&w.ws, brief);
    let after_open = v.tabs().len();
    v.select(0);

    assert!(v.apply(&w.ws, Outcome::OpenNode(brief)));

    assert_eq!(v.tabs().len(), after_open);
    assert_eq!(v.document_root(), Some(brief));
}

#[test]
fn explore_from_a_pin_with_no_main_tab_mints_one() {
    let (mut v, mut w) = on_document();
    let notes = v.document_root().expect("notes");
    assert!(pin_active_document(&mut v));
    let welcome = v
        .tabs()
        .iter()
        .position(|t| matches!(t.view, TabView::Welcome(_)))
        .expect("welcome remains after pin");
    assert!(v.tabs_mut().close(welcome));
    assert_eq!(v.tabs().main_count(), 0);
    let brief = a_document(&mut w.ws, "Brief");

    assert!(v.apply(&w.ws, Outcome::OpenNode(brief)));

    assert_eq!(v.tabs().pinned_count(), 1);
    assert_eq!(v.tabs().main_count(), 1);
    assert_eq!(v.document_root(), Some(brief));
    assert_eq!(
        v.tabs().get(0).and_then(|t| t.view.root()),
        Some(notes),
        "the pin did not change"
    );
}

#[test]
fn explore_from_a_pin_reuses_the_last_main_tab() {
    let (mut v, mut w) = on_document();
    let notes = v.document_root().expect("notes");
    assert!(pin_active_document(&mut v));
    let brief = a_document(&mut w.ws, "Brief");
    v.open_node(&w.ws, brief);
    let before = v.tabs().len();
    v.select(0);
    assert!(v.tabs().active_tab().is_pinned());
    let other = a_document(&mut w.ws, "Other");

    assert!(v.apply(&w.ws, Outcome::OpenNode(other)));

    assert_eq!(v.tabs().len(), before);
    assert_eq!(v.document_root(), Some(other));
    assert_eq!(v.tabs().get(0).and_then(|t| t.view.root()), Some(notes));
}

#[test]
fn explore_does_not_replace_a_draft() {
    let (mut v, mut w) = container();
    v.tabs_mut().push(Tab::draft(None).expect("draft"));
    assert!(v.tabs().active_tab().draft.is_some());
    let before = v.tabs().len();
    let brief = a_document(&mut w.ws, "Brief");

    assert!(v.apply(&w.ws, Outcome::OpenNode(brief)));

    assert!(
        v.tabs().iter().any(|t| t.draft.is_some()),
        "the draft is still there"
    );
    assert_eq!(v.tabs().len(), before + 1);
    assert_eq!(v.document_root(), Some(brief));
}

#[test]
fn background_tab_from_a_pin_lands_in_main_and_keeps_the_pin_showing() {
    let (mut v, mut w) = on_document();
    let notes = v.document_root().expect("notes");
    assert!(pin_active_document(&mut v));
    let brief = a_document(&mut w.ws, "Brief");

    assert!(v.open_node_in_background_tab(&w.ws, brief));

    assert!(
        v.tabs().active_tab().is_pinned(),
        "Ctrl+click must not leave the pin"
    );
    assert_eq!(v.document_root(), Some(notes));
    assert_eq!(v.tabs().pinned_count(), 1);
    assert!(
        v.tabs()
            .iter()
            .any(|t| t.view.root() == Some(brief) && !t.is_pinned()),
        "the opened document is a Main tab, not a second pin"
    );
}

#[test]
fn same_tab_on_a_pin_opens_a_new_main_tab() {
    let (mut v, mut w) = on_document();
    let notes = v.document_root().expect("notes");
    assert!(pin_active_document(&mut v));
    let brief = a_document(&mut w.ws, "Brief");

    assert!(v.open_node_here(&w.ws, brief));

    assert_eq!(v.document_root(), Some(brief));
    assert!(!v.tabs().active_tab().is_pinned());
    assert_eq!(v.tabs().get(0).and_then(|t| t.view.root()), Some(notes));
}

/// Production names the store document on the pin. SameTab used to look that
/// id up and conclude the pin was already the target, so every pill after
/// pinning — including Ctrl+click once Jump List had cleared the Ctrl bit —
/// was a no-op. The in-memory suite usually has `active_document = None`,
/// which is why `same_tab_on_a_pin_opens_a_new_main_tab` did not catch it.
#[test]
fn a_same_tab_pill_on_a_pin_does_not_treat_the_pin_as_the_target() {
    let (mut v, mut w) = on_document();
    let notes = v.document_root().expect("notes");
    assert!(pin_active_document(&mut v));
    v.active_document = v.tabs().active_tab().document;
    let brief = a_document(&mut w.ws, "Brief");
    let TabView::Leaf(leaf) = v.active_view() else {
        panic!("document leaf");
    };
    leaf.view_mut().wants_open = Some((brief, OpenWhere::SameTab));

    assert!(v.drain_wants_open(&w.ws));

    assert_eq!(v.document_root(), Some(brief));
    assert!(!v.tabs().active_tab().is_pinned());
    assert_eq!(v.tabs().get(0).and_then(|t| t.view.root()), Some(notes));
}

#[test]
fn same_tab_on_a_pin_focuses_a_main_tab_already_showing_it() {
    let (mut v, mut w) = on_document();
    assert!(pin_active_document(&mut v));
    let brief = a_document(&mut w.ws, "Brief");
    v.open_node(&w.ws, brief);
    let before = v.tabs().len();
    v.select(0);
    assert!(v.tabs().active_tab().is_pinned());

    assert!(v.open_node_here(&w.ws, brief));

    assert_eq!(v.tabs().len(), before);
    assert_eq!(v.document_root(), Some(brief));
    assert!(!v.tabs().active_tab().is_pinned());
}

#[test]
fn replace_this_tab_on_a_pin_does_not_change_the_pin() {
    let (mut v, mut w) = on_document();
    let notes = v.document_root().expect("notes");
    assert!(pin_active_document(&mut v));
    let brief = a_document(&mut w.ws, "Brief");

    assert!(v.replace_this_tab(&w.ws, brief, None));

    assert_eq!(v.tabs().get(0).and_then(|t| t.view.root()), Some(notes));
    assert!(v.tabs().get(0).is_some_and(|t| t.is_pinned()));
    assert_eq!(v.document_root(), Some(brief));
    assert!(!v.tabs().active_tab().is_pinned());
}

#[test]
fn a_same_tab_pill_on_a_reusable_tab_still_replaces() {
    let (mut v, mut w) = container();
    let a = a_document(&mut w.ws, "A");
    let b = a_document(&mut w.ws, "B");
    v.open_node(&w.ws, a);
    let before = v.tabs().len();

    assert!(v.open_node_here(&w.ws, b));

    assert_eq!(v.tabs().len(), before);
    assert_eq!(v.document_root(), Some(b));
}

#[test]
fn a_routed_same_tab_pill_on_a_pin_asks_for_a_new_tab() {
    let (mut v, _w) = on_document();
    assert!(pin_active_document(&mut v));
    let target = koru_editor::trigger::DocumentTarget {
        document: koru_core::DocumentId::new(),
        node: None,
    };
    let TabView::Leaf(leaf) = v.active_view() else {
        panic!("document leaf");
    };
    leaf.view_mut().wants_entity_open = Some((target, OpenWhere::SameTab));

    assert!(v.sync_entity_picker());
    assert_eq!(
        v.pending_lands.front().copied(),
        Some((target.document, OpenWhere::NewTab))
    );
}

#[test]
fn a_draft_is_not_reusable() {
    let tab = Tab::draft(None).expect("draft");
    assert!(!tab.is_reusable());
    assert!(!tab.is_pinned());
}

fn a_session_tab() -> Tab {
    let (sender, _commands) = koru_session_backend::channel();
    let view =
        crate::session_tab::SessionView::new(&crate::session_tab::SessionSender(sender), None);
    Tab::session(view, "Claude")
}

#[test]
fn a_session_tab_is_not_reusable() {
    let tab = a_session_tab();
    assert!(
        !tab.is_reusable(),
        "an agent is a running process, not a view"
    );
    assert!(!tab.is_pinned());
}

/// Clicking a document while an agent tab is active opens the document in a
/// tab of its own; the agent tab is not replaced, renamed or re-pointed.
#[test]
fn explore_with_an_active_agent_tab_opens_a_new_tab_and_keeps_the_agent() {
    let (mut v, mut w) = on_document();
    let brief = a_document(&mut w.ws, "Brief");
    v.tabs_mut().push(a_session_tab());
    let agent = v.tabs().active();
    let before = v.tabs().len();
    assert!(matches!(v.tabs().active_tab().view, TabView::Session(_)));

    assert!(v.apply(&w.ws, Outcome::OpenNode(brief)));

    assert!(
        matches!(
            v.tabs().get(agent).map(|t| &t.view),
            Some(TabView::Session(_))
        ),
        "the agent tab is still an agent tab, in its place"
    );
    assert_eq!(
        v.tabs().get(agent).map(|t| t.label.clone()),
        Some("Claude".into())
    );
    assert_eq!(
        v.document_root(),
        Some(brief),
        "the document is what is showing"
    );
    assert!(
        v.tabs().len() >= before,
        "no tab was consumed: {} -> {}",
        before,
        v.tabs().len()
    );
}
