// SPDX-License-Identifier: MPL-2.0
use super::stack::{Stack, until};
use koru_ai_core::ChatId;
use koru_ai_wire::bot_threads::Command;
use koru_bots_engine::{messages, person};
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

fn conversation(stack: &Stack, bot: Uuid) -> Uuid {
    let answer = stack.person(person::Command::Change {
        id: Uuid::new_v4(),
        action: person::Change::Conversation {
            bot,
            title: "Synthetic side conversation".into(),
        },
    });
    serde_json::from_value(answer["answer"]["id"].clone()).unwrap()
}
fn send(stack: &Stack, bot: Uuid, thread: Uuid, body: &str) {
    stack.send(messages::Envelope {
        id: Uuid::new_v4(),
        to: bot,
        thread: Some(thread),
        in_reply_to: None,
        body: body.into(),
        attachments: vec![],
        references: vec![json!({"kind":"fixture_workspace","workspace":stack.workspace})],
        deadline_ms: None,
        reply_to: Default::default(),
    });
}
fn history(stack: &Stack, thread: Uuid) -> Value {
    stack.thread(Command::History {
        thread: ChatId::from_uuid(thread),
        before: None,
    })["messages"]
        .clone()
}
fn completed(stack: &Stack, thread: Uuid) {
    until(
        "synthetic conversation publishes",
        Duration::from_secs(45),
        || {
            let messages = history(stack, thread);
            let messages = messages.as_array().unwrap();
            assert!(
                !messages
                    .iter()
                    .any(|m| m["content"]["references"][0]["notice"] == "session_setup_failed"),
                "provider setup failure"
            );
            messages
                .iter()
                .any(|m| m["content"]["body"] == "SYNTHETIC COMPLETE")
        },
    );
}
pub fn run(stack: &mut Stack, bot: Uuid) {
    let holding = conversation(stack, bot);
    send(stack, bot, holding, "HOLD");
    let mut turn = Value::Null;
    until("held turn running", Duration::from_secs(20), || {
        let snapshot = stack.thread(Command::Read {
            thread: ChatId::from_uuid(holding),
        });
        turn = snapshot["snapshot"]["turns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["state"] == "running")
            .cloned()
            .unwrap_or(Value::Null);
        !turn.is_null()
    });
    let other = conversation(stack, bot);
    send(stack, bot, other, "PARALLEL");
    completed(stack, other);
    assert_eq!(
        stack
            .status()
            .workers
            .iter()
            .filter(|w| w.bot == bot.to_string())
            .count(),
        1,
        "one worker serves both conversations"
    );
    let active: koru_ai_wire::bot_threads::ActiveTurn = serde_json::from_value(turn).unwrap();
    let stop_turn = koru_ai_wire::bot_threads::Turn {
        id: active.id,
        generation: active.generation,
        worker: active.worker.unwrap(),
        session: active.session.unwrap(),
    };
    stack.thread(Command::Stop {
        id: Uuid::new_v4(),
        turn: stop_turn,
    });
    until(
        "stopped thread has durable notice",
        Duration::from_secs(20),
        || {
            history(stack, holding).as_array().unwrap().iter().any(|m| {
                m["content"]["references"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r["notice"] == "turn_stopped")
            })
        },
    );
    println!("PASS two conversations share one worker; Stop preserves the other conversation");

    let recovering = conversation(stack, bot);
    send(stack, bot, recovering, "RECOVER");
    until(
        "native write applied before crash",
        Duration::from_secs(30),
        || stack.root.join("recovery.ready").exists(),
    );
    // The flag is released only after the old process is gone, so it cannot
    // publish in the gap before its engine is killed.
    stack.restart_after_write();
    completed(stack, recovering);
    let outputs = stack.person(person::Command::Read {
        query: person::Query::Outputs {
            thread: recovering,
            after: None,
        },
    });
    assert_eq!(
        outputs["items"].as_array().unwrap().len(),
        1,
        "rebuild did not repeat the applied native effect"
    );
    assert!(
        !history(stack, holding)
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["content"]["body"] == "SYNTHETIC COMPLETE"),
        "Stop survives restart"
    );
    println!(
        "PASS abrupt engine death after applied write; rebuilt input replays one native effect and publishes"
    );
}
