// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::authority::{Call, Secret};
use serde_json::{Value, json};

fn session(f: &mut Fixture) -> (Worker, Secret, messages::Turn) {
    f.db.accept_message(
        &f.who,
        &messages::Envelope {
            id: Uuid::new_v4(),
            to: f.bot,
            thread: Some(f.main),
            in_reply_to: None,
            body: "Prepare a daily briefing".into(),
            attachments: vec![],
            references: vec![],
            deadline_ms: None,
            reply_to: messages::ReplyTo::Thread,
        },
        1,
    )
    .unwrap();
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
    let issued =
        f.db.mint_session(
            worker,
            Mint {
                command: Uuid::new_v4(),
                thread: f.main,
                predecessor: None,
            },
            2,
        )
        .unwrap();
    let turn =
        f.db.claim_session_messages(
            worker,
            issued.session,
            messages::Resume::Rebuilt,
            128 * 1024,
            3,
        )
        .unwrap()
        .unwrap()
        .turn;
    (worker, issued.secret.unwrap(), turn)
}
fn call(
    f: &mut Fixture,
    worker: Worker,
    secret: &Secret,
    name: &str,
    args: Value,
) -> Result<Value, Error> {
    let call = Call::parse(
        Uuid::new_v4(),
        name.into(),
        &serde_json::to_vec(&args).unwrap(),
    )?;
    let binding = f.db.bind_call(worker, secret, &call, Uuid::new_v4(), 4)?;
    crate::own_tools::execute(&mut f.db, secret, binding.id, &call, 5)
}
fn create() -> Value {
    json!({"id":Uuid::new_v4(),"expected_bot_revision":1,"name":"Briefing","objective":"Prepare a briefing","recurrence":{"type":"zoned","cron":"0 9 * * *","zone":"UTC"}})
}

#[test]
fn attention_tool_uses_the_bound_routine_turn_and_never_prose() {
    let mut f = Fixture::new();
    let routine = f.create();
    let occurrence = f
        .change(
            Change::RunNow {
                bot: f.bot,
                routine: routine.id,
                revision: routine.revision,
            },
            1,
        )
        .occurrence
        .unwrap();
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
    let issued =
        f.db.mint_session(
            worker,
            Mint {
                command: Uuid::new_v4(),
                thread: routine.thread,
                predecessor: None,
            },
            2,
        )
        .unwrap();
    let delivery =
        f.db.claim_session_messages(
            worker,
            issued.session,
            messages::Resume::Rebuilt,
            128 * 1024,
            3,
        )
        .unwrap()
        .unwrap();
    f.db.delivery_accepted(delivery.turn, delivery.id).unwrap();
    let secret = issued.secret.unwrap();
    let arguments = json!({"id":Uuid::new_v4(),"occurrence":occurrence.id,"needs_person":false});
    assert_eq!(
        call(
            &mut f,
            worker,
            &secret,
            "set_routine_attention",
            arguments.clone()
        )
        .unwrap()["needs_person"],
        false
    );
    let mut forged = arguments.clone();
    forged["bot"] = json!(Uuid::new_v4());
    assert!(call(&mut f, worker, &secret, "set_routine_attention", forged).is_err());
    assert!(
        call(
            &mut f,
            worker,
            &secret,
            "set_routine_attention",
            json!({"id":Uuid::new_v4(),"occurrence":occurrence.id,"needs_person":"false"})
        )
        .is_err()
    );
    f.db.publish_reply(
        &messages::Publish {
            command: Uuid::new_v4(),
            turn: delivery.turn,
            body: "Attention required according to these arbitrary words".into(),
            outputs: vec![],
        },
        6,
    )
    .unwrap();
    assert_eq!(f.db.routine_reviews(&f.who, f.bot, None).unwrap().count, 0);
}

#[test]
fn bound_bot_creation_is_enabled_atomic_and_replayable_and_edit_keeps_human_controls() {
    let mut f = Fixture::new();
    let (worker, secret, turn) = session(&mut f);
    let request = create();
    let created = call(&mut f, worker, &secret, "create_routine", request.clone()).unwrap();
    let routine: Routine = serde_json::from_value(created["routine"].clone()).unwrap();
    assert!(routine.enabled);
    assert_eq!(routine.access.access, BotAccess::Default);
    assert!(routine.access.selected_pages.is_empty());
    assert_eq!(
        created,
        call(&mut f, worker, &secret, "create_routine", request.clone()).unwrap()
    );
    let mut changed = request;
    changed["objective"] = json!("A different task");
    assert_eq!(
        call(&mut f, worker, &secret, "create_routine", changed).err(),
        Some(Error::Conflict)
    );
    let enabled = f.enable(&routine);
    let edit = json!({"id":Uuid::new_v4(),"routine":routine.id,"revision":enabled.revision,"name":"Evening briefing","objective":"Review the day","recurrence":{"type":"zoned","cron":"0 18 * * *","zone":"UTC"}});
    let edited = call(&mut f, worker, &secret, "edit_routine", edit.clone()).unwrap();
    let updated: Routine = serde_json::from_value(edited["routine"].clone()).unwrap();
    assert!(updated.enabled);
    assert_eq!(updated.access, enabled.access);
    assert_eq!(updated.grant_revision, enabled.grant_revision);
    assert_eq!(
        edited,
        call(&mut f, worker, &secret, "edit_routine", edit).unwrap()
    );
    let list = call(&mut f, worker, &secret, "list_routines", json!({})).unwrap();
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["bot_revision"], 1);
    f.db.stop_turn(&f.who, Uuid::new_v4(), turn, 8).unwrap();
    assert!(call(&mut f, worker, &secret, "create_routine", create()).is_err());
}

#[test]
fn bot_tools_reject_nominated_identity_access_enable_and_foreign_routine() {
    let mut f = Fixture::new();
    let (worker, secret, _) = session(&mut f);
    for (field, value) in [
        ("bot", json!(Uuid::new_v4())),
        ("enabled", json!(true)),
        ("documents", json!([Uuid::new_v4()])),
        ("access", json!({})),
    ] {
        let mut request = create();
        request[field] = value;
        assert_eq!(
            call(&mut f, worker, &secret, "create_routine", request).err(),
            Some(Error::InvalidRequest)
        );
    }
    let unknown = json!({"id":Uuid::new_v4(),"routine":Uuid::new_v4(),"revision":1,"name":"Other","objective":"Other task","recurrence":{"type":"zoned","cron":"0 9 * * *","zone":"UTC"}});
    assert_eq!(
        call(&mut f, worker, &secret, "edit_routine", unknown).err(),
        Some(Error::NotAllowed)
    );
    assert_eq!(
        f.db.routines(&f.who, f.bot, None, 16).unwrap().items.len(),
        0
    );
}

#[test]
fn failure_before_receipt_commit_leaves_no_definition_or_thread() {
    let mut f = Fixture::new();
    let (worker, secret, _) = session(&mut f);
    let before: u64 =
        f.db.connection
            .query_row("SELECT count(*) FROM threads", [], |r| r.get(0))
            .unwrap();
    let request = create();
    f.db.connection.execute_batch("CREATE TEMP TRIGGER fail_routine_receipt BEFORE INSERT ON receipts WHEN NEW.sender LIKE 'bot:%' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(call(&mut f, worker, &secret, "create_routine", request.clone()).is_err());
    let after: u64 =
        f.db.connection
            .query_row("SELECT count(*) FROM threads", [], |r| r.get(0))
            .unwrap();
    assert_eq!(before, after);
    assert!(
        f.db.routines(&f.who, f.bot, None, 16)
            .unwrap()
            .items
            .is_empty()
    );
    f.db.connection
        .execute_batch("DROP TRIGGER fail_routine_receipt;")
        .unwrap();
    assert!(call(&mut f, worker, &secret, "create_routine", request).is_ok());
}

#[test]
fn main_bot_triggers_existing_routine_with_exact_receipt_and_independent_access() {
    let mut f = Fixture::new();
    let row = f.create();
    let paused = f
        .change(
            Change::Enable {
                bot: f.bot,
                routine: row.id,
                revision: row.revision,
                enabled: false,
            },
            2,
        )
        .routine;
    let (worker, secret, _) = session(&mut f);
    let request = json!({"id":Uuid::new_v4(),"routine":row.id,"revision":paused.revision});
    let result = call(&mut f, worker, &secret, "run_routine", request.clone()).unwrap();
    let changed: Changed = serde_json::from_value(result.clone()).unwrap();
    let occurrence = changed.occurrence.unwrap();
    assert_eq!(occurrence.state, "queued");
    assert_eq!(changed.routine.thread, row.thread);
    assert!(!changed.routine.enabled);
    assert_eq!(changed.routine.access, row.access);
    assert_eq!(
        result,
        call(&mut f, worker, &secret, "run_routine", request.clone()).unwrap()
    );
    let mut wrong = request;
    wrong["revision"] = json!(paused.revision + 1);
    assert_eq!(
        call(&mut f, worker, &secret, "run_routine", wrong).err(),
        Some(Error::Conflict)
    );
    let busy = call(
        &mut f,
        worker,
        &secret,
        "run_routine",
        json!({"id":Uuid::new_v4(),"routine":row.id,"revision":paused.revision}),
    )
    .unwrap();
    assert_eq!(busy["occurrence"]["state"], "skipped");
    assert_eq!(busy["occurrence"]["reason"], "busy");
    assert_ne!(busy["occurrence"]["id"], result["occurrence"]["id"]);
    assert_eq!(
        f.history(&row)
            .iter()
            .filter(|o| o.state == "queued")
            .count(),
        1
    );
}

#[test]
fn routine_trigger_never_bypasses_target_owner_pause_or_thread_busy() {
    let mut f = Fixture::new();
    let row = f.create();
    let (worker, secret, _) = session(&mut f);
    assert_eq!(
        call(
            &mut f,
            worker,
            &secret,
            "run_routine",
            json!({"id":Uuid::new_v4(),"routine":Uuid::new_v4(),"revision":1})
        )
        .err(),
        Some(Error::NotAllowed)
    );
    let arguments = || json!({"id":Uuid::new_v4(),"routine":row.id,"revision":1});
    f.db.connection
        .execute("UPDATE bots SET paused=1 WHERE id=?1", [f.bot.to_string()])
        .unwrap();
    assert!(call(&mut f, worker, &secret, "run_routine", arguments()).is_err());
    f.db.connection
        .execute("UPDATE bots SET paused=0 WHERE id=?1", [f.bot.to_string()])
        .unwrap();
    // The Routine's own live turn is busy even when nothing is queued.
    f.change(
        Change::RunNow {
            bot: f.bot,
            routine: row.id,
            revision: 1,
        },
        8,
    );
    let issued =
        f.db.mint_session(
            worker,
            Mint {
                command: Uuid::new_v4(),
                thread: row.thread,
                predecessor: None,
            },
            10,
        )
        .unwrap();
    let delivery =
        f.db.claim_session_messages(
            worker,
            issued.session,
            messages::Resume::Rebuilt,
            128 * 1024,
            11,
        )
        .unwrap()
        .unwrap();
    f.db.delivery_accepted(delivery.turn, delivery.id).unwrap();
    let own = call(
        &mut f,
        worker,
        &issued.secret.unwrap(),
        "run_routine",
        arguments(),
    )
    .unwrap();
    assert_eq!(own["occurrence"]["state"], "skipped");
    assert_eq!(own["occurrence"]["reason"], "busy");
    assert_eq!(
        f.history(&row)
            .iter()
            .filter(|o| o.state == "running")
            .count(),
        1
    );
}
