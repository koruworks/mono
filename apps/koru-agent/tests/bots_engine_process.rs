// SPDX-License-Identifier: MPL-2.0
//! Explicit disposable-root proof. Ordinary cargo test returns before I/O.
#[path = "bots_engine_process/provider.rs"]
mod provider;
#[path = "bots_engine_process/real.rs"]
mod real;
#[path = "bots_engine_process/scenarios.rs"]
mod scenarios;
#[path = "bots_engine_process/stack.rs"]
mod stack;
use koru_bots_engine::{messages, person};
use serde_json::json;
use std::{path::PathBuf, time::Duration};
use uuid::Uuid;

fn main() {
    if std::env::current_exe()
        .unwrap()
        .file_stem()
        .is_some_and(|n| n == "claude-agent-acp")
    {
        real::proxy();
        return;
    }
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--version") {
        provider::trace("version");
        println!("codex-cli 0.166.0");
        return;
    }
    if args.first().is_some_and(|arg| arg == "app-server") {
        provider::trace("app-server");
        provider::run();
        return;
    }
    if args.first().is_none_or(|arg| arg != "--run") {
        println!("SKIP engine integration proof: requires --run --root <new absolute path>");
        return;
    }
    assert!(cfg!(windows), "this proof qualifies Windows containment");
    assert!(matches!(args.len(), 3 | 5));
    assert_eq!(args[1], "--root");
    let root = PathBuf::from(&args[2]);
    assert!(root.is_absolute());
    std::fs::create_dir(&root).expect("new disposable root");
    let real = if args.len() == 5 {
        assert_eq!(args[3], "--provider-entry");
        let entry = PathBuf::from(&args[4]);
        assert!(entry.is_absolute() && entry.is_file());
        Some(entry)
    } else {
        None
    };
    let mut stack = stack::Stack::start(&root, real.as_deref());
    let bot = Uuid::new_v4();
    let created = stack.person(person::Command::Change {
        id: Uuid::new_v4(),
        action: person::Change::Create {
            bot,
            definition: person::Definition {
                name: "Synthetic integration Bot".into(),
                avatar: None,
                configuration: koru_bots_engine::configuration::Configuration {
                    provider: if real.is_some() { "claude" } else { "codex" }
                        .parse()
                        .unwrap(),
                    selection: Default::default(),
                    working_folder: None,
                },
                access: koru_delegate_core::CapturedBotPolicy {
                    workspace: stack.workspace,
                    destination: stack.workspace,
                    access: koru_delegate_core::BotAccess::Workspace,
                    selected_pages: Default::default(),
                    connections: Default::default(),
                },
                role: Some(stack.role),
                knowledge: vec![],
                memory: None,
            },
        },
    });
    let thread: Uuid = serde_json::from_value(created["answer"]["main_thread"].clone()).unwrap();
    if real.is_some() {
        real::run(&stack, bot, thread);
        stack.finish();
        return;
    }
    let attachment = Uuid::new_v4();
    let attachment_url = format!(
        "{}/v1/ai/chats/{thread}/attachments/{attachment}",
        stack.endpoint
    );
    let uploaded = stack
        .http()
        .put(&attachment_url)
        .bearer_auth(&stack.bearer)
        .query(&[("name", "synthetic.txt"), ("source", "drop")])
        .body("SYNTHETIC FILE")
        .send()
        .unwrap();
    assert!(uploaded.status().is_success(), "native attachment upload");
    let message = messages::Envelope {
        id: Uuid::new_v4(),
        to: bot,
        thread: Some(thread),
        in_reply_to: None,
        body: "SYNTHETIC integration input".into(),
        attachments: vec![json!(attachment)],
        references: vec![json!({"kind":"fixture_workspace","workspace":stack.workspace})],
        deadline_ms: None,
        reply_to: Default::default(),
    };
    let accepted = stack.send(message.clone());
    let replay = stack.send(message);
    assert_eq!(accepted["message"], replay["message"]);
    assert_eq!(replay["replayed"], true);
    stack::until(
        "real worker publishes after native Store write",
        Duration::from_secs(90),
        || {
            let history = stack.person(person::Command::Read {
                query: person::Query::History { thread, after: 0 },
            });
            assert!(
                !history
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|m| m["content"]["references"][0]["notice"] == "session_setup_failed"),
                "synthetic provider setup failed"
            );
            history
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message["content"]["body"] == "SYNTHETIC COMPLETE")
        },
    );
    let outputs = stack.person(person::Command::Read {
        query: person::Query::Outputs {
            thread,
            after: None,
        },
    });
    assert!(
        !outputs["items"].as_array().unwrap().is_empty(),
        "native write has a durable output"
    );
    println!(
        "PASS production engine, worker, tool door, gateway boundary and native Store publication"
    );
    let bytes = stack
        .http()
        .get(&attachment_url)
        .bearer_auth(&stack.bearer)
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .bytes()
        .unwrap();
    assert_eq!(&bytes[..], b"SYNTHETIC FILE");
    let removed = stack
        .http()
        .delete(&attachment_url)
        .bearer_auth(&stack.bearer)
        .send()
        .unwrap();
    assert!(
        !removed.status().is_success(),
        "published attachment cannot be removed"
    );
    println!("PASS public HTTP Send/replay and native attachment delivery/read protection");
    scenarios::run(&mut stack, bot);
    stack.finish();
}
