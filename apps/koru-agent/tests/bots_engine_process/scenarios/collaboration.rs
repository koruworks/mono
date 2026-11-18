// SPDX-License-Identifier: MPL-2.0
use super::*;

pub fn run(stack: &Stack, asker: Uuid) {
    let mut targets = vec![];
    for _ in 0..2 {
        let bot = Uuid::new_v4();
        stack.person(person::Command::Change {
            id: Uuid::new_v4(),
            action: person::Change::Create {
                bot,
                definition: person::Definition {
                    name: "Synthetic collaborator".into(),
                    avatar: None,
                    configuration: koru_bots_engine::configuration::Configuration {
                        provider: "codex".parse().unwrap(),
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
        targets.push(bot);
    }
    let origins = [conversation(stack, asker), conversation(stack, asker)];
    for thread in origins {
        stack.send(messages::Envelope {
            id: Uuid::new_v4(),
            to: asker,
            thread: Some(thread),
            in_reply_to: None,
            body: "COLLABORATE".into(),
            attachments: vec![],
            references: vec![json!({"targets":targets})],
            deadline_ms: None,
            reply_to: Default::default(),
        });
    }
    for thread in origins {
        until(
            "Bot answers return to their origin",
            Duration::from_secs(45),
            || {
                history(stack, thread)
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|m| m["content"]["body"] == "SYNTHETIC COLLABORATION COMPLETE")
            },
        );
        let messages = history(stack, thread);
        let sides: Vec<Uuid> = messages
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["kind"] == "thread_reference")
            .map(|m| {
                serde_json::from_value(m["content"]["references"][0]["thread"].clone()).unwrap()
            })
            .collect();
        assert_eq!(sides.len(), 2, "one side per recipient despite tool retry");
        for side in sides {
            until(
                "side answer is readable without joining",
                Duration::from_secs(30),
                || {
                    history(stack, side)
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|m| m["content"]["body"] == "SYNTHETIC CHILD COMPLETE")
                },
            );
            let snapshot = stack.thread(Command::Read {
                thread: ChatId::from_uuid(side),
            });
            assert_eq!(snapshot["snapshot"]["joined"], false);
            let join = Command::Join {
                id: Uuid::new_v4(),
                thread: ChatId::from_uuid(side),
            };
            stack.thread(join.clone());
            assert_eq!(stack.thread(join)["replayed"], true);
            assert_eq!(
                stack.thread(Command::Read {
                    thread: ChatId::from_uuid(side)
                })["snapshot"]["joined"],
                true
            );
        }
        let cards = stack.thread(Command::Cards {
            thread: ChatId::from_uuid(thread),
            after: None,
        });
        assert_eq!(
            cards["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|c| c["kind"] == "thread")
                .count(),
            2
        );
    }
    println!(
        "PASS two origins each ask two Bots through authenticated tools; side answers return to each origin"
    );
}
