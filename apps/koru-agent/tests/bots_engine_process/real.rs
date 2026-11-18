// SPDX-License-Identifier: MPL-2.0
//! Opt-in installed ACP peer. Only synthetic context and this run's MCP door
//! reach the model: native tools, inherited settings and other MCP are disabled.
use super::stack::{Stack, until};
use koru_bots_engine::{messages, person};
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    process::{Command, Stdio},
    time::Duration,
};
use uuid::Uuid;

pub fn proxy() {
    let entry =
        std::env::var_os("BOTS_ENGINE_FIXTURE_PROVIDER_ENTRY").expect("explicit provider entry");
    let mut command = Command::new("node");
    command
        .arg(entry)
        .args(std::env::args_os().skip(1))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().unwrap();
    let mut output = child.stdout.take().unwrap();
    // The version query has no JSON input. Its result must be the installed
    // wrapper's actual result, never the scripted peer's compatibility print.
    if std::env::args().any(|a| a == "--version") {
        child.stdin.take();
        std::io::copy(&mut output, &mut std::io::stdout()).unwrap();
        assert!(child.wait().unwrap().success());
        return;
    }
    let mut input = child.stdin.take().unwrap();
    let forwarding = std::thread::spawn(move || {
        for line in std::io::BufReader::new(output).lines() {
            let line = line.unwrap();
            let packet: Value = serde_json::from_str(&line).unwrap();
            super::provider::trace(match packet["method"].as_str() {
                Some("session/update") => "out:update",
                Some("session/request_permission") => "out:permission",
                Some(_) => "out:other",
                None if !packet["error"].is_null() => "out:error",
                None => "out:response",
            });
            let mut stdout = std::io::stdout().lock();
            writeln!(stdout, "{line}").unwrap();
            stdout.flush().unwrap();
        }
    });
    for line in std::io::stdin().lock().lines() {
        let mut packet: Value = serde_json::from_str(&line.unwrap()).unwrap();
        super::provider::trace(match packet["method"].as_str() {
            Some("initialize") => "in:initialize",
            Some("session/new") => "in:new",
            Some("session/load") => "in:load",
            Some("session/prompt") => "in:prompt",
            Some("session/set_mode") => "in:mode",
            Some(_) => "in:other",
            None => "in:response",
        });
        if matches!(
            packet["method"].as_str(),
            Some("session/new" | "session/load")
        ) {
            let options = &mut packet["params"]["_meta"]["claudeCode"]["options"];
            options["tools"] = json!([]);
            options["settingSources"] = json!([]);
            options["strictMcpConfig"] = json!(true);
        }
        serde_json::to_writer(&mut input, &packet).unwrap();
        input.write_all(b"\n").unwrap();
        input.flush().unwrap();
    }
    drop(input);
    let _ = child.kill();
    let _ = child.wait();
    forwarding.join().unwrap();
}

pub(super) fn ends_with_marker(message: &Value, marker: &str) -> bool {
    // ACP may stream progress narration before its final answer. Completion
    // is checked separately through the settled snapshot and ledger audit.
    message["kind"] == "words"
        && message["content"]["body"]
            .as_str()
            .is_some_and(|body| body.trim().ends_with(marker))
}

pub(super) fn settled(stack: &Stack, thread: Uuid) -> bool {
    let view = stack.thread(koru_ai_wire::bot_threads::Command::Read {
        thread: koru_ai_core::ChatId::from_uuid(thread),
    });
    view["snapshot"]["turns"].as_array().unwrap().is_empty()
        && view["snapshot"]["waiting_messages"] == 0
}

pub fn run(stack: &Stack, bot: Uuid, thread: Uuid) {
    let operation = Uuid::new_v4();
    stack.send(messages::Envelope {
        id: Uuid::new_v4(), to: bot, thread: Some(thread), in_reply_to: None,
        body: format!("Synthetic integration verification. Use only this session's Koru MCP tools. Call create_document with operation_id {operation}, workspace_id {}, title Synthetic proof, and initial_operations [{{\"op\":\"insert\",\"node_id\":\"body\",\"parent_id\":\"root\",\"after_node_id\":null,\"kind\":\"paragraph\",\"text\":\"SYNTHETIC PROOF\"}}]. Repeat that exact call with the same operation_id and arguments to verify its receipt. Then read_document using the returned reference.document and this workspace. Finally reply exactly SYNTHETIC COMPLETE. Do not use other services or tools.", stack.workspace),
        attachments: vec![], references: vec![], deadline_ms: None, reply_to: Default::default(),
    });
    until(
        "installed provider publishes native write",
        Duration::from_secs(180),
        || {
            let questions = stack.thread(koru_ai_wire::bot_threads::Command::Questions {
                thread: koru_ai_core::ChatId::from_uuid(thread),
            });
            let questions: Vec<koru_ai_wire::bot_threads::Question> =
                serde_json::from_value(questions["questions"].clone()).unwrap();
            for question in questions.into_iter().filter(|q| q.state == "open") {
                let koru_ai_wire::bot_threads::Prompt::Approval { summary, choices } =
                    question.prompt
                else {
                    panic!("fixture expected only tool approval");
                };
                assert!(
                    summary.contains("create_document") || summary.contains("read_document"),
                    "fixture permits only its requested native tools"
                );
                let choice = choices
                    .iter()
                    .find(|c| c.intent == koru_session_protocol::ApprovalIntent::Allow)
                    .expect("one-time tool approval")
                    .id;
                stack.thread(koru_ai_wire::bot_threads::Command::Answer {
                    id: Uuid::new_v4(),
                    question: question.id,
                    answer: koru_ai_wire::bot_threads::Answer::Approval {
                        decision: koru_session_protocol::ApprovalDecision::Approve,
                        choice: Some(choice),
                    },
                });
            }
            let history = stack.person(person::Command::Read {
                query: person::Query::History { thread, after: 0 },
            });
            let history = history.as_array().unwrap();
            assert!(
                !history
                    .iter()
                    .any(|m| m["content"]["references"][0]["notice"] == "session_setup_failed"),
                "installed provider setup refused"
            );
            settled(stack, thread)
                && history.iter().any(|m| {
                    m["sender"] == json!(bot.to_string())
                        && ends_with_marker(m, "SYNTHETIC COMPLETE")
                })
        },
    );
    let outputs = stack.person(person::Command::Read {
        query: person::Query::Outputs {
            thread,
            after: None,
        },
    });
    assert_eq!(
        outputs["items"].as_array().unwrap().len(),
        1,
        "one applied native output"
    );
    println!("PASS installed ACP provider through production worker, native gateway and Store");
}
