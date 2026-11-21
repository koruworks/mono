// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::person::lists;

fn conversation(f: &mut Fixture, at: i64) -> Thread {
    let bot = f.bot.id;
    serde_json::from_value(
        change::apply(
            &mut f.db,
            &f.who,
            Uuid::new_v4(),
            &Change::Conversation {
                bot,
                title: "Side".into(),
            },
            at,
        )
        .unwrap()
        .answer,
    )
    .unwrap()
}
#[test]
fn conversations_keep_main_first_then_recency_and_cursors_skip_no_ties() {
    let mut f = Fixture::new();
    let older = conversation(&mut f, 10);
    let newer = conversation(&mut f, 20);
    let tied = conversation(&mut f, 20);
    let mut expected = vec![newer.id, tied.id];
    expected.sort();
    expected.insert(0, f.bot.main_thread);
    expected.push(older.id);
    let mut after = None;
    let mut actual = vec![];
    loop {
        let value = lists::conversations(&f.db.connection, &f.who, f.bot.id, after, 1).unwrap();
        actual.push(
            serde_json::from_value::<Thread>(value["items"][0].clone())
                .unwrap()
                .id,
        );
        after = serde_json::from_value(value["next"].clone()).unwrap();
        if after.is_none() {
            break;
        }
    }
    assert_eq!(actual, expected);
    let accepted =
        f.db.accept_message(
            &f.who,
            &messages::Envelope {
                id: Uuid::new_v4(),
                to: f.bot.id,
                thread: Some(older.id),
                in_reply_to: None,
                body: "new activity".into(),
                attachments: vec![],
                references: vec![],
                deadline_ms: None,
                reply_to: messages::ReplyTo::Thread,
            },
            30,
        )
        .unwrap();
    let updated = read::thread(&f.db.connection, &f.who, older.id).unwrap();
    assert_eq!(updated.created_at_ms, 10);
    assert_eq!(updated.updated_at_ms, 30);
    let page = lists::conversations(&f.db.connection, &f.who, f.bot.id, None, 100).unwrap();
    assert_eq!(page["items"][1]["id"], older.id.to_string());
    f.db.connection.execute_batch("CREATE TRIGGER fail_new_message AFTER INSERT ON messages BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(
        f.db.accept_message(
            &f.who,
            &messages::Envelope {
                id: Uuid::new_v4(),
                to: f.bot.id,
                thread: Some(older.id),
                in_reply_to: Some(accepted.answer.message),
                body: "fails".into(),
                attachments: vec![],
                references: vec![],
                deadline_ms: None,
                reply_to: messages::ReplyTo::Thread,
            },
            50
        )
        .is_err()
    );
    assert_eq!(
        read::thread(&f.db.connection, &f.who, older.id)
            .unwrap()
            .updated_at_ms,
        30
    );
}

#[test]
fn tab_list_keeps_between_bot_requests_reachable_without_person_membership() {
    let mut f = Fixture::new();
    let side = conversation(&mut f, 10);
    f.db.connection
        .execute(
            "DELETE FROM participants WHERE thread=?1 AND kind='person'",
            [side.id.to_string()],
        )
        .unwrap();
    let page = lists::conversations(&f.db.connection, &f.who, f.bot.id, None, 1).unwrap();
    assert!(page["next"].is_null());
    f.db.connection
        .execute(
            "UPDATE threads SET kind='between_bots' WHERE id=?1",
            [side.id.to_string()],
        )
        .unwrap();
    let first = lists::conversations(&f.db.connection, &f.who, f.bot.id, None, 1).unwrap();
    let after = serde_json::from_value(first["next"].clone()).unwrap();
    let second = lists::conversations(&f.db.connection, &f.who, f.bot.id, after, 1).unwrap();
    assert_eq!(second["items"][0]["id"], side.id.to_string());
    assert!(second["next"].is_null());
    assert!(read::thread(&f.db.connection, &f.who, side.id).is_ok());
}

#[test]
fn activity_is_owner_scoped_includes_paused_queue_and_ignores_old_attention() {
    let mut f = Fixture::new();
    let home = f.bot.definition.access.workspace.as_uuid();
    f.change(Change::Pause {
        bot: f.bot.id,
        revision: 1,
        paused: true,
    });
    f.send(f.bot.main_thread);
    let activity = lists::activity(&f.db.connection, &f.who, home).unwrap();
    assert_eq!(activity["items"].as_array().unwrap().len(), 1);
    assert_eq!(activity["items"][0]["needs_you"], false);
    f.db.connection.execute("INSERT INTO turns(id,thread,bot,grant,state,generation,details,created_at_ms) VALUES (?1,?2,?3,x'7b7d','needs_you',1,x'7b7d',1)",params![Uuid::new_v4().to_string(),f.bot.main_thread.to_string(),f.bot.id.to_string()]).unwrap();
    assert_eq!(
        lists::activity(&f.db.connection, &f.who, home).unwrap()["items"][0]["needs_you"],
        true
    );
    f.db.connection.execute("INSERT INTO turns(id,thread,bot,grant,state,generation,details,created_at_ms) VALUES (?1,?2,?3,x'7b7d','answered',2,x'7b7d',2)",params![Uuid::new_v4().to_string(),f.bot.main_thread.to_string(),f.bot.id.to_string()]).unwrap();
    assert_eq!(
        lists::activity(&f.db.connection, &f.who, home).unwrap()["items"][0]["needs_you"],
        false
    );
    let other = Person {
        owner: "other".into(),
        sender: f.who.sender.clone(),
    };
    assert!(
        lists::activity(&f.db.connection, &other, home).unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    f.change(Change::BotLifecycle {
        bot: f.bot.id,
        revision: 2,
        lifecycle: Lifecycle::Trashed,
        source: None,
    });
    assert!(
        lists::activity(&f.db.connection, &f.who, home).unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        lists::trashed(&f.db.connection, &f.who, home, None, 100).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
