// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn a_routine_appears_in_conversations_only_after_its_first_message() {
    let mut f = Fixture::new();
    let hidden = f.create();
    let active = f.create();
    f.change(
        Change::Timing {
            bot: f.bot,
            routine: hidden.id,
            revision: hidden.revision,
            name: "Renamed before running".into(),
            recurrence: hidden.recurrence.clone(),
        },
        3,
    );
    let page = |f: &mut Fixture, after| {
        person::handle(
            &mut f.db,
            &Default::default(),
            &mut Default::default(),
            person::Request {
                owner: f.who.owner.clone(),
                sender: f.who.sender.clone(),
                command: person::Command::Read {
                    query: person::Query::Conversations {
                        bot: f.bot,
                        after,
                        limit: 1,
                    },
                },
            },
            4,
        )
        .unwrap()
        .0
    };
    let initial = page(&mut f, None);
    assert_eq!(initial["items"].as_array().unwrap().len(), 1);
    assert_eq!(initial["items"][0]["id"], f.main.to_string());
    assert!(
        initial["next"].is_null(),
        "empty routines must not consume a list page"
    );
    assert_eq!(
        f.db.routines(&f.who, f.bot, None, 16).unwrap().items.len(),
        2,
        "both definitions remain available in Routines"
    );
    f.change(
        Change::RunNow {
            bot: f.bot,
            routine: active.id,
            revision: active.revision,
        },
        10,
    );
    let first = page(&mut f, None);
    let next = serde_json::from_value(first["next"].clone()).unwrap();
    let second = page(&mut f, next);
    assert_eq!(second["items"][0]["id"], active.thread.to_string());
    assert_eq!(second["items"][0]["title"], active.name);
    assert!(second["next"].is_null());
}

#[test]
fn repeated_firings_share_named_thread_and_keep_its_edited_model_over_bot_defaults() {
    let mut f = Fixture::new();
    let row = f.create();
    let worker = Worker {
        bot: f.bot,
        incarnation: Uuid::new_v4(),
    };
    f.db.connection
        .execute(
            "INSERT INTO workers VALUES (?1,?2,0,0)",
            params![f.bot.to_string(), worker.incarnation.to_string()],
        )
        .unwrap();
    let first_configuration = f.db.configuration(worker, row.thread).unwrap();
    assert!(first_configuration.selection.model.is_none());
    let first = f
        .change(
            Change::RunNow {
                bot: f.bot,
                routine: row.id,
                revision: row.revision,
            },
            10,
        )
        .occurrence
        .unwrap();
    let delivery = f.claim(&row, 11);
    f.publish(&delivery, 12);

    let mut selection = first_configuration.selection;
    selection.model = Some("routine-choice".parse().unwrap());
    person::handle(
        &mut f.db,
        &Default::default(),
        &mut Default::default(),
        person::Request {
            owner: f.who.owner.clone(),
            sender: f.who.sender.clone(),
            command: person::Command::Change {
                id: Uuid::new_v4(),
                action: person::Change::ConversationSettings {
                    bot: f.bot,
                    thread: row.thread,
                    revision: 1,
                    provider: first_configuration.provider.clone(),
                    selection,
                },
            },
        },
        13,
    )
    .unwrap();
    let mut defaults = crate::configuration::conversation(&f.db.connection, f.bot, row.thread)
        .unwrap()
        .configuration;
    defaults.selection.model = Some("new-bot-default".parse().unwrap());
    person::handle(
        &mut f.db,
        &Default::default(),
        &mut Default::default(),
        person::Request {
            owner: f.who.owner.clone(),
            sender: f.who.sender.clone(),
            command: person::Command::Change {
                id: Uuid::new_v4(),
                action: person::Change::Configure {
                    bot: f.bot,
                    revision: 1,
                    name: "Scout".into(),
                    avatar: None,
                    configuration: defaults,
                    role: None,
                },
            },
        },
        14,
    )
    .unwrap();
    let renamed = f
        .change(
            Change::Timing {
                bot: f.bot,
                routine: row.id,
                revision: row.revision,
                name: "Daily research".into(),
                recurrence: row.recurrence.clone(),
            },
            15,
        )
        .routine;
    assert_eq!(renamed.thread, row.thread);
    let title: String =
        f.db.connection
            .query_row(
                "SELECT title FROM threads WHERE id=?1",
                [row.thread.to_string()],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(title, "Daily research");
    assert_eq!(
        f.db.configuration(worker, row.thread)
            .unwrap()
            .selection
            .model
            .unwrap()
            .as_str(),
        "routine-choice"
    );
    let second = f
        .change(
            Change::RunNow {
                bot: f.bot,
                routine: row.id,
                revision: renamed.revision,
            },
            20,
        )
        .occurrence
        .unwrap();
    let delivery = f.claim(&renamed, 21);
    f.publish(&delivery, 22);
    let history = f.history(&renamed);
    let first = history.iter().find(|o| o.id == first.id).unwrap();
    let second = history.iter().find(|o| o.id == second.id).unwrap();
    assert_ne!(first.id, second.id);
    assert_ne!(first.message, second.message);
    assert_ne!(first.reply, second.reply);
    assert_eq!(
        (first.state.as_str(), second.state.as_str()),
        ("answered", "answered")
    );
    for message in [first.message, first.reply, second.message, second.reply]
        .into_iter()
        .flatten()
    {
        let thread: String =
            f.db.connection
                .query_row(
                    "SELECT thread FROM messages WHERE id=?1",
                    [message.to_string()],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(thread, row.thread.to_string());
    }
}
