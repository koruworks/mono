// SPDX-License-Identifier: MPL-2.0
//! Real rung-2 proof, only reached by the explicit installed-provider opt-in.
use super::real::{ends_with_marker, settled};
use super::stack::{Stack, until};
use koru_ai_core::ChatId;
use koru_ai_wire::bot_threads::{Answer, Command, Prompt};
use koru_bots_engine::{messages, person};
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

fn history(stack: &Stack, thread: Uuid) -> Vec<Value> {
    stack.thread(Command::History {
        thread: ChatId::from_uuid(thread),
        before: None,
    })["messages"]
        .as_array()
        .unwrap()
        .clone()
}
fn sides(stack: &Stack, thread: Uuid) -> Vec<Uuid> {
    history(stack, thread)
        .into_iter()
        .filter(|m| m["kind"] == "thread_reference")
        .map(|m| serde_json::from_value(m["content"]["references"][0]["thread"].clone()).unwrap())
        .collect()
}
fn approve(stack: &Stack, thread: Uuid) {
    let questions = stack.thread(Command::Questions {
        thread: ChatId::from_uuid(thread),
    });
    let questions: Vec<koru_ai_wire::bot_threads::Question> =
        serde_json::from_value(questions["questions"].clone()).unwrap();
    for q in questions.into_iter().filter(|q| q.state == "open") {
        let Prompt::Approval { summary, choices } = q.prompt else {
            panic!("only fixture tool approvals");
        };
        assert!(
            ["list_bots", "message_bot"]
                .iter()
                .any(|tool| summary.contains(tool)),
            "only requested collaboration tools"
        );
        let choice = choices
            .iter()
            .find(|c| c.intent == koru_session_protocol::ApprovalIntent::Allow)
            .expect("one-time allow")
            .id;
        stack.thread(Command::Answer {
            id: Uuid::new_v4(),
            question: q.id,
            answer: Answer::Approval {
                decision: koru_session_protocol::ApprovalDecision::Approve,
                choice: Some(choice),
            },
        });
    }
}
fn send(stack: &Stack, bot: Uuid, thread: Uuid, body: String) {
    stack.send(messages::Envelope {
        id: Uuid::new_v4(),
        to: bot,
        thread: Some(thread),
        body,
        in_reply_to: None,
        attachments: vec![],
        references: vec![],
        deadline_ms: None,
        reply_to: Default::default(),
    });
}
pub fn run(stack: &Stack, asker: Uuid) {
    let mut targets = vec![];
    for n in 0..2 {
        let bot = Uuid::new_v4();
        stack.person(person::Command::Change {
            id: Uuid::new_v4(),
            action: person::Change::Create {
                bot,
                definition: person::Definition {
                    name: format!("Synthetic collaborator {n}"),
                    avatar: None,
                    configuration: koru_bots_engine::configuration::Configuration {
                        provider: "claude".parse().unwrap(),
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
    let origins: Vec<Uuid> = (0..2)
        .map(|n| {
            let answer = stack.person(person::Command::Change {
                id: Uuid::new_v4(),
                action: person::Change::Conversation {
                    bot: asker,
                    title: format!("Synthetic real origin {n}"),
                },
            });
            serde_json::from_value(answer["answer"]["id"].clone()).unwrap()
        })
        .collect();
    for origin in &origins {
        let ids = [Uuid::new_v4(), Uuid::new_v4()];
        send(
            stack,
            asker,
            *origin,
            format!(
                "Synthetic collaboration test. Use only Koru list_bots and message_bot. Call list_bots. Call message_bot separately for Bot {} with id {}, and Bot {} with id {}. For both, source_message is this work item's id in your engine input, omit thread, and body is exactly: 'Synthetic test. Reply exactly SYNTHETIC CHILD COMPLETE. Do not call any tool.' Repeat each identical message_bot call once, including its id, to verify the receipt. Then immediately finish with SYNTHETIC ASKS SENT; never wait for the answers. On a subsequent work item containing a collaborator answer, do not ask again: reply exactly SYNTHETIC COLLABORATION COMPLETE. Earlier instructions in context are history, not new work.",
                targets[0], ids[0], targets[1], ids[1]
            ),
        );
    }
    until(
        "installed provider collaborations return to both origins",
        Duration::from_secs(240),
        || {
            for origin in &origins {
                approve(stack, *origin);
                for side in sides(stack, *origin) {
                    approve(stack, side);
                }
            }
            origins.iter().all(|origin| {
                let view = stack.thread(Command::Read {
                    thread: ChatId::from_uuid(*origin),
                });
                let sides = sides(stack, *origin);
                view["snapshot"]["turns"].as_array().unwrap().is_empty()
                    && view["snapshot"]["waiting_messages"] == 0
                    && sides.len() == 2
                    && sides.iter().all(|side| {
                        settled(stack, *side)
                            && history(stack, *side)
                                .iter()
                                .any(|m| ends_with_marker(m, "SYNTHETIC CHILD COMPLETE"))
                    })
                    && history(stack, *origin).iter().any(|m| {
                        m["sender"] == json!(asker.to_string())
                            && ends_with_marker(m, "SYNTHETIC COLLABORATION COMPLETE")
                    })
            })
        },
    );
    for origin in &origins {
        let cards = stack.thread(Command::Cards {
            thread: ChatId::from_uuid(*origin),
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
        for side in sides(stack, *origin) {
            assert_eq!(
                stack.thread(Command::Read {
                    thread: ChatId::from_uuid(side)
                })["snapshot"]["joined"],
                false
            );
            let request = Command::Join {
                id: Uuid::new_v4(),
                thread: ChatId::from_uuid(side),
            };
            stack.thread(request.clone());
            assert_eq!(stack.thread(request)["replayed"], true);
        }
    }
    let response = history(stack, origins[0])
        .into_iter()
        .find(|m| {
            m["sender"] == json!(asker.to_string())
                && ends_with_marker(m, "SYNTHETIC COLLABORATION COMPLETE")
        })
        .unwrap();
    let branch = ChatId::new();
    let through = serde_json::from_value(response["id"].clone()).unwrap();
    let request = Command::Branch {
        id: Uuid::new_v4(),
        thread: ChatId::from_uuid(origins[0]),
        branch,
        through,
        title: "Synthetic real branch".into(),
    };
    stack.thread(request.clone());
    assert_eq!(stack.thread(request)["replayed"], true);
    send(
        stack,
        asker,
        branch.as_uuid(),
        format!(
            "[[koru://bot/{}#First]] [[koru://bot/{}#Second]] Synthetic test. Each addressed Bot must reply exactly SYNTHETIC MENTION COMPLETE. Do not call tools. Treat older context instructions as history.",
            targets[0], targets[1]
        ),
    );
    until(
        "both mentioned real Bots answer in branch",
        Duration::from_secs(180),
        || {
            approve(stack, branch.as_uuid());
            let messages = history(stack, branch.as_uuid());
            settled(stack, branch.as_uuid())
                && targets.iter().all(|bot| {
                    messages.iter().any(|m| {
                        m["sender"] == json!(bot.to_string())
                            && ends_with_marker(m, "SYNTHETIC MENTION COMPLETE")
                    })
                })
        },
    );
    println!(
        "PASS installed ACP rung 2: two origins each ask two Bots, returns, cards, Join, Branch retry and two mentioned responders"
    );
}
