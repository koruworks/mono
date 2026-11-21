// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::bots::{BotMainPreview, BotSummary};

#[test]
fn dot_is_main_unread_or_pending_result_or_open_request_and_request_overrides_preview() {
    let mut bot = RosterBot::from(crate::bots::tests::bot(
        BotId::mint(),
        WorkspaceId::new(),
        "Scout",
        false,
    ));
    for unread in [false, true] {
        for pending in [0, 1] {
            for requests in [0, 1] {
                bot.summary = Some(BotSummary {
                    latest: Some(BotMainPreview {
                        message: koru_ai_core::MessageId::new(),
                        at_ms: 1,
                        text: "Latest reply\nsecond line".into(),
                    }),
                    pending_results: pending,
                    open_requests: requests,
                });
                assert_eq!(
                    needs_attention(&bot, unread),
                    unread || pending != 0 || requests != 0
                );
                assert_eq!(
                    caption(&bot, Lang::En),
                    if requests > 0 {
                        "needs you"
                    } else {
                        "Latest reply second line"
                    }
                );
            }
        }
    }
}

#[test]
fn flat_rows_clip_preview_and_keep_only_bot_and_menu_keyboard_targets() {
    let mut bot = crate::bots::tests::bot(BotId::mint(), WorkspaceId::new(), "Scout", false);
    bot.summary = Some(BotSummary {
        latest: Some(BotMainPreview {
            message: koru_ai_core::MessageId::new(),
            at_ms: 1,
            text: "A long last reply ".repeat(35),
        }),
        pending_results: 1,
        open_requests: 0,
    });
    let mut roster = BotsRoster::default();
    roster.take(Ok(vec![bot.clone()]));
    let mut renamed = bot.clone();
    renamed.config.name = "Scout renamed".into();
    renamed.summary = None;
    assert!(roster.know_bot(RosterBot::from(renamed)));
    assert_eq!(roster.row(bot.id).unwrap().summary, bot.summary);
    assert_eq!(
        roster.keyboard_stops(),
        vec![row_key(bot.id), more_key(bot.id)]
    );
    for width in [220.0, 320.0] {
        roster.with_input_frame(Rect::new(0.0, 0.0, width, 700.0), 0.0, |_, frame| {
            assert!(!frame.nodes.iter().any(|n| n.name == "Bot activity"));
            let name = frame.nodes.iter().find(|n| n.name == "Bot name").unwrap();
            let preview = frame
                .nodes
                .iter()
                .find(|n| n.name == "Bot last message")
                .unwrap();
            assert!(preview.rect.y >= name.rect.y + name.rect.h);
            assert!(preview.rect.x + preview.rect.w <= width);
            assert!(preview.rect.h < 40.0, "preview remains one clipped line");
        });
    }
    assert!(
        roster
            .accessible_name(roster.row(bot.id).unwrap())
            .contains("A long last reply")
    );
}
