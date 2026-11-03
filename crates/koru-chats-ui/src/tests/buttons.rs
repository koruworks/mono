// SPDX-License-Identifier: MPL-2.0

//! A row's own buttons (koru-ai D86): ⋯ and the pin, on hover and under
//! the keyboard, the pin always on a pinned row.

use std::collections::HashMap;

use koru_ai_core::Timestamp;

use super::*;
use crate::{ChatStatus, RowButton};

fn first_row(surface: &mut ChatsSurface) -> (Row, Rect) {
    surface.rows(tall(), TOP)[0]
}

/// At rest a row shows neither; hovered, both — ⋯ at the end, over where
/// the status mark was, the pin before it.
#[test]
fn hover_shows_more_and_the_pin_over_the_status_slot() {
    let mut surface = three();
    let newest = surface.chats()[0].id;
    surface.set_statuses(HashMap::from([(newest, ChatStatus::Unread)]));
    let (row, rect) = first_row(&mut surface);
    let rest = surface.row_parts(row, rect);
    assert_eq!((rest.more.w, rest.pin.w), (0.0, 0.0));
    assert!(rest.end.w > 0.0, "the mark, at rest");

    surface.mouse_move(tall(), TOP, rect.x + 20.0, rect.y + rect.h * 0.5);
    let hovered = surface.row_parts(row, rect);
    assert!(hovered.more.w > 0.0 && hovered.pin.w > 0.0);
    assert_eq!(hovered.end.w, 0.0, "the buttons take the mark's spot");
    assert!(hovered.pin.x < hovered.more.x);
    assert_eq!(hovered.more.x + hovered.more.w, rest.end.x + rest.end.w);
    assert!(hovered.title.x + hovered.title.w <= hovered.pin.x);
}

/// A pinned row shows its pin without the pointer; the keyboard's row
/// shows both buttons.
#[test]
fn a_pinned_row_keeps_its_pin_and_the_keyboards_row_shows_both() {
    let mut pinned = chat(1, "Pinned", 5_000);
    pinned.pinned_at = Some(Timestamp::from_millis(1));
    let mut surface = showing(vec![pinned, chat(2, "Other", 1_000)]);
    let rows = surface.rows(tall(), TOP);
    let (row, rect) = rows[1];
    assert_eq!(row, Row::Chat(0));
    let parts = surface.row_parts(row, rect);
    assert!(parts.pin.w > 0.0);
    assert_eq!(parts.more.w, 0.0);

    surface.key(tall(), TOP, Press::End);
    let (row, rect) = *surface
        .rows(tall(), TOP)
        .iter()
        .find(|(row, _)| *row == Row::Chat(1))
        .unwrap();
    let parts = surface.row_parts(row, rect);
    assert!(parts.more.w > 0.0 && parts.pin.w > 0.0);
}

/// **The pin pins at once and ⋯ asks for the menu under itself**; neither
/// opens the chat.
#[test]
fn the_pin_pins_and_more_asks_for_the_menu_and_neither_opens() {
    let mut surface = three();
    let (row, rect) = first_row(&mut surface);
    let chat = surface.chats()[0].id;
    surface.mouse_move(tall(), TOP, rect.x + 20.0, rect.y + 4.0);
    let parts = surface.row_parts(row, rect);

    let (px, py) = (parts.pin.x + 4.0, parts.pin.y + parts.pin.h * 0.5);
    assert!(surface.mouse_move(tall(), TOP, px, py), "a plate under it");
    assert!(surface.mouse_down(tall(), TOP, px, py));
    assert_eq!(surface.take_wants_pin(), Some((chat, true)));
    assert!(surface.is_pinned(chat));
    assert_eq!(surface.take_wants_open(), None);

    let (row, rect) = *surface
        .rows(tall(), TOP)
        .iter()
        .find(|(row, _)| *row == Row::Chat(0))
        .unwrap();
    surface.mouse_move(tall(), TOP, rect.x + 20.0, rect.y + 4.0);
    let more = surface.row_parts(row, rect).more;
    assert_eq!(
        surface.button_at(row, rect, more.x + 2.0, more.y + 2.0),
        Some(RowButton::More)
    );
    assert!(surface.mouse_down(tall(), TOP, more.x + 2.0, more.y + 2.0));
    assert_eq!(
        surface.take_wants_menu(),
        Some((vec![chat], (more.x, more.y + more.h)))
    );
    assert_eq!(surface.take_wants_open(), None);
}
