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
