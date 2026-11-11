// SPDX-License-Identifier: MPL-2.0

//! **Which row actions a list offers** (Bots P1.8 S8, QUESTIONS O31.3,
//! approved 2026-11-10): an ordinary list keeps every one, exactly as before;
//! a list whose host leaves them out has no pin, no ⋯, no menu and no
//! Delete anywhere — drawn, hit, keyed or asked — and its rows still open.

use std::collections::HashMap;

use super::*;
use crate::{ChatStatus, RowActions};

fn hovered_first(surface: &mut ChatsSurface) -> (Row, Rect) {
    let (row, rect) = surface.rows(tall(), TOP)[0];
    surface.mouse_move(tall(), TOP, rect.x + 20.0, rect.y + rect.h * 0.5);
    (row, rect)
}

/// **A list no host has told is the ordinary list**: every action, the pin
/// and ⋯ under the pointer, the menu from a right-click and the menu key,
/// and Delete from its key.
#[test]
fn an_ordinary_list_keeps_every_row_action() {
    let mut surface = three();
    assert_eq!(surface.row_actions(), RowActions::ORDINARY);
    assert_eq!(RowActions::default(), RowActions::ORDINARY);
    let first = surface.chats()[0].id;
    let (row, rect) = hovered_first(&mut surface);
    let parts = surface.row_parts(row, rect);
    assert!(parts.pin.w > 0.0 && parts.more.w > 0.0);
    assert!(
        surface
            .context_menu(tall(), TOP, rect.x + 20.0, rect.y + 5.0)
            .is_some()
    );
    // The pin last: pinning moves the row under a *Pinned* heading.
    let centre = |r: Rect| (r.x + r.w * 0.5, r.y + r.h * 0.5);
    let (x, y) = centre(parts.pin);
    assert!(surface.mouse_down(tall(), TOP, x, y));
    assert!(surface.take_wants_pin().is_some(), "the pin pins");
    assert!(surface.take_wants_open().is_none(), "and does not open");
    surface.select_only(first);
    assert!(surface.key(tall(), TOP, Press::Menu));
    assert!(surface.take_wants_menu().is_some());
    assert!(surface.key(tall(), TOP, Press::Delete));
    assert_eq!(surface.take_wants_delete(), vec![first]);
}

/// **Open only: no pin, no ⋯, no menu, no Delete** — the row under the
/// pointer keeps its status mark, a press where the buttons would be opens
/// the row, the keys fall through, and an accessibility client's `ask_*`
/// asks for nothing. Enter still opens.
#[test]
fn an_open_only_list_has_no_row_actions_and_still_opens() {
    let mut surface = three();
    let first = surface.chats()[0].id;
    surface.set_statuses(HashMap::from([(first, ChatStatus::NeedsYou)]));
    assert!(surface.set_row_actions(RowActions::OPEN_ONLY));
    assert!(!surface.set_row_actions(RowActions::OPEN_ONLY), "unchanged");
    let (row, rect) = hovered_first(&mut surface);
    let parts = surface.row_parts(row, rect);
    assert_eq!((parts.pin.w, parts.more.w), (0.0, 0.0), "no buttons");
    assert!(parts.end.w > 0.0, "the mark stays under the pointer");
    // Where ⋯ would be: the row opens.
    let end = rect.x + rect.w - 12.0;
    assert!(surface.mouse_down(tall(), TOP, end, rect.y + rect.h * 0.5));
    assert_eq!(surface.take_wants_open(), Some(first));
    assert!(surface.take_wants_pin().is_none());
    assert!(surface.take_wants_menu().is_none());

    assert!(
        surface
            .context_menu(tall(), TOP, rect.x + 20.0, rect.y + 5.0)
            .is_none(),
        "no menu to raise"
    );
    surface.select_only(first);
    assert!(
        !surface.key(tall(), TOP, Press::Menu),
        "the menu key falls through"
    );
    assert!(
        !surface.key(tall(), TOP, Press::Delete),
        "Delete falls through"
    );
    surface.ask_pin(first, true);
    surface.ask_menu(first, (0.0, 0.0));
    surface.ask_delete(first);
    assert!(surface.take_wants_pin().is_none());
    assert!(surface.take_wants_menu().is_none());
    assert!(surface.take_wants_delete().is_empty());
    assert!(!surface.is_pinned(first), "nothing moved");
    assert!(surface.key(tall(), TOP, Press::Enter));
    assert_eq!(surface.take_wants_open(), Some(first), "Enter opens");
}
