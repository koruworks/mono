// SPDX-License-Identifier: MPL-2.0

//! Back and forward through chat tabs (koru-ai D102): a chat tab is a place
//! in the window's history, like a document, a run or a Tasks page.

use koru_ai_ui::ChatId;
use koru_chats_ui::ChatStatus;
use koru_workspace_host::View;

use super::super::harness::*;
use super::*;

/// The chat in front, if the tab in front is a chat.
fn chat_in_front(v: &MainView) -> Option<ChatId> {
    match &v.tabs().active_tab().view {
        TabView::AiChat(view) => view.chat_id(),
        _ => None,
    }
}

fn front_chat_view(v: &mut MainView) -> &mut crate::ai_chat_view::AiChatView {
    match v.tabs_mut().active_view() {
        TabView::AiChat(view) => view,
        _ => panic!("a chat tab in front"),
    }
}

/// **Back and Forward walk chat → chat → document and back**, as the thumb
/// buttons and `Alt`+←/→ (`⌘[`/`⌘]`) do — the shell hands the window the
/// same direction for either. Chats are reached the way the Chats list and
/// `Ctrl+Shift+↑`/`↓` reach them, and the document the way a reply's link
/// lands; each chat comes back at the scroll it was left at.
#[test]
fn back_and_forward_walk_chat_chat_document_and_back() {
    let (mut v, mut w) = container();
    let first = a_document(&mut w.ws, "Brief");
    let linked = a_document(&mut w.ws, "Linked from a reply");
    v.open_node(&w.ws, first);
    let (a, b) = (ChatId::new(), ChatId::new());
    assert!(v.open_ai_chat_tab_for(a));
    front_chat_view(&mut v).restore_conversation_scroll(80.0);
    assert!(v.open_ai_chat_tab_for(b));
    assert!(v.open_node(&w.ws, linked), "the reply's link lands");
    assert_eq!(v.document_root(), Some(linked));

    assert!(v.navigate_history(&w.ws, -1));
    assert_eq!(
        chat_in_front(&v),
        Some(b),
        "back to the chat the link was in"
    );
    assert!(v.navigate_history(&w.ws, -1));
    assert_eq!(chat_in_front(&v), Some(a));
    assert_eq!(front_chat_view(&mut v).conversation_scroll(), 80.0);
    assert!(v.navigate_history(&w.ws, -1));
    assert_eq!(v.document_root(), Some(first), "and the page before them");

    assert!(v.navigate_history(&w.ws, 1));
    assert_eq!(chat_in_front(&v), Some(a));
    assert!(v.navigate_history(&w.ws, 1));
    assert_eq!(chat_in_front(&v), Some(b));
    assert!(v.navigate_history(&w.ws, 1));
    assert_eq!(v.document_root(), Some(linked));
    assert!(!v.navigate_history(&w.ws, 1), "nothing beyond the newest");
}

/// **A chat whose tab was closed comes back in a tab of its own**, at the
/// scroll it was left at — the forgiveness a closed document tab is shown.
#[test]
fn back_to_a_chat_whose_tab_was_closed_reopens_it_where_it_was_left() {
    let (mut v, mut w) = container();
    let page = a_document(&mut w.ws, "Page");
    let chat = ChatId::new();
    assert!(v.open_ai_chat_tab_for(chat));
    front_chat_view(&mut v).restore_conversation_scroll(140.0);
    let chat_tab = v.tabs().active();
    assert!(v.open_node(&w.ws, page));
    assert!(v.close(chat_tab));
    assert!(
        !v.tabs()
            .iter()
            .any(|tab| matches!(&tab.view, TabView::AiChat(_)))
    );

    assert!(v.navigate_history(&w.ws, -1));
    assert_eq!(chat_in_front(&v), Some(chat));
    assert_eq!(front_chat_view(&mut v).conversation_scroll(), 140.0);
    assert!(v.navigate_history(&w.ws, 1));
    assert_eq!(v.document_root(), Some(page));
}

/// **Opening the chat already in front is standing still**, not a stop: a
/// second press on its row leaves Back meaning what it meant.
#[test]
fn opening_the_chat_in_front_records_nothing() {
    let (mut v, mut w) = container();
    let page = a_document(&mut w.ws, "Page");
    v.open_node(&w.ws, page);
    let chat = ChatId::new();
    assert!(v.open_ai_chat_tab_for(chat));
    let depth = v.history.len();
    assert!(v.open_ai_chat_tab_for(chat));
    assert_eq!(v.history.len(), depth);
    assert!(v.navigate_history(&w.ws, -1));
    assert_eq!(v.document_root(), Some(page));
}

/// **A message being written stays with its chat** across Back and
/// Forward, with the keyboard in the composer when the step was asked for:
/// each chat tab holds its own composer, and a step only brings a tab
/// forward.
#[test]
fn a_draft_in_the_composer_stays_with_its_chat_across_steps() {
    let (mut v, mut w) = container();
    let page = a_document(&mut w.ws, "Page");
    v.open_node(&w.ws, page);
    let chat = ChatId::new();
    assert!(v.open_ai_chat_tab_for(chat));
    for letter in ["h", "i"] {
        View::key(
            front_chat_view(&mut v),
            &mut w.ws,
            &ch(letter),
            Some(letter),
            winit::keyboard::ModifiersState::empty(),
        );
    }
    assert_eq!(front_chat_view(&mut v).composer_text(), "hi");

    assert!(v.navigate_history(&w.ws, -1), "a step from the composer");
    assert_eq!(v.document_root(), Some(page));
    assert!(v.navigate_history(&w.ws, 1));
    assert_eq!(chat_in_front(&v), Some(chat));
    assert_eq!(front_chat_view(&mut v).composer_text(), "hi");
}

/// **A running reply is never stopped by a step**: the chat's tab goes
/// behind, keeps working there — its mark says so — and is in front and
/// still working when Forward brings it back.
#[test]
fn a_step_away_from_a_running_reply_leaves_it_running() {
    let (mut v, mut w) = container();
    let page = a_document(&mut w.ws, "Page");
    v.open_node(&w.ws, page);
    let chat = ChatId::new();
    assert!(v.open_ai_chat_tab_for(chat));
    front_chat_view(&mut v).pretend_reply_running();

    assert!(v.navigate_history(&w.ws, -1));
    assert_eq!(v.document_root(), Some(page));
    v.maintain_chat_status();
    let behind = v
        .tabs()
        .iter()
        .find_map(|tab| match &tab.view {
            TabView::AiChat(view) if view.chat_id() == Some(chat) => {
                Some((view.is_working(), view.status()))
            }
            _ => None,
        })
        .expect("the chat's tab is still open");
    assert_eq!(behind, (true, ChatStatus::Working));

    assert!(v.navigate_history(&w.ws, 1));
    assert_eq!(chat_in_front(&v), Some(chat));
    assert!(front_chat_view(&mut v).is_working());
}

/// **A chat link or a search hit opens the chat as a tab** (the retired
/// standalone window's door, 2026-11-03): at its block, in a new tab the
/// first time, in the tab already showing it after that — and an id that
/// is not a uuid opens nothing.
#[test]
fn a_chat_link_opens_its_chat_as_a_tab_and_finds_it_again() {
    let (mut v, mut w) = container();
    let page = a_document(&mut w.ws, "Page");
    v.open_node(&w.ws, page);
    let chat = ChatId::new();
    let block = koru_ai_ui::MessageBlockId::new();
    let chat_tabs = |v: &MainView| {
        v.tabs()
            .iter()
            .filter(|tab| matches!(&tab.view, TabView::AiChat(_)))
            .count()
    };

    assert!(View::open_chat_tab(
        &mut v,
        &chat.to_string(),
        Some(&block.to_string())
    ));
    assert_eq!(chat_in_front(&v), Some(chat));
    assert_eq!(chat_tabs(&v), 1);

    assert!(v.open_node(&w.ws, page));
    assert!(View::open_chat_tab(&mut v, &chat.to_string(), None));
    assert_eq!(chat_in_front(&v), Some(chat), "the open tab comes forward");
    assert_eq!(chat_tabs(&v), 1, "and no second tab opens");

    assert!(!View::open_chat_tab(&mut v, "not-a-chat", None));
    assert!(!View::open_chat_tab(
        &mut v,
        &chat.to_string(),
        Some("not-a-block")
    ));
}

/// **A chat of another profile is never stepped back into** (AI schema 13):
/// a stop made before the window switched profile names a chat of the old
/// one, which does not open here — Back steps over it to the page before.
#[test]
fn back_steps_over_a_chat_of_the_profile_the_window_left() {
    let (mut v, mut w) = container();
    let workspace = w.ws.id().expect("workspace id");
    let first = a_document(&mut w.ws, "Brief");
    let linked = a_document(&mut w.ws, "Linked from a reply");
    v.open_node(&w.ws, first);
    let chat = ChatId::new();
    assert!(v.open_ai_chat_tab_for(chat));
    assert!(v.open_node(&w.ws, linked));

    View::set_pane_context(
        &mut v,
        Vec::new(),
        koru_core::ProfileId::new(),
        Vec::new(),
        Some(workspace),
    );
    assert!(v.navigate_history(&w.ws, -1));
    assert_ne!(chat_in_front(&v), Some(chat), "not the old profile's chat");
    assert_eq!(v.document_root(), Some(first), "the page before it");
}
