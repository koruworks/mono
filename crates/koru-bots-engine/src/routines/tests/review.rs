// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::routines::review;

fn start(f: &mut Fixture, routine: &Routine) -> (Uuid, messages::Delivery) {
    let occurrence = f
        .change(
            Change::RunNow {
                bot: f.bot,
                routine: routine.id,
                revision: routine.revision,
            },
            10,
        )
        .occurrence
        .unwrap();
    (occurrence.id, f.claim(routine, 11))
}

#[test]
fn attention_is_explicit_and_skip_preserves_the_exact_result() {
    let mut f = Fixture::new();
    let routine = f.create();
    for declaration in [Some(false), Some(true), None] {
        let (key, delivery) = start(&mut f, &routine);
        if let Some(needs_person) = declaration {
            let command = Uuid::new_v4();
            let a = review::declare(
                &mut f.db,
                delivery.turn,
                f.bot,
                command,
                key,
                needs_person,
                12,
            )
            .unwrap();
            assert_eq!(
                review::declare(
                    &mut f.db,
                    delivery.turn,
                    f.bot,
                    command,
                    key,
                    needs_person,
                    13
                )
                .unwrap(),
                a
            );
            assert!(
                review::declare(
                    &mut f.db,
                    delivery.turn,
                    f.bot,
                    command,
                    key,
                    !needs_person,
                    13
                )
                .is_err()
            );
        }
        let published = f.publish(&delivery, 14);
        assert!(f.db.publish_reply(&published, 15).unwrap().replayed);
        let page = f.db.routine_reviews(&f.who, f.bot, None).unwrap();
        assert_eq!(page.count, u64::from(declaration != Some(false)));
        let before = f.db.routine_review(&f.who, f.bot, key, 0).unwrap();
        assert!(!before.text.is_empty());
        if declaration != Some(false) {
            let command = Uuid::new_v4();
            let skipped =
                f.db.skip_routine_review(&f.who, command, f.bot, key, 16)
                    .unwrap();
            assert_eq!(skipped.answer.review.as_deref(), Some("skipped"));
            assert!(
                f.db.skip_routine_review(&f.who, command, f.bot, key, 17)
                    .unwrap()
                    .replayed
            );
            assert_eq!(f.db.routine_reviews(&f.who, f.bot, None).unwrap().count, 0);
        }
        let after = f.db.routine_review(&f.who, f.bot, key, 0).unwrap();
        assert_eq!(before.text, after.text);
        assert_eq!(before.item.reply, after.item.reply);
        assert_eq!(before.item.card, after.item.card);
        assert!(f.history(&routine).iter().any(|v| v.id == key));
    }
}

#[test]
fn review_is_owner_and_bot_scoped_and_declarations_are_turn_scoped() {
    let mut f = Fixture::new();
    let routine = f.create();
    let (key, delivery) = start(&mut f, &routine);
    assert!(
        review::declare(
            &mut f.db,
            delivery.turn,
            Uuid::new_v4(),
            Uuid::new_v4(),
            key,
            false,
            12
        )
        .is_err()
    );
    assert!(
        review::declare(
            &mut f.db,
            delivery.turn,
            f.bot,
            Uuid::new_v4(),
            Uuid::new_v4(),
            false,
            12
        )
        .is_err()
    );
    f.publish(&delivery, 13);
    assert!(
        review::declare(
            &mut f.db,
            delivery.turn,
            f.bot,
            Uuid::new_v4(),
            key,
            false,
            14
        )
        .is_err()
    );
    let stranger = Person {
        owner: "other".into(),
        sender: "other".into(),
    };
    assert!(f.db.routine_reviews(&stranger, f.bot, None).is_err());
    assert!(f.db.routine_review(&stranger, f.bot, key, 0).is_err());
    assert!(
        f.db.skip_routine_review(&stranger, Uuid::new_v4(), f.bot, key, 15)
            .is_err()
    );
    assert!(f.db.routine_review(&f.who, Uuid::new_v4(), key, 0).is_err());
    assert_eq!(f.db.routine_reviews(&f.who, f.bot, None).unwrap().count, 1);
}

#[test]
fn interrupted_turn_cannot_hide_a_replacement_result() {
    let mut f = Fixture::new();
    let routine = f.create();
    let (key, delivery) = start(&mut f, &routine);
    review::declare(
        &mut f.db,
        delivery.turn,
        f.bot,
        Uuid::new_v4(),
        key,
        false,
        12,
    )
    .unwrap();
    f.db.interrupt_turn(delivery.turn).unwrap();
    let replacement = f.claim(&routine, 14);
    assert_ne!(replacement.turn.id, delivery.turn.id);
    f.publish(&replacement, 15);
    assert_eq!(f.db.routine_reviews(&f.who, f.bot, None).unwrap().count, 1);
    assert_eq!(
        f.db.routine_review(&f.who, f.bot, key, 0)
            .unwrap()
            .item
            .review
            .as_deref(),
        Some("pending")
    );
}

#[test]
fn pages_keep_total_count_and_equal_time_cursors_and_chunks_are_exact() {
    let mut f = Fixture::new();
    let routine = f.create();
    let body = "\"\n🦀".repeat(24_000);
    let mut ids = std::collections::HashSet::new();
    for _ in 0..19 {
        let (key, delivery) = start(&mut f, &routine);
        ids.insert(key);
        f.db.publish_reply(
            &messages::Publish {
                command: Uuid::new_v4(),
                turn: delivery.turn,
                body: body.clone(),
                outputs: vec![],
            },
            20,
        )
        .unwrap();
    }
    let mut cursor = None;
    let mut visited = std::collections::HashSet::new();
    loop {
        let page = f.db.routine_reviews(&f.who, f.bot, cursor).unwrap();
        assert_eq!(page.count, 19);
        assert!(page.items.len() <= crate::limits::MAX_REVIEW_PAGE_ENTRIES);
        assert!(encode(&page).unwrap().len() <= crate::limits::MAX_PAGE_BYTES / 2);
        for item in page.items {
            assert!(visited.insert(item.occurrence));
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(visited, ids);
    let key = *ids.iter().next().unwrap();
    let mut offset = 0;
    let mut full = String::new();
    loop {
        let chunk = f.db.routine_review(&f.who, f.bot, key, offset).unwrap();
        assert!(chunk.text.len() <= crate::limits::MAX_ROUTINE_RESULT_CHUNK_BYTES);
        assert!(encode(&chunk).unwrap().len() < crate::person::MAX_COMMAND_BYTES);
        full.push_str(&chunk.text);
        let Some(next) = chunk.next else {
            break;
        };
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(full, body);
    assert!(f.db.routine_review(&f.who, f.bot, key, 3).is_err());
}

#[test]
fn v2_migration_preserves_history_without_pending_backfill() {
    use koru_sqlite::{Migration, Schema};
    static MIGRATIONS: [Migration; 3] = [
        Migration {
            to: 1,
            name: "001_engine.sql",
            sql: include_str!("../../../migrations/001_engine.sql"),
            columns: &[],
        },
        Migration {
            to: 2,
            name: "002_routines.sql",
            sql: include_str!("../../../migrations/002_routines.sql"),
            columns: &[],
        },
        Migration {
            to: 3,
            name: "003_routine_review.sql",
            sql: include_str!("../../../migrations/003_routine_review.sql"),
            columns: &[],
        },
    ];
    let old = Schema {
        database: "engine.db",
        understood: 2,
        migrations: &MIGRATIONS[..2],
    };
    let mut f = Fixture::with_db(Database {
        connection: koru_sqlite::open_in_memory(&old).unwrap(),
    });
    let routine = f.create();
    let occurrence = Uuid::new_v4();
    let reply = Uuid::new_v4();
    let card = Uuid::new_v4();
    let content = messages::Content {
        body: "Retained v2 result".into(),
        attachments: vec![],
        references: vec![],
        in_reply_to: None,
        deadline_ms: None,
        reply_to: messages::ReplyTo::Thread,
    };
    for (id, thread, kind) in [
        (reply, routine.thread, "words"),
        (card, f.main, "thread_reference"),
    ] {
        f.db.connection.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES (?1,?2,1,?3,?4,?5,'published',10)",params![id.to_string(),thread.to_string(),f.bot.to_string(),kind,encode(&content).unwrap()]).unwrap();
    }
    f.db.connection.execute("INSERT INTO routine_occurrences(id,routine,definition_revision,occurrence_ms,manual,state,reply,card,recorded_at_ms,updated_at_ms) VALUES (?1,?2,1,10,1,'answered',?3,?4,10,10)",params![occurrence.to_string(),routine.id.to_string(),reply.to_string(),card.to_string()]).unwrap();
    let schema = Schema {
        database: "engine.db",
        understood: 3,
        migrations: &MIGRATIONS,
    };
    koru_sqlite::prepare(&schema, &mut f.db.connection).unwrap();
    assert_eq!(f.db.routine_reviews(&f.who, f.bot, None).unwrap().count, 0);
    let result = f.db.routine_review(&f.who, f.bot, occurrence, 0).unwrap();
    assert_eq!(result.text, content.body);
    assert_eq!(result.item.review, None);
    assert_eq!(f.history(&routine)[0].reply, Some(reply));
    assert!(koru_sqlite::prepare(&old, &mut f.db.connection).is_err());
}
