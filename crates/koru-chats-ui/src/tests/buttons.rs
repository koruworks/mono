// SPDX-License-Identifier: MPL-2.0

//! A row's own buttons (koru-ai D86): ⋯ and the pin, **on the row under
//! the pointer only** (D91) — the keyboard reaches the same menu with the
//! menu key.

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

/// **Only the pointer brings the buttons** (the user, 2026-11-03, D91): a
/// pinned row at rest shows no pin — its *Pinned* heading says so — and
/// the keyboard's row, the open one and a picked one show neither; each
/// shows its status mark instead. The pointer over the row shows both.
#[test]
fn only_the_row_under_the_pointer_shows_its_pin_and_more() {
    let mut pinned = chat(1, "Pinned", 5_000);
    pinned.pinned_at = Some(Timestamp::from_millis(1));
    let mut surface = showing(vec![pinned, chat(2, "Other", 1_000)]);
    surface.set_statuses(HashMap::from([(id(1), ChatStatus::Working)]));
    let rows = surface.rows(tall(), TOP);
    let (row, rect) = rows[1];
    assert_eq!(row, Row::Chat(0));
    let parts = surface.row_parts(row, rect);
    assert_eq!(
        (parts.pin.w, parts.more.w),
        (0.0, 0.0),
        "a pinned row at rest"
    );
    assert!(parts.end.w > 0.0, "its status mark shows");

    // The keyboard's row, open and picked: still no buttons.
    surface.key(tall(), TOP, Press::Home);
    surface.set_current(Some(id(1)));
    surface.select_only(id(1));
    let parts = surface.row_parts(row, rect);
    assert_eq!(
        (parts.pin.w, parts.more.w),
        (0.0, 0.0),
        "not the keyboard's"
    );
    assert!(parts.end.w > 0.0);

    // The pointer over the row — or over a button, which is inside it.
    surface.mouse_move(tall(), TOP, rect.x + 20.0, rect.y + rect.h * 0.5);
    let parts = surface.row_parts(row, rect);
    assert!(parts.more.w > 0.0 && parts.pin.w > 0.0);
    surface.mouse_move(
        tall(),
        TOP,
        parts.more.x + parts.more.w * 0.5,
        parts.more.y + parts.more.h * 0.5,
    );
    assert!(surface.row_parts(row, rect).more.w > 0.0, "over ⋯ itself");
    assert!(surface.clear_hover());
    assert_eq!(surface.row_parts(row, rect).more.w, 0.0, "the pointer left");
}

/// **The menu key opens the keyboard row's menu** (D91) — the one a
/// right-click and the row's ⋯ open, about that row — hung under the row's
/// end; `Shift+F10` is the same key. With no row under the keyboard it is
/// not claimed.
#[test]
fn the_menu_key_asks_for_the_keyboard_rows_menu() {
    use winit::keyboard::{Key, ModifiersState, NamedKey};
    assert_eq!(
        Press::of(&Key::Named(NamedKey::ContextMenu)),
        Some(Press::Menu)
    );
    assert_eq!(
        Press::of_with(&Key::Named(NamedKey::F10), ModifiersState::SHIFT),
        Some(Press::Menu)
    );
    assert_eq!(Press::of(&Key::Named(NamedKey::F10)), None, "F10 alone");

    let mut surface = three();
    assert!(!surface.key(tall(), TOP, Press::Menu), "no keyboard row");
    surface.key(tall(), TOP, Press::Home);
    let newest = surface.chats()[0].id;
    assert!(surface.key(tall(), TOP, Press::Menu));
    let (row, rect) = surface.rows(tall(), TOP)[0];
    assert_eq!(row, Row::Chat(0));
    let (chats, at) = surface.take_wants_menu().expect("the menu is asked for");
    assert_eq!(chats, vec![newest]);
    assert_eq!(at.1, rect.y + rect.h, "under the row");
    assert!(
        at.0 > rect.x + rect.w * 0.5,
        "at its end, where ⋯ would sit"
    );
    assert_eq!(surface.take_wants_open(), None, "and nothing opened");
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

/// **What a press would choose, asked without choosing it** — the host's
/// question before a modified press (the navigator's Alt+click): the chat
/// under a row's title, nothing on its pin or ⋯, and nothing off a row; and
/// asking picks nothing and opens nothing.
#[test]
fn chat_pressed_at_names_the_row_off_its_buttons_and_changes_nothing() {
    let mut surface = three();
    let (row, rect) = first_row(&mut surface);
    let chat = surface.chats()[0].id;
    let (x, y) = (rect.x + 20.0, rect.y + rect.h * 0.5);
    surface.mouse_move(tall(), TOP, x, y);
    assert_eq!(surface.chat_pressed_at(tall(), TOP, x, y), Some(chat));

    let parts = surface.row_parts(row, rect);
    for button in [parts.pin, parts.more] {
        let (bx, by) = (button.x + button.w * 0.5, button.y + button.h * 0.5);
        assert_eq!(
            surface.chat_pressed_at(tall(), TOP, bx, by),
            None,
            "a row's button keeps its own meaning"
        );
    }
    let last = surface.rows(tall(), TOP).last().copied().unwrap().1;
    assert_eq!(
        surface.chat_pressed_at(tall(), TOP, x, last.y + last.h + 40.0),
        None,
        "off the rows"
    );
    assert!(surface.selected().is_empty(), "nothing picked");
    assert_eq!(surface.take_wants_open(), None, "nothing opened");
}

/// **A row being renamed stands aside for the field** (D89, the user's
/// screenshot): no card, tint or ring behind the field, no status mark, no
/// pin and no ⋯ to draw or press — and the field's text starts where the
/// title did and runs to the row's end pad. When the rename ends the row
/// is itself again.
#[test]
fn a_row_being_renamed_draws_and_hits_nothing_but_the_fields_area() {
    let mut pinned = chat(1, "Renamed", 1_000);
    pinned.pinned_at = Some(Timestamp::from_millis(1));
    let mut surface = showing(vec![pinned, chat(2, "Other", 60_000)]);
    let chat_id = id(1);
    surface.set_current(Some(chat_id));
    surface.set_statuses(HashMap::from([(chat_id, ChatStatus::Working)]));
    let (row, rect) = *surface
        .rows(tall(), TOP)
        .iter()
        .find(|(row, _)| *row == Row::Chat(0))
        .unwrap();
    // The pointer and the keyboard on it: every button showing.
    surface.mouse_move(tall(), TOP, rect.x + 20.0, rect.y + rect.h * 0.5);
    surface.reveal(chat_id, tall(), TOP);
    let before = surface.row_parts(row, rect);
    assert!(before.more.w > 0.0 && before.pin.w > 0.0);

    assert!(surface.set_renaming(Some(chat_id)));
    let parts = surface.row_parts(row, rect);
    assert_eq!(
        (parts.end.w, parts.pin.w, parts.more.w),
        (0.0, 0.0, 0.0),
        "no mark, no pin, no ⋯"
    );
    let pad_end = rect.x + rect.w - koru_draw::theme::NAV_PAD;
    assert_eq!(parts.title.x, before.title.x, "the title's own inset");
    assert_eq!(parts.title.x + parts.title.w, pad_end, "to the end pad");
    for at in [before.pin, before.more] {
        let (x, y) = (at.x + at.w * 0.5, at.y + at.h * 0.5);
        assert_eq!(surface.button_at(row, rect, x, y), None);
    }
    assert_eq!(surface.row_treatment(row), "Renaming");

    // The field's box: the text area widened by the field's own text pad,
    // so the typed text starts where the title did, on the row's height.
    let inset = 7.0;
    let field = surface
        .rename_rect(tall(), TOP, chat_id, inset)
        .expect("the row is showing");
    assert_eq!(field.x + inset, parts.title.x);
    assert_eq!(field.x + field.w, pad_end);
    assert_eq!((field.y, field.h), (rect.y, rect.h));
    // The same box before the host has said it is renaming.
    surface.set_renaming(None);
    assert_eq!(
        surface.rename_rect(tall(), TOP, chat_id, inset),
        Some(field)
    );
    surface.set_renaming(Some(chat_id));

    // Drawn: the row's record holds nothing — no plate, ring, text or icon.
    let mut engine = koru_draw::text::TextEngine::new();
    let capture = koru_draw::inspect::Frame::begin(
        1,
        vello::kurbo::Affine::IDENTITY,
        vello::kurbo::Rect::new(0.0, 0.0, 260.0, 2_000.0),
    );
    let mut scene = vello::Scene::new();
    surface.draw(
        &mut scene,
        vello::kurbo::Affine::IDENTITY,
        &mut engine,
        tall(),
        TOP,
    );
    let snapshot = capture.finish();
    let records = &snapshot.records;
    let rows: Vec<usize> = records
        .iter()
        .enumerate()
        .filter(|(_, record)| record.name == "ExploreRow")
        .map(|(index, _)| index)
        .collect();
    let treatment = |index: usize| {
        records[index]
            .properties
            .iter()
            .find(|property| property.name == "treatment")
            .map(|property| property.value.clone())
    };
    let renamed = *rows
        .iter()
        .find(|index| treatment(**index).as_deref() == Some("Renaming"))
        .expect("the renamed row is recorded");
    let inside = |ancestor: usize| {
        records.iter().filter(move |record| {
            let mut at = record.parent;
            while let Some(parent) = at {
                if parent == ancestor {
                    return true;
                }
                at = records[parent].parent;
            }
            false
        })
    };
    assert_eq!(
        inside(renamed).count(),
        0,
        "nothing drawn on the renamed row: {:?}",
        inside(renamed)
            .map(|record| record.name)
            .collect::<Vec<_>>()
    );
    let other = *rows
        .iter()
        .find(|index| **index != renamed && treatment(**index).as_deref() == Some("Idle"))
        .expect("the other row");
    assert!(inside(other).count() > 0, "an ordinary row draws its title");

    // Done: the row is itself again.
    assert!(surface.set_renaming(None));
    let after = surface.row_parts(row, rect);
    assert!(after.more.w > 0.0 && after.pin.w > 0.0);
    assert_eq!(surface.row_treatment(row), "Card");
}
