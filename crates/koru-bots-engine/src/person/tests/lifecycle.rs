// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn restore_reverses_the_named_layer_and_returns_paused_without_restarting_work() {
    let mut f = Fixture::new();
    let bot = f.bot.id;
    let archive = Uuid::new_v4();
    let action = Change::BotLifecycle {
        bot,
        revision: 1,
        lifecycle: Lifecycle::Archived,
        source: None,
    };
    let original = change::apply(&mut f.db, &f.who, archive, &action, 2).unwrap();
    let trash = Uuid::new_v4();
    change::apply(
        &mut f.db,
        &f.who,
        trash,
        &Change::BotLifecycle {
            bot,
            revision: 2,
            lifecycle: Lifecycle::Trashed,
            source: None,
        },
        3,
    )
    .unwrap();
    let restore = Uuid::new_v4();
    assert_eq!(
        change::apply(
            &mut f.db,
            &f.who,
            restore,
            &Change::BotLifecycle {
                bot,
                revision: 3,
                lifecycle: Lifecycle::Active,
                source: Some(archive),
            },
            4
        )
        .err(),
        Some(Error::Conflict)
    );
    let restored = change::apply(
        &mut f.db,
        &f.who,
        restore,
        &Change::BotLifecycle {
            bot,
            revision: 3,
            lifecycle: Lifecycle::Active,
            source: Some(trash),
        },
        4,
    )
    .unwrap();
    assert_eq!(restored.answer["lifecycle"], "archived");
    assert_eq!(restored.answer["held_by"]["request"], archive.to_string());
    assert_eq!(restored.answer["paused"], true);
    let operation = lifecycle_ops::read(&f.db.connection, &f.who, bot, trash).unwrap();
    assert_eq!(operation.reversed_by, Some(restore));
    assert_eq!(operation.completed_at_ms, 3);
    f.change(Change::BotLifecycle {
        bot,
        revision: 4,
        lifecycle: Lifecycle::Active,
        source: Some(archive),
    });
    let active = read::bot(&f.db.connection, &f.who, bot).unwrap();
    assert!(active.lifecycle == Lifecycle::Active && active.paused && active.held_by.is_none());
    let replay = change::apply(&mut f.db, &f.who, archive, &action, 6).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.answer, original.answer);
    let other = Person {
        owner: "other".into(),
        sender: f.who.sender.clone(),
    };
    assert_eq!(
        lifecycle_ops::read(&f.db.connection, &other, bot, archive).err(),
        Some(Error::NotAllowed)
    );
}

#[test]
fn failed_restore_rolls_back_reversal_and_a_separately_trashed_conversation_stays_trashed() {
    let mut f = Fixture::new();
    let thread: Thread = serde_json::from_value(f.change(Change::Conversation {
        bot: f.bot.id,
        title: "Separate".into(),
    }))
    .unwrap();
    let trash = Uuid::new_v4();
    change::apply(
        &mut f.db,
        &f.who,
        trash,
        &Change::ConversationLifecycle {
            bot: f.bot.id,
            revision: 1,
            thread: thread.id,
            lifecycle: Lifecycle::Trashed,
            source: None,
        },
        3,
    )
    .unwrap();
    let archive = Uuid::new_v4();
    change::apply(
        &mut f.db,
        &f.who,
        archive,
        &Change::BotLifecycle {
            bot: f.bot.id,
            revision: 1,
            lifecycle: Lifecycle::Archived,
            source: None,
        },
        4,
    )
    .unwrap();
    assert_eq!(
        change::apply(
            &mut f.db,
            &f.who,
            Uuid::new_v4(),
            &Change::ConversationLifecycle {
                bot: f.bot.id,
                revision: 2,
                thread: thread.id,
                lifecycle: Lifecycle::Active,
                source: Some(trash),
            },
            5
        )
        .err(),
        Some(Error::NotAllowed)
    );
    f.db.connection.execute_batch("CREATE TRIGGER fail_restore BEFORE UPDATE OF lifecycle ON bots BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    let restore = Uuid::new_v4();
    let action = Change::BotLifecycle {
        bot: f.bot.id,
        revision: 2,
        lifecycle: Lifecycle::Active,
        source: Some(archive),
    };
    assert_eq!(
        change::apply(&mut f.db, &f.who, restore, &action, 5).err(),
        Some(Error::StorageUnavailable)
    );
    assert!(
        lifecycle_ops::read(&f.db.connection, &f.who, f.bot.id, archive)
            .unwrap()
            .reversed_by
            .is_none()
    );
    assert!(lifecycle_ops::read(&f.db.connection, &f.who, f.bot.id, restore).is_err());
    f.db.connection
        .execute_batch("DROP TRIGGER fail_restore")
        .unwrap();
    change::apply(&mut f.db, &f.who, restore, &action, 6).unwrap();
    let still = read::thread(&f.db.connection, &f.who, thread.id).unwrap();
    assert!(still.lifecycle == Lifecycle::Trashed);
    assert_eq!(still.held_by.unwrap().request, trash);
    f.change(Change::ConversationLifecycle {
        bot: f.bot.id,
        revision: 3,
        thread: thread.id,
        lifecycle: Lifecycle::Active,
        source: Some(trash),
    });
    assert!(
        read::thread(&f.db.connection, &f.who, thread.id)
            .unwrap()
            .lifecycle
            == Lifecycle::Active
    );
}
