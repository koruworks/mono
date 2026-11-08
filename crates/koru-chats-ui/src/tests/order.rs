// SPDX-License-Identifier: MPL-2.0

//! The list's order (koru-ai D85): pinned first, oldest pin first, under
//! a heading; the rest by last activity, moving as it happens.

use koru_ai_core::Timestamp;

use super::*;

fn pinned(mut chat: koru_ai_core::Chat, at: i64) -> koru_ai_core::Chat {
    chat.pinned_at = Some(Timestamp::from_millis(at));
    chat
}

/// **Pinned above the rest, under its heading, oldest pin first**; the
/// rest by activity, newest first, after a little air.
#[test]
fn pinned_chats_head_the_list_oldest_pin_first() {
    let mut surface = showing(vec![
        chat(1, "Newest", 1_000),
        pinned(chat(2, "Pinned later", 60_000), 900),
        chat(3, "Oldest", 3_600_000),
        pinned(chat(4, "Pinned first", 120_000), 100),
    ]);
    assert_eq!(
        titles(&surface),
        ["Pinned first", "Pinned later", "Newest", "Oldest"]
    );
    assert_eq!(
        rows_of(&mut surface),
        [
            Row::PinnedHeading,
            Row::Chat(0),
            Row::Chat(1),
            Row::GroupGap,
            Row::Chat(2),
            Row::Chat(3),
        ]
    );
}

/// No pin, no heading; every chat pinned, no gap.
#[test]
fn the_heading_and_the_gap_show_only_when_they_separate_something() {
    let mut none = three();
    assert!(!rows_of(&mut none).contains(&Row::PinnedHeading));
    let mut all = showing(vec![pinned(chat(1, "Only", 0), 5)]);
    assert_eq!(rows_of(&mut all), [Row::PinnedHeading, Row::Chat(0)]);
}

/// The keyboard steps over the heading and the gap: they are not rows a
/// person can choose.
#[test]
fn the_keyboard_never_lands_on_the_heading() {
    let mut surface = showing(vec![
        pinned(chat(1, "Pinned", 5_000), 10),
        chat(2, "Rest", 1_000),
    ]);
    surface.key(tall(), TOP, Press::Home);
    assert_eq!(surface.focused_chat(), Some(id(1)));
    surface.key(tall(), TOP, Press::Down);
    assert_eq!(surface.focused_chat(), Some(id(2)));
    surface.key(tall(), TOP, Press::Up);
    assert_eq!(surface.focused_chat(), Some(id(1)));
}

/// **A chat moves up when a message is sent in it or its reply arrives**,
/// without the list being asked again; one already newer does not move.
#[test]
fn a_touched_chat_moves_to_the_top_of_the_unpinned() {
    let mut surface = three();
    assert!(surface.touch(id(3), NOW + 5));
    assert_eq!(titles(&surface), ["Oldest", "Newest", "Middle"]);
    assert!(!surface.touch(id(3), NOW), "an older moment moves nothing");
    let mut with_pin = showing(vec![
        pinned(chat(1, "Pinned", 9_000), 1),
        chat(2, "Rest", 1),
    ]);
    with_pin.touch(id(2), NOW + 10);
    assert_eq!(titles(&with_pin), ["Pinned", "Rest"], "pins stay above");
}

/// **A pin moves the row at once** and an unpin returns it to its place by
/// activity — pinning is not activity, so `updated_at` is untouched.
#[test]
fn pinning_moves_the_row_at_once_and_unpinning_returns_it() {
    let mut surface = three();
    assert!(surface.set_pinned(id(3), true, NOW));
    assert_eq!(titles(&surface), ["Oldest", "Newest", "Middle"]);
    assert!(!surface.set_pinned(id(3), true, NOW + 1), "already pinned");
    assert!(surface.set_pinned(id(3), false, NOW));
    assert_eq!(titles(&surface), ["Newest", "Middle", "Oldest"]);
    assert_eq!(
        surface.chats()[2].updated_at.millis(),
        NOW - 3_600_000,
        "not activity"
    );
}

/// **A step moves through the list as shown** (D87): into and out of the
/// Pinned group, and not past either end.
#[test]
fn a_step_moves_through_the_list_as_shown_and_stops_at_the_ends() {
    let surface = showing(vec![
        chat(1, "Newest", 1_000),
        pinned(chat(2, "Pinned", 60_000), 5),
        chat(3, "Oldest", 3_600_000),
    ]);
    let chats = surface.chats();
    assert_eq!(crate::ChatStep::Next.from(chats, id(2)), Some(id(1)));
    assert_eq!(crate::ChatStep::Previous.from(chats, id(1)), Some(id(2)));
    assert_eq!(crate::ChatStep::Next.from(chats, id(1)), Some(id(3)));
    assert_eq!(
        crate::ChatStep::Previous.from(chats, id(2)),
        None,
        "the top"
    );
    assert_eq!(crate::ChatStep::Next.from(chats, id(3)), None, "the end");
    assert_eq!(crate::ChatStep::Next.from(chats, id(9)), None, "not listed");
}

/// The keyboard stays on its chat when a row moves under it.
#[test]
fn the_keyboard_follows_its_chat_when_the_order_moves() {
    let mut surface = three();
    surface.key(tall(), TOP, Press::End);
    assert_eq!(surface.focused_chat(), Some(id(3)));
    surface.touch(id(3), NOW + 5);
    assert_eq!(surface.focused_chat(), Some(id(3)));
}
