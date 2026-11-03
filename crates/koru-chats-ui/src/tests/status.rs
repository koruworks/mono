// SPDX-License-Identifier: MPL-2.0

//! A row's status mark (koru-ai D83): where it sits, what it is called, and
//! when it asks for frames.

use std::collections::HashMap;

use super::*;
use crate::ChatStatus;

/// The mark takes a slot at the row's end only when there is one to show,
/// so an idle title keeps the whole width.
#[test]
fn a_status_takes_the_rows_end_and_an_idle_row_keeps_its_width() {
    let mut surface = three();
    let rows = surface.rows(tall(), TOP);
    let (row, rect) = rows[0];
    let idle = surface.row_parts(row, rect);
    assert_eq!(idle.end.w, 0.0);

    let newest = surface.chats()[0].id;
    assert!(surface.set_statuses(HashMap::from([(newest, ChatStatus::Unread)])));
    let marked = surface.row_parts(row, rect);
    assert!(marked.end.w > 0.0);
    assert!(
        marked.title.x + marked.title.w <= marked.end.x,
        "no overlap"
    );
    assert!(marked.title.w < idle.title.w);
}

/// The same map twice is no repaint; idle entries are not kept.
#[test]
fn the_same_statuses_twice_owe_no_repaint() {
    let mut surface = three();
    let chat = surface.chats()[1].id;
    let statuses = HashMap::from([(chat, ChatStatus::NeedsYou)]);
    assert!(surface.set_statuses(statuses.clone()));
    assert!(!surface.set_statuses(statuses));
    assert_eq!(surface.status(chat), ChatStatus::NeedsYou);
    assert!(!surface.set_statuses(HashMap::from([(chat, ChatStatus::NeedsYou)])));
    assert!(surface.set_statuses(HashMap::from([(chat, ChatStatus::Idle)])));
    assert_eq!(surface.status(chat), ChatStatus::Idle);
}

/// **Frames only while a ring turns**: an idle list, an unread one and a
/// reduced-motion one ask for none.
#[test]
fn only_a_working_row_asks_for_frames_and_never_under_reduced_motion() {
    let mut surface = three();
    assert!(!surface.animating(), "an idle list");
    let chat = surface.chats()[2].id;
    surface.set_statuses(HashMap::from([(chat, ChatStatus::Unread)]));
    assert!(!surface.animating(), "a dot holds still");
    surface.set_statuses(HashMap::from([(chat, ChatStatus::Working)]));
    assert!(surface.animating());
    surface.set_reduced_motion(true);
    assert!(!surface.animating(), "reduced motion");
}

/// A screen reader hears what the mark says, after the row's name.
#[test]
fn the_accessible_name_says_the_status() {
    let mut surface = three();
    let chat = surface.chats()[0].id;
    let plain = surface.accessible_name(chat).unwrap();
    surface.set_statuses(HashMap::from([(chat, ChatStatus::Working)]));
    let named = surface.accessible_name(chat).unwrap();
    assert_eq!(named, format!("{plain}, working"));
}
