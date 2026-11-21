// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::routines::tests::Fixture;

fn read(f: &Fixture) -> BotSummary {
    let workspace: String =
        f.db.connection
            .query_row(
                "SELECT json_extract(CAST(access AS TEXT),'$.workspace') FROM bots WHERE id=?1",
                [f.bot.to_string()],
                |r| r.get(0),
            )
            .unwrap();
    page(
        &f.db.connection,
        &f.who,
        id(workspace).unwrap(),
        &[f.bot.to_string()],
    )
    .unwrap()
    .remove(&f.bot)
    .unwrap()
}

#[test]
fn questions_and_approvals_agree_with_current_tab_attention() {
    use koru_session_protocol as p;
    for (blocking, delivery, approval, expected) in [
        (true, p::QuestionsDelivery::InTurn, false, 1),
        (false, p::QuestionsDelivery::InTurn, false, 0),
        (true, p::QuestionsDelivery::NextMessage, false, 1),
        (false, p::QuestionsDelivery::NextMessage, false, 0),
        (true, p::QuestionsDelivery::InTurn, true, 1),
    ] {
        let mut f = Fixture::new();
        let row = f.create();
        f.change(
            crate::routines::Change::RunNow {
                bot: f.bot,
                routine: row.id,
                revision: 1,
            },
            10,
        );
        let work = f.claim(&row, 11);
        let event = if approval {
            p::AgentEvent::ApprovalRequired {
                turn_id: work.turn.id.to_string().parse().unwrap(),
                approval_id: p::ApprovalId::mint(),
                summary: "Approve".into(),
                choices: p::one_time_approval_choices(),
            }
        } else {
            p::AgentEvent::QuestionsRequired {
                turn_id: work.turn.id.to_string().parse().unwrap(),
                request_id: p::QuestionRequestId::mint(),
                questions: vec![p::AgentQuestion {
                    id: "q".into(),
                    header: String::new(),
                    question: "Choose".into(),
                    options: vec![],
                    allow_other: true,
                    secret: false,
                }],
                blocking,
                delivery,
            }
        };
        f.db.question_event(work.turn, &event, 12).unwrap();
        let summary = read(&f);
        assert_eq!(
            summary.open_requests, expected,
            "approval={approval}, blocking={blocking}, delivery={delivery:?}"
        );
        let workspace: String =
            f.db.connection
                .query_row(
                    "SELECT json_extract(CAST(access AS TEXT),'$.workspace') FROM bots WHERE id=?1",
                    [f.bot.to_string()],
                    |r| r.get(0),
                )
                .unwrap();
        let targets = lists::activity(&f.db.connection, &f.who, id(workspace).unwrap()).unwrap();
        let needed = targets["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["needs_you"] == true)
            .count();
        assert_eq!(needed as u64, summary.open_requests);
    }
}

#[test]
fn pending_results_follow_publication_and_skip_without_becoming_a_main_preview() {
    let mut f = Fixture::new();
    let row = f.create();
    let occurrence = f
        .change(
            crate::routines::Change::RunNow {
                bot: f.bot,
                routine: row.id,
                revision: 1,
            },
            10,
        )
        .occurrence
        .unwrap();
    assert_eq!(read(&f).pending_results, 0);
    let work = f.claim(&row, 11);
    f.publish(&work, 12);
    assert_eq!(read(&f).pending_results, 1);
    assert!(read(&f).latest.is_none());
    f.db.skip_routine_review(&f.who, Uuid::new_v4(), f.bot, occurrence.id, 13)
        .unwrap();
    assert_eq!(read(&f).pending_results, 0);
}

fn message(
    f: &Fixture,
    thread: Uuid,
    sequence: u64,
    sender: &str,
    kind: &str,
    state: &str,
    body: &str,
) -> Uuid {
    let key = Uuid::new_v4();
    f.db.connection
        .execute(
            "INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?3)",
            params![
                key.to_string(),
                thread.to_string(),
                sequence,
                sender,
                kind,
                serde_json::to_vec(&serde_json::json!({"body":body})).unwrap(),
                state
            ],
        )
        .unwrap();
    key
}

#[test]
fn preview_uses_only_published_own_main_words_and_is_bounded_and_redacted() {
    let f = Fixture::new();
    let main = f.main;
    assert!(read(&f).latest.is_none());
    let key = message(
        &f,
        main,
        1,
        &f.bot.to_string(),
        "words",
        "published",
        &"é".repeat(2000),
    );
    message(
        &f,
        main,
        2,
        &f.who.sender,
        "words",
        "accepted",
        "person words",
    );
    message(
        &f,
        main,
        3,
        &f.bot.to_string(),
        "thread_reference",
        "published",
        "routine card",
    );
    message(
        &f,
        main,
        4,
        &Uuid::new_v4().to_string(),
        "words",
        "published",
        "guest Bot words",
    );
    message(
        &f,
        main,
        5,
        &f.bot.to_string(),
        "words",
        "accepted",
        "unpublished words",
    );
    let summary = read(&f);
    let latest = summary.latest.as_ref().unwrap();
    assert_eq!(latest.message.as_uuid(), key);
    assert_eq!(latest.at_ms, 1);
    assert_eq!(latest.text, "é".repeat(512));
    assert!(!format!("{summary:?}").contains('é'));
    assert_eq!((summary.pending_results, summary.open_requests), (0, 0));
    let foreign = Person {
        owner: "other".into(),
        sender: f.who.sender.clone(),
    };
    assert!(
        page(
            &f.db.connection,
            &foreign,
            Uuid::new_v4(),
            &[f.bot.to_string()]
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn request_after_first_two_hundred_targets_is_counted_and_reachable() {
    let f = Fixture::new();
    let c = &f.db.connection;
    let workspace: String = c
        .query_row(
            "SELECT json_extract(CAST(access AS TEXT),'$.workspace') FROM bots WHERE id=?1",
            [f.bot.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    let workspace = id(workspace).unwrap();
    for i in 1..=205 {
        let thread = Uuid::from_u128(i).to_string();
        c.execute("INSERT INTO threads(id,bot,kind,title,lifecycle) VALUES (?1,?2,'conversation','','active')",
            params![thread, f.bot.to_string()]).unwrap();
        c.execute("INSERT INTO participants(thread,participant,kind) VALUES (?1,?2,'bot'),(?1,?3,'person')",
            params![thread, f.bot.to_string(), f.who.sender]).unwrap();
        c.execute(
            "INSERT INTO turns(id,thread,bot,grant,state,generation,details,created_at_ms)
            VALUES (?1,?2,?3,x'7b7d',?4,1,x'7b7d',1)",
            params![
                Uuid::new_v4().to_string(),
                thread,
                f.bot.to_string(),
                if i == 205 { "needs_you" } else { "running" }
            ],
        )
        .unwrap();
    }
    assert_eq!(read(&f).open_requests, 1);
    let first = lists::activity_page(c, &f.who, workspace, None).unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 200);
    assert!(
        first["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["needs_you"] == false)
    );
    let next = serde_json::from_value(first["next"].clone()).unwrap();
    let second = lists::activity_page(c, &f.who, workspace, next).unwrap();
    assert_eq!(second["items"].as_array().unwrap().len(), 5);
    assert_eq!(second["items"][4]["needs_you"], true);
    assert!(second["next"].is_null());
    // Person-invisible side threads are excluded; between-Bot requests remain
    // owner-visible even without a person participant row.
    let thread = Uuid::from_u128(205).to_string();
    c.execute(
        "DELETE FROM participants WHERE thread=?1 AND kind='person'",
        [&thread],
    )
    .unwrap();
    assert_eq!(read(&f).open_requests, 0);
    c.execute(
        "UPDATE threads SET kind='between_bots' WHERE id=?1",
        [&thread],
    )
    .unwrap();
    assert_eq!(read(&f).open_requests, 1);
    // The old generation, inactive thread and inactive Bot all lose attention.
    let thread = Uuid::from_u128(205).to_string();
    c.execute(
        "INSERT INTO turns(id,thread,bot,grant,state,generation,details,created_at_ms)
        VALUES (?1,?2,?3,x'7b7d','answered',2,x'7b7d',2)",
        params![Uuid::new_v4().to_string(), thread, f.bot.to_string()],
    )
    .unwrap();
    assert_eq!(read(&f).open_requests, 0);
    c.execute(
        "UPDATE turns SET state='needs_you' WHERE thread=?1 AND generation=2",
        [&thread],
    )
    .unwrap();
    c.execute(
        "UPDATE threads SET lifecycle='trashed' WHERE id=?1",
        [&thread],
    )
    .unwrap();
    assert_eq!(read(&f).open_requests, 0);
    c.execute(
        "UPDATE threads SET lifecycle='active' WHERE id=?1",
        [&thread],
    )
    .unwrap();
    c.execute(
        "UPDATE bots SET lifecycle='archived' WHERE id=?1",
        [f.bot.to_string()],
    )
    .unwrap();
    assert_eq!(read(&f).open_requests, 0);
}

#[test]
fn operation_attention_counts_only_with_a_current_needs_you_turn() {
    let (db, records) = crate::operations::tests::leased_fixture(1);
    let c = &db.connection;
    let (bot, owner, workspace, thread): (String, String, String, String) = c.query_row(
        "SELECT b.id,b.owner,json_extract(CAST(b.access AS TEXT),'$.workspace'),t.id FROM bots b JOIN threads t ON t.bot=b.id",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    let who = Person {
        owner,
        sender: "person".into(),
    };
    let workspace = id(workspace).unwrap();
    let check = |expected| {
        let summary = page(c, &who, workspace, &[bot.clone()]).unwrap();
        assert_eq!(summary[&id(bot.clone()).unwrap()].open_requests, expected);
        let activity = lists::activity(c, &who, workspace).unwrap();
        assert_eq!(
            activity["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v["needs_you"] == true)
                .count() as u64,
            expected
        );
    };
    assert_eq!(records.len(), 1);
    assert!(
        !db.operation_attention(&who, id(thread).unwrap(), None)
            .unwrap()
            .items
            .is_empty()
    );
    check(0);
    c.execute("UPDATE turns SET state='needs_you'", []).unwrap();
    check(1);
    c.execute("UPDATE turns SET state='answered'", []).unwrap();
    check(0);
}
