// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::repository::tests::turn;
use koru_ai_core::{AttemptStatus, RecoveryCounts};

#[test]
fn ordinary_recovery_leaves_retired_bot_work_and_feed_unchanged() {
    let mut repository = Repository::in_memory().unwrap();
    let bot = fixture_bot();
    repository
        .save_bot(&bot, ProfileId::new(), BotRequestId::mint(), "create", None)
        .unwrap();
    let ordinary = ChatId::new();
    repository.create_chat(&chat(ordinary, 0)).unwrap();
    let bot_turn = turn(bot.main_chat, uuid::Uuid::new_v4(), 1);
    let ordinary_turn = turn(ordinary, uuid::Uuid::new_v4(), 1);
    repository.begin_turn(&bot_turn).unwrap();
    repository.begin_turn(&ordinary_turn).unwrap();
    let before = repository.attempt(bot_turn.attempt.id).unwrap().unwrap();
    let counts = repository
        .recover_ordinary_interrupted(Timestamp::from_millis(1_770_000_000_003))
        .unwrap();
    assert_eq!(
        counts,
        RecoveryCounts {
            attempts: 1,
            messages: 1
        }
    );
    assert_eq!(
        repository.attempt(bot_turn.attempt.id).unwrap().unwrap(),
        before
    );
    assert_eq!(
        repository
            .attempt(ordinary_turn.attempt.id)
            .unwrap()
            .unwrap()
            .status,
        AttemptStatus::Interrupted
    );
    assert!(repository.feed_audit_refusals().is_empty());
}
