// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::{authority::Binding, person};
mod returns;

struct Fixture {
    db: Database,
    person: Person,
    workspace: koru_delegate_core::WorkspaceId,
    bots: Vec<(Uuid, Uuid)>,
}
impl Fixture {
    fn new(count: usize) -> Self {
        let mut f = Self {
            db: Database::in_memory().unwrap(),
            person: Person {
                owner: "fixture-owner".into(),
                sender: "fixture-person".into(),
            },
            workspace: koru_delegate_core::WorkspaceId::new(),
            bots: vec![],
        };
        for _ in 0..count {
            f.add();
        }
        f
    }
    fn add(&mut self) {
        let bot = Uuid::new_v4();
        let d = person::Definition {
            name: "Fixture".into(),
            avatar: None,
            configuration: crate::configuration::Configuration {
                provider: "codex".parse().unwrap(),
                selection: Default::default(),
                working_folder: None,
            },
            access: koru_delegate_core::CapturedBotPolicy {
                workspace: self.workspace,
                destination: self.workspace,
                access: koru_delegate_core::BotAccess::Workspace,
                selected_pages: Default::default(),
                connections: Default::default(),
            },
            role: None,
            knowledge: vec![],
            memory: None,
        };
        let created =
            person::change::create(&self.db.connection, &self.person, bot, &d, 1).unwrap();
        self.bots.push((
            bot,
            serde_json::from_value(created["main_thread"].clone()).unwrap(),
        ));
    }
    fn send(&mut self, body: String) -> Accepted {
        self.db
            .accept_message(
                &self.person,
                &Envelope {
                    id: Uuid::new_v4(),
                    to: self.bots[0].0,
                    thread: Some(self.bots[0].1),
                    in_reply_to: None,
                    body,
                    attachments: vec![],
                    references: vec![],
                    deadline_ms: None,
                    reply_to: ReplyTo::Thread,
                },
                2,
            )
            .unwrap()
            .answer
    }
    fn claim(&mut self, bot: Uuid, thread: Uuid) -> Delivery {
        let d = self
            .db
            .claim_messages(
                bot,
                thread,
                Uuid::new_v4(),
                Uuid::new_v4(),
                Resume::Rebuilt,
                65536,
                3,
            )
            .unwrap()
            .unwrap();
        self.db.delivery_accepted(d.turn, d.id).unwrap();
        d
    }
    fn identity(&self, d: &Delivery, bot: Uuid, thread: Uuid) -> Resolution {
        Resolution {
            scope: self.person.owner.clone(),
            bot,
            thread,
            binding: Binding {
                id: Uuid::new_v4(),
                turn: d.turn,
            },
            captured_grant: json!({}),
            current_grant: json!({}),
        }
    }
    fn ask(&mut self, d: &Delivery, from: (Uuid, Uuid), to: Uuid) -> (Ask, Value) {
        let request = Ask {
            id: Uuid::new_v4(),
            to,
            source_message: d.work[0].id,
            body: "Synthetic request".into(),
            thread: None,
            deadline_ms: None,
        };
        let identity = self.identity(d, from.0, from.1);
        let result = ask(&mut self.db, &identity, &request, 4).unwrap();
        (request, result)
    }
    fn publish(&mut self, d: &Delivery) -> Receipt<Published> {
        self.db
            .publish_reply(
                &Publish {
                    command: Uuid::new_v4(),
                    turn: d.turn,
                    body: "Synthetic answer".into(),
                    outputs: vec![],
                },
                5,
            )
            .unwrap()
    }
    fn count(&self, sql: &str) -> u64 {
        self.db.connection.query_row(sql, [], |r| r.get(0)).unwrap()
    }
}

#[test]
fn side_answer_continues_origin_without_holding_asking_turn_or_reply_loop() {
    let mut f = Fixture::new(2);
    f.send("begin".into());
    let a = f.bots[0];
    let b = f.bots[1];
    let first = f.claim(a.0, a.1);
    let (request, sent) = f.ask(&first, a, b.0);
    let side: Uuid = serde_json::from_value(sent["thread"].clone()).unwrap();
    assert_ne!(side, a.1);
    assert_eq!(
        f.count("SELECT count(*) FROM turns WHERE state='running'"),
        1
    );
    let identity = f.identity(&first, a.0, a.1);
    assert_eq!(ask(&mut f.db, &identity, &request, 6).unwrap(), sent);
    let mut conflict = request.clone();
    conflict.body.push('!');
    assert_eq!(
        ask(&mut f.db, &identity, &conflict, 6).err(),
        Some(Error::Conflict)
    );
    f.publish(&first);
    let second = f.claim(b.0, side);
    let answer = f.publish(&second).answer.message;
    let continuation = f.claim(a.0, a.1);
    assert_eq!(continuation.work[0].id, answer);
    assert_eq!(continuation.work[0].sender, b.0.to_string());
    assert_eq!(f.count("SELECT count(*) FROM inbox"), 1);
    f.publish(&continuation);
    assert_eq!(f.count("SELECT count(*) FROM inbox"), 0);
    assert_eq!(f.count("SELECT count(*) FROM bot_dispatches"), 1);
}

#[test]
fn mentions_fan_out_atomically_and_each_bot_catches_up_without_extra_work() {
    let mut f = Fixture::new(3);
    let a = f.bots[0];
    let b = f.bots[1];
    let c = f.bots[2];
    f.send("earlier context".into());
    let first = f.claim(a.0, a.1);
    f.publish(&first);
    let named = f.send(format!(
        "[[koru://bot/{}#Second]] [[koru://bot/{}#Third]]",
        b.0, c.0
    ));
    assert_eq!(f.count("SELECT count(*) FROM inbox"), 2);
    let second = f.claim(b.0, a.1);
    let third = f.claim(c.0, a.1);
    assert_eq!(second.work.len(), 1);
    assert_eq!(second.work[0].id, named.message);
    assert_eq!(third.work[0].id, named.message);
    assert!(
        second
            .context
            .iter()
            .any(|m| m.content.body == "earlier context")
    );
    f.publish(&second);
    f.db.recover_interrupted_turns().unwrap();
    let resumed = f.claim(c.0, a.1);
    assert_eq!(resumed.work[0].id, named.message);
    assert_eq!(
        f.count("SELECT count(*) FROM obligations WHERE state='answered'"),
        2
    );
    f.publish(&resumed);
    assert_eq!(f.count("SELECT count(*) FROM inbox"), 0);
}

#[test]
fn stopped_trashed_or_expired_origin_keeps_answer_without_wake() {
    for reason in ["stop", "trash", "deadline"] {
        let mut f = Fixture::new(2);
        f.send("begin".into());
        let a = f.bots[0];
        let b = f.bots[1];
        let first = f.claim(a.0, a.1);
        let request = Ask {
            id: Uuid::new_v4(),
            to: b.0,
            source_message: first.work[0].id,
            body: "work".into(),
            thread: None,
            deadline_ms: if reason == "deadline" { Some(4) } else { None },
        };
        let identity = f.identity(&first, a.0, a.1);
        let sent = ask(&mut f.db, &identity, &request, 4).unwrap();
        let side = serde_json::from_value(sent["thread"].clone()).unwrap();
        let second = f.claim(b.0, side);
        match reason {
            "stop" => {
                f.db.stop_turn(&f.person, Uuid::new_v4(), first.turn, 4)
                    .unwrap();
            }
            "trash" => {
                f.db.connection
                    .execute(
                        "UPDATE threads SET lifecycle='trashed' WHERE id=?1",
                        [a.1.to_string()],
                    )
                    .unwrap();
            }
            _ => (),
        }
        f.publish(&second);
        assert_eq!(
            f.count("SELECT count(*) FROM obligations WHERE state='owed'"),
            0
        );
        assert!(
            f.db.read_messages_for_bot(b.0, side, 0)
                .unwrap()
                .iter()
                .any(|m| m.content.body == "Synthetic answer")
        );
    }
}

#[test]
fn asks_check_work_identity_owner_workspace_permission_and_roll_back() {
    let mut f = Fixture::new(2);
    f.send("begin".into());
    let a = f.bots[0];
    let b = f.bots[1];
    let first = f.claim(a.0, a.1);
    let identity = f.identity(&first, a.0, a.1);
    let mut request = Ask {
        id: Uuid::new_v4(),
        to: b.0,
        source_message: Uuid::new_v4(),
        body: "work".into(),
        thread: None,
        deadline_ms: None,
    };
    assert_eq!(
        ask(&mut f.db, &identity, &request, 4).err(),
        Some(Error::NotAllowed)
    );
    request.source_message = first.work[0].id;
    f.db.connection
        .execute("UPDATE bots SET may_ask=0 WHERE id=?1", [a.0.to_string()])
        .unwrap();
    assert_eq!(
        ask(&mut f.db, &identity, &request, 4).err(),
        Some(Error::NotAllowed)
    );
    f.db.connection
        .execute("UPDATE bots SET may_ask=1 WHERE id=?1", [a.0.to_string()])
        .unwrap();
    f.db.connection
        .execute(
            "UPDATE bots SET owner='foreign' WHERE id=?1",
            [b.0.to_string()],
        )
        .unwrap();
    assert_eq!(
        ask(&mut f.db, &identity, &request, 4).err(),
        Some(Error::NotAllowed)
    );
    f.db.connection
        .execute(
            "UPDATE bots SET owner=?1 WHERE id=?2",
            params![f.person.owner, b.0.to_string()],
        )
        .unwrap();
    f.db.connection.execute_batch("CREATE TEMP TRIGGER fail_dispatch BEFORE INSERT ON bot_dispatches BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(ask(&mut f.db, &identity, &request, 4).is_err());
    assert_eq!(f.count("SELECT count(*) FROM threads"), 2);
    assert_eq!(f.count("SELECT count(*) FROM inbox"), 1);
    assert_eq!(f.count("SELECT count(*) FROM turn_recipients"), 0);
}

#[test]
fn three_recipients_per_turn_and_eight_bots_across_new_threads() {
    let mut f = Fixture::new(9);
    f.send("begin".into());
    let a = f.bots[0];
    let first = f.claim(a.0, a.1);
    for i in 1..=3 {
        let to = f.bots[i].0;
        assert_eq!(f.ask(&first, a, to).1["status"], "accepted");
    }
    let to = f.bots[4].0;
    assert_eq!(f.ask(&first, a, to).1["code"], "bot_chain_limit");
    // A fresh chain of asks, each in a newly created thread, still admits
    // only eight distinct Bots rather than resetting on each new thread.
    let mut f = Fixture::new(9);
    f.send("begin".into());
    let mut current = f.bots[0];
    for index in 1..=8 {
        let d = f.claim(current.0, current.1);
        let to = f.bots[index].0;
        let sent = f.ask(&d, current, to).1;
        if index == 8 {
            assert_eq!(sent["code"], "bot_chain_limit");
        } else {
            current = (to, serde_json::from_value(sent["thread"].clone()).unwrap());
        }
    }
    assert_eq!(f.count("SELECT count(*) FROM bot_chains"), 1);
}

#[test]
fn twenty_bot_messages_survive_new_threads_and_person_intervention_resets() {
    let mut f = Fixture::new(2);
    f.send("begin".into());
    let a = f.bots[0];
    let b = f.bots[1];
    let first = f.claim(a.0, a.1);
    for _ in 0..20 {
        assert_eq!(f.ask(&first, a, b.0).1["status"], "accepted");
    }
    let (refused, answer) = f.ask(&first, a, b.0);
    assert_eq!(answer["code"], "bot_chain_limit");
    f.send("person intervenes".into());
    let identity = f.identity(&first, a.0, a.1);
    assert_eq!(
        ask(&mut f.db, &identity, &refused, 7).unwrap(),
        answer,
        "refusal receipt stays immutable"
    );
    assert_eq!(f.ask(&first, a, b.0).1["status"], "accepted");
}
