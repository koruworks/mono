// SPDX-License-Identifier: MPL-2.0

//! The interaction contract, from the keyboard: focus enters on open,
//! Up/Down and Home/End move, Enter opens, Delete asks, Escape gives the
//! keyboard back — and only those keys are claimed.

use winit::keyboard::{Key, NamedKey};

use super::*;
use crate::{Focus, Press};

#[test]
fn only_the_contracts_keys_are_claimed() {
    for (key, press) in [
        (NamedKey::ArrowUp, Press::Up),
        (NamedKey::ArrowDown, Press::Down),
        (NamedKey::Home, Press::Home),
        (NamedKey::End, Press::End),
        (NamedKey::Enter, Press::Enter),
        (NamedKey::Space, Press::Space),
        (NamedKey::Delete, Press::Delete),
        (NamedKey::Escape, Press::Escape),
    ] {
        assert_eq!(Press::of(&Key::Named(key)), Some(press));
    }
    assert_eq!(Press::of(&Key::Character(" ".into())), Some(Press::Space));
    assert_eq!(Press::of(&Key::Character("a".into())), None);
    assert_eq!(Press::of(&Key::Named(NamedKey::Tab)), None);
    assert_eq!(Press::of(&Key::Named(NamedKey::Backspace)), None);
}

/// Focus enters on open — on the chat the host has open, when it has one.
#[test]
fn focus_enters_on_the_open_chat_else_the_newest() {
    let mut surface = three();
    assert!(surface.focus_first(area(), TOP));
    assert_eq!(surface.focus(), Some(Focus::Chat(id(1))));

    let mut surface = three();
    surface.set_current(Some(id(3)));
    surface.focus_first(area(), TOP);
    assert_eq!(surface.focus(), Some(Focus::Chat(id(3))));

    // An empty list has nowhere to land: *New chat* is the heading's.
    let mut surface = showing(Vec::new());
    assert!(!surface.focus_first(area(), TOP));
    assert_eq!(surface.focus(), None);
}

#[test]
fn up_down_home_and_end_move_the_selection() {
    let mut surface = three();
    surface.focus_first(area(), TOP);
    assert!(surface.key(area(), TOP, Press::Down));
    assert_eq!(surface.focused_chat(), Some(id(2)));
    assert!(surface.key(area(), TOP, Press::End));
    assert_eq!(surface.focused_chat(), Some(id(3)));
    // Down at the end stays at the end.
    surface.key(area(), TOP, Press::Down);
    assert_eq!(surface.focused_chat(), Some(id(3)));
    assert!(surface.key(area(), TOP, Press::Home));
    assert_eq!(surface.focused_chat(), Some(id(1)), "Home is the newest");
    surface.key(area(), TOP, Press::Up);
    assert_eq!(surface.focused_chat(), Some(id(1)), "Up at the top stays");
    surface.key(area(), TOP, Press::Down);
    assert_eq!(surface.focused_chat(), Some(id(2)));
}

#[test]
fn enter_opens_and_the_heading_asks_for_a_new_chat() {
    let mut surface = three();
    surface.focus_first(area(), TOP);
    surface.key(area(), TOP, Press::Down);
    assert!(surface.key(area(), TOP, Press::Enter));
    assert_eq!(surface.take_wants_open(), Some(id(2)));
    assert_eq!(surface.take_wants_open(), None, "a one-shot drain");
    assert_eq!(surface.selected(), [id(2)], "Enter picks what it opens");

    // *New chat* is asked for by the host's heading button, not a row.
    surface.ask_new();
    assert!(surface.take_wants_new());
    assert!(!surface.take_wants_new());
}

/// **Delete asks; it does not delete.** Nothing leaves the list until the
/// host has confirmed and called `delete`.
#[test]
fn delete_asks_and_deletes_nothing() {
    let mut surface = three();
    surface.focus_first(area(), TOP);
    assert!(surface.key(area(), TOP, Press::Delete));
    assert_eq!(surface.take_wants_delete(), [id(1)]);
    assert_eq!(surface.chats().len(), 3);
    // With the keyboard nowhere there is nothing to delete, and the key is
    // not ours.
    surface.clear_focus();
    assert!(!surface.key(area(), TOP, Press::Delete));
    assert!(surface.take_wants_delete().is_empty());
}

#[test]
fn escape_gives_the_keyboard_back() {
    let mut surface = three();
    surface.focus_first(area(), TOP);
    assert!(surface.key(area(), TOP, Press::Escape));
    assert!(!surface.has_keyboard());
    // Without the keyboard, Enter, Delete and Escape are not ours.
    assert!(!surface.key(area(), TOP, Press::Enter));
    assert!(!surface.key(area(), TOP, Press::Delete));
    assert!(!surface.key(area(), TOP, Press::Escape));
}

/// **Deleting the selected row moves the selection to its neighbour**: the
/// chat that took its place, or the one above when it was the last.
#[test]
fn deleting_the_selected_row_moves_the_selection_to_its_neighbour() {
    let mut surface = three();
    surface.focus_first(area(), TOP);
    surface.key(area(), TOP, Press::Down);
    assert_eq!(surface.focused_chat(), Some(id(2)));
    surface.delete(id(2));
    surface.take_deleted(id(2), Ok(()));
    assert_eq!(titles(&surface), ["Newest", "Oldest"]);
    assert_eq!(
        surface.focused_chat(),
        Some(id(3)),
        "the one below moved up"
    );
    assert_eq!(surface.take_deleted_chats(), [id(2)]);

    // The last row's neighbour is the one above it.
    surface.delete(id(3));
    surface.take_deleted(id(3), Ok(()));
    assert_eq!(surface.focused_chat(), Some(id(1)));

    // The last chat has no neighbour: the keyboard has nowhere to be.
    surface.delete(id(1));
    surface.take_deleted(id(1), Ok(()));
    assert_eq!(surface.focus(), None);
}

/// The same rule when the chat vanished because somebody else deleted it —
/// the pane, another tab, the standalone window.
#[test]
fn a_listing_without_the_focused_chat_moves_the_selection_to_its_neighbour() {
    let mut surface = three();
    surface.focus_first(area(), TOP);
    assert_eq!(surface.focused_chat(), Some(id(1)));
    surface.take_listing(Ok(crate::Listing {
        chats: vec![chat(2, "Middle", 60_000), chat(3, "Oldest", 3_600_000)],
        agents: vec![],
    }));
    assert_eq!(surface.focused_chat(), Some(id(2)));
}

/// The keyboard stays on the same chat when a refresh reorders the list.
#[test]
fn a_reorder_keeps_the_keyboard_on_the_same_chat() {
    let mut surface = three();
    surface.focus_first(area(), TOP);
    surface.key(area(), TOP, Press::End);
    assert_eq!(surface.focused_chat(), Some(id(3)));
    // "Oldest" was just answered in, so it is the newest now.
    surface.take_listing(Ok(crate::Listing {
        chats: vec![
            chat(1, "Newest", 1_000),
            chat(2, "Middle", 60_000),
            chat(3, "Oldest", 0),
        ],
        agents: vec![],
    }));
    assert_eq!(titles(&surface)[0], "Oldest");
    assert_eq!(surface.focused_chat(), Some(id(3)));
}

#[test]
fn a_click_opens_and_a_right_click_names_the_chat() {
    let mut surface = three();
    let (_, rect) = surface
        .rows(area(), TOP)
        .into_iter()
        .find(|(row, _)| *row == Row::Chat(1))
        .unwrap();
    assert!(surface.mouse_down(area(), TOP, 60.0, rect.y + 4.0));
    assert_eq!(surface.take_wants_open(), Some(id(2)));
    assert_eq!(
        surface.context_menu(area(), TOP, 60.0, rect.y + 4.0),
        Some(vec![id(2)])
    );
    assert_eq!(surface.focused_chat(), Some(id(2)));
}

/// The keyboard never leaves the band: End on a long list scrolls the last
/// row into view.
#[test]
fn the_keyboard_scrolls_its_row_into_the_band() {
    let many: Vec<_> = (1..=60)
        .map(|n| chat(n, &format!("Chat {n}"), i64::from(n) * 1_000))
        .collect();
    let mut surface = showing(many);
    surface.focus_first(area(), TOP);
    surface.key(area(), TOP, Press::End);
    let band = surface.band(area(), TOP);
    let (_, rect) = surface
        .rows(area(), TOP)
        .into_iter()
        .find(|(row, _)| *row == Row::Chat(59))
        .expect("the last row is in the band");
    assert!(rect.y >= band.y && rect.y + rect.h <= band.y + band.h + 0.5);
}
