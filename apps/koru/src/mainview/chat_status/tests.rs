// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::mainview::harness::container;

fn tab_status(v: &MainView, chat: ChatId) -> ChatStatus {
    v.tabs()
        .iter()
        .find_map(|tab| match &tab.view {
            TabView::AiChat(view) if view.chat_id() == Some(chat) => Some(view.status()),
            _ => None,
        })
        .expect("a tab on the chat")
}

/// **A look is seen; an arrival nobody looked at is unread until the chat
/// comes into view.** Two chat tabs, the second in front: the first is
/// behind it, and news there lights its tab until it is chosen.
#[test]
fn an_arrival_behind_another_tab_is_unread_until_its_tab_is_chosen() {
    let (mut v, _ws) = container();
    let (behind, front) = (ChatId::new(), ChatId::new());
    assert!(v.open_ai_chat_tab_for(behind));
    let behind_index = v.tabs().active();
    assert!(v.open_ai_chat_tab_for(front));
    let front_index = v.tabs().active();
    v.maintain_chat_status();
    assert!(v.chats_looking.contains(&front));
    assert!(!v.chats_looking.contains(&behind));
    assert!(
        with_book(|book| book.mark(front).is_some()),
        "a look leaves a mark"
    );
    assert_eq!(tab_status(&v, behind), ChatStatus::Idle);

    // A reply ended in the chat behind.
    with_book(|book| book.arrived(behind, epoch_millis(), None));
    v.maintain_chat_status();
    assert_eq!(tab_status(&v, behind), ChatStatus::Unread);
    assert_eq!(tab_status(&v, front), ChatStatus::Idle);

    // Chosen: seen, and the one left behind is seen as it was left.
    v.select(behind_index);
    v.maintain_chat_status();
    assert_eq!(tab_status(&v, behind), ChatStatus::Idle);
    assert!(with_book(|book| !book.is_unread(behind, None)));
    v.select(front_index);
    v.maintain_chat_status();
    assert_eq!(tab_status(&v, behind), ChatStatus::Idle, "still seen");
}

/// **A chat that comes into view unread opens on its NEW divider**, after
/// the message seen last; one that comes into view with nothing new puts
/// its divider away (D84).
#[test]
fn an_unread_chat_coming_into_view_gets_its_divider_from_the_last_seen_message() {
    let (mut v, _ws) = container();
    let (behind, front) = (ChatId::new(), ChatId::new());
    v.open_ai_chat_tab_for(behind);
    let behind_index = v.tabs().active();
    v.open_ai_chat_tab_for(front);
    v.maintain_chat_status();
    let last = MessageId::new();
    with_book(|book| book.arrived(behind, epoch_millis(), Some(last)));
    v.maintain_chat_status();
    // Coming into view, it is asked to open from `last`.
    v.select(behind_index);
    let from = with_book(|book| {
        book.is_unread(behind, None)
            .then(|| book.last_seen(behind))
            .flatten()
    });
    assert_eq!(from, Some(last), "read before the look marks it seen");
    v.maintain_chat_status();
    assert!(with_book(|book| !book.is_unread(behind, None)));
}

/// **Ctrl+Shift+↓ in a chat tab opens the chat below in the Chats list**
/// (D87), as choosing its row does — focusing its tab when one shows it.
#[test]
fn a_step_in_a_chat_tab_opens_the_chat_below_in_the_list() {
    let (mut v, _ws) = container();
    let chat = |n: u128, updated: i64| koru_chats_ui::Chat {
        id: ChatId::from_uuid(uuid::Uuid::from_u128(n)),
        title: format!("Chat {n}"),
        private: false,
        current_model: koru_ai_core::ModelSelection {
            provider: koru_ai_core::AiProviderId::parse("openai").unwrap(),
            requested_model: "gpt-test".into(),
            default_reasoning: koru_ai_core::ReasoningChoice::Default,
        },
        branched_from: None,
        agent_working_folder: None,
        agent_mode: None,
        agent_fast: None,
        pinned_at: None,
        created_at: koru_ai_core::Timestamp::from_millis(1),
        updated_at: koru_ai_core::Timestamp::from_millis(updated),
    };
    let (top, below) = (chat(0xa1, 20), chat(0xa2, 10));
    v.explore
        .chats_mut()
        .take_listing(Ok(koru_chats_ui::Listing {
            chats: vec![below.clone(), top.clone()],
            agents: Vec::new(),
        }));
    assert!(v.open_ai_chat_tab_for(top.id));
    let primary_shift = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER | winit::keyboard::ModifiersState::SHIFT
    } else {
        winit::keyboard::ModifiersState::CONTROL | winit::keyboard::ModifiersState::SHIFT
    };
    let TabView::AiChat(view) = &mut v.tabs_mut().active_mut().view else {
        panic!("a chat tab in front");
    };
    let mut ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("create");
    koru_workspace_host::View::key(
        view,
        &mut ws,
        &winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowDown),
        None,
        primary_shift,
    );
    assert!(v.step_chats());
    let TabView::AiChat(view) = &v.tabs().active_tab().view else {
        panic!("a chat tab in front");
    };
    assert_eq!(view.chat_id(), Some(below.id));
    assert!(!v.step_chats(), "asked once");
}

/// **An idle window asks for no frames**: chat tabs that neither work nor
/// wait leave the status deadline empty, unread or not.
#[test]
fn an_idle_window_asks_the_status_pass_for_no_frames() {
    let (mut v, _ws) = container();
    assert_eq!(v.chat_status_deadline(), None, "no chat at all");
    let (behind, front) = (ChatId::new(), ChatId::new());
    v.open_ai_chat_tab_for(behind);
    v.open_ai_chat_tab_for(front);
    with_book(|book| book.arrived(behind, epoch_millis(), None));
    v.maintain_chat_status();
    assert_eq!(tab_status(&v, behind), ChatStatus::Unread);
    assert_eq!(v.chat_status_deadline(), None, "a dot holds still");
}

/// The book rides the session file as one line, and a window restoring
/// that line — or an empty one — never makes a seen chat unread.
#[test]
fn the_book_rides_the_session_line_and_a_restore_only_adds() {
    let chat = ChatId::new();
    with_book(|book| book.seen(chat, 50, None));
    let line = capture();
    assert!(line.contains(&chat.to_storage()));
    restore("");
    restore(&line);
    assert!(with_book(|book| book.mark(chat).is_some()));
    forget(chat);
    assert!(with_book(|book| book.mark(chat).is_none()));
}
