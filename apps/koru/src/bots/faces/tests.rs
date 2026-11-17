// SPDX-License-Identifier: MPL-2.0

use koru_ai_core::bots::{BotId, DEFAULT_AVATAR};
use koru_draw::el::Name;
use koru_i18n::Key as Words;
use koru_store_core::ids::WorkspaceId;

use super::*;
use crate::bots::roster::{BotsRoster, RosterBot, row_key, runs_here_key};
use crate::bots::tests::bot;
use crate::bots::view::BotFacts;

fn area() -> Rect {
    Rect::new(
        0.0,
        koru_draw::theme::CAPTION_H,
        koru_draw::theme::SIDEBAR_W,
        600.0,
    )
}

/// A face leaf is a square of its size, drawn in the middle of what it is
/// given, and never larger than that.
#[test]
fn a_face_is_a_square_in_the_middle_of_its_allocation() {
    let leaf = FaceLeaf::still(3, "Scout", ROW_FACE);
    assert_eq!(leaf.measure(None), (ROW_FACE.value, ROW_FACE.value));
    assert_eq!(leaf.measure(Some(200.0)), (ROW_FACE.value, ROW_FACE.value));
    assert_eq!(leaf.pose, FacePose::STILL, "at rest unless it is posed");
    assert_eq!(
        centred(Rect::new(10.0, 20.0, 100.0, 40.0), 28.0),
        Rect::new(46.0, 26.0, 28.0, 28.0)
    );
    assert_eq!(
        centred(Rect::new(0.0, 0.0, 20.0, 60.0), 28.0),
        Rect::new(0.0, 20.0, 20.0, 20.0),
        "never larger than its room"
    );
}

/// **A Bot's row and its tab wear the Bot's face**: the one the person
/// picked when one is stored, else the default one (row 0 of the table, the
/// gray face with two dot eyes), as the Bot itself says. The row draws it at
/// the row's size, before the name, and says where the Bot runs with a mark
/// that has words of its own.
#[test]
fn a_row_shows_the_stored_face_and_the_default_one() {
    let home = WorkspaceId::new();
    let mut picked = bot(BotId::mint(), home, "Scout", false);
    picked.config.avatar = Some(7);
    let plain = bot(BotId::mint(), home, "Ledger", false);
    assert_eq!(plain.config.avatar, None);

    let rows = [
        RosterBot::from(picked.clone()),
        RosterBot::from(plain.clone()),
    ];
    assert_eq!(rows[0].avatar, 7, "the stored choice");
    assert_eq!(rows[1].avatar, 0, "none stored: the first of the table");
    assert_eq!(rows[1].avatar, DEFAULT_AVATAR);
    assert_eq!(rows[1].avatar, plain.avatar(), "as the Bot itself says");
    // The tab's header reads the same two.
    assert_eq!(BotFacts::from(picked.clone()).avatar, 7);
    assert_eq!(BotFacts::from(&rows[1]).avatar, DEFAULT_AVATAR);
    assert_eq!(BotFacts::from(plain.clone()).avatar, DEFAULT_AVATAR);

    let mut roster = BotsRoster::new();
    roster.listed_for_test(rows.to_vec());
    let top = area().y + koru_draw::theme::CAPTION_H;
    roster.with_input_frame(area(), top, |_, frame| {
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        let faces: Vec<Rect> = frame.named("Bot face").map(|node| node.rect).collect();
        assert_eq!(faces.len(), 2, "one face a row");
        for (face, row) in faces.iter().zip(&rows) {
            assert_eq!((face.w, face.h), (ROW_FACE.value, ROW_FACE.value));
            let plate = frame.node(row_key(row.id)).expect("the row").rect;
            assert!(plate.h >= ROW_MIN.value, "{plate:?}");
            assert!(
                face.y >= plate.y && face.y + face.h <= plate.y + plate.h,
                "the face is inside its row: {face:?} in {plate:?}"
            );
            let name = frame
                .named("Bot name")
                .map(|node| node.rect)
                .find(|name| name.y >= plate.y && name.y < plate.y + plate.h)
                .expect("the name");
            assert!(face.x + face.w <= name.x, "the face, then the name");
            // Where it runs: a mark after the name, named in words.
            let mark = frame.node(runs_here_key(row.id)).expect("the device mark");
            assert_eq!(mark.label, Some(Name::Key(Words::BotsRunsOnThisComputer)));
            assert!(mark.rect.x >= name.x, "after the name");
        }
        // No status line under the name: nothing records one.
        assert_eq!(frame.named("Bot name").count(), 2);
    });
}
