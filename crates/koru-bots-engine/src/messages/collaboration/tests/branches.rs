// SPDX-License-Identifier: MPL-2.0
use super::*;

fn command(f: &mut Fixture, command: person::Command) -> Result<Value, Error> {
    person::handle(
        &mut f.db,
        &Default::default(),
        &mut Default::default(),
        person::Request {
            owner: f.person.owner.clone(),
            sender: f.person.sender.clone(),
            command,
        },
        20,
    )
    .map(|r| r.0)
}
fn change(f: &mut Fixture, id: Uuid, action: &person::Change) -> Result<Receipt<Value>, Error> {
    command(
        f,
        person::Command::Change {
            id,
            action: action.clone(),
        },
    )
    .map(|v| serde_json::from_value(v).unwrap())
}

#[test]
fn branch_copies_exact_prefix_and_settings_without_copying_work_and_retries_once() {
    let mut f = Fixture::new(2);
    let a = f.bots[0];
    let b = f.bots[1];
    f.send(format!("[[koru://bot/{}#B]]", b.0));
    let d = f.claim(b.0, a.1);
    let response = f.publish(&d).answer.message;
    f.send("after selected response".into());
    let source = f.db.read_messages(&f.person, a.1, 0).unwrap();
    let through = source.iter().find(|m| m.id == response).unwrap().sequence;
    let branch = Uuid::new_v4();
    let command = Uuid::new_v4();
    let action = person::Change::Branch {
        thread: a.1,
        branch,
        through: response,
        title: "Branch".into(),
    };
    let work = f.count("SELECT count(*) FROM obligations");
    assert!(!change(&mut f, command, &action).unwrap().replayed);
    assert!(change(&mut f, command, &action).unwrap().replayed);
    assert_eq!(f.count("SELECT count(*) FROM obligations"), work);
    let copied = f.db.read_messages(&f.person, branch, 0).unwrap();
    assert_eq!(copied.len() as u64, through);
    for (original, copy) in source.iter().zip(&copied) {
        assert_ne!(original.id, copy.id);
        assert_eq!(original.content.body, copy.content.body);
        assert_eq!(original.sender, copy.sender);
    }
    assert!(copied.iter().all(|m| {
        m.content
            .in_reply_to
            .is_none_or(|id| copied.iter().any(|m| m.id == id))
    }));
    let c = &f.db.connection;
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM conversation_settings WHERE thread=?1",
            [branch.to_string()],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM participants WHERE thread=?1 AND read_sequence<>0",
            [branch.to_string()],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM turns WHERE thread=?1",
            [branch.to_string()],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    let send = Envelope {
        id: Uuid::new_v4(),
        to: a.0,
        thread: Some(branch),
        body: "continue here".into(),
        in_reply_to: None,
        attachments: vec![],
        references: vec![],
        deadline_ms: None,
        reply_to: ReplyTo::Thread,
    };
    f.db.accept_message(&f.person, &send, 21).unwrap();
    let d = f.claim(a.0, branch);
    assert_eq!(d.work.len(), 1);
    assert_eq!(d.work[0].content.body, "continue here");
    assert!(
        d.context
            .iter()
            .any(|m| m.content.body == "Synthetic answer")
    );
}

#[test]
fn reading_side_does_not_join_but_join_is_receipted_without_waking_a_bot() {
    let mut f = Fixture::new(2);
    let a = f.bots[0];
    let b = f.bots[1];
    f.send("ask".into());
    let d = f.claim(a.0, a.1);
    let (_, asked) = f.ask(&d, a, b.0);
    let side: Uuid = serde_json::from_value(asked["thread"].clone()).unwrap();
    f.db.read_messages(&f.person, side, 0).unwrap();
    assert_eq!(
        f.count("SELECT count(*) FROM participants WHERE kind='person'"),
        2
    );
    let work = f.count("SELECT count(*) FROM obligations");
    let action = person::Change::Join { thread: side };
    let request = Uuid::new_v4();
    assert!(!change(&mut f, request, &action).unwrap().replayed);
    assert!(change(&mut f, request, &action).unwrap().replayed);
    assert_eq!(
        f.count("SELECT count(*) FROM participants WHERE kind='person'"),
        3
    );
    assert_eq!(f.count("SELECT count(*) FROM obligations"), work);
    f.person = Person {
        owner: "someone else".into(),
        sender: "stranger".into(),
    };
    assert_eq!(
        change(&mut f, Uuid::new_v4(), &action).err(),
        Some(Error::NotAllowed)
    );
}

#[test]
fn branch_refuses_person_or_foreign_response_atomically() {
    let mut f = Fixture::new(1);
    let a = f.bots[0];
    let message = f.send("person".into()).message;
    let before = f.count("SELECT count(*) FROM threads");
    for through in [message, Uuid::new_v4()] {
        let action = person::Change::Branch {
            thread: a.1,
            branch: Uuid::new_v4(),
            through,
            title: "no".into(),
        };
        assert_eq!(
            change(&mut f, Uuid::new_v4(), &action).err(),
            Some(Error::NotAllowed)
        );
        assert_eq!(f.count("SELECT count(*) FROM threads"), before);
    }
}

#[test]
fn branch_attachment_keeps_physical_owner_and_nested_copy_is_flattened() {
    use koru_ai_core::{
        AttachmentKind, AttachmentSource, ChatAttachment, ChatAttachmentId, Timestamp,
    };
    let mut f = Fixture::new(1);
    let a = f.bots[0];
    let attachment = ChatAttachment {
        id: ChatAttachmentId::new(),
        message_id: None,
        ordinal: None,
        kind: AttachmentKind::File,
        source: AttachmentSource::Drop,
        file_name: "fixture.txt".into(),
        media_type: "text/plain".into(),
        byte_count: 5,
        dimensions: None,
        origin: None,
        reference_path: None,
        created_at: Timestamp::from_millis(1),
    };
    f.db.stage_attachment(
        &f.person,
        &crate::attachments::Record {
            thread: a.1,
            attachment: attachment.clone(),
            content_blake3: blake3::hash(b"hello").to_hex().to_string(),
            has_sent_copy: false,
        },
    )
    .unwrap();
    f.db.accept_message(
        &f.person,
        &Envelope {
            id: Uuid::new_v4(),
            to: a.0,
            thread: Some(a.1),
            body: "with file".into(),
            in_reply_to: None,
            attachments: vec![json!(attachment.id.as_uuid())],
            references: vec![],
            deadline_ms: None,
            reply_to: ReplyTo::Thread,
        },
        2,
    )
    .unwrap();
    let d = f.claim(a.0, a.1);
    let response = f.publish(&d).answer.message;
    let mut origin = a.1;
    let mut through = response;
    for _ in 0..2 {
        let branch = Uuid::new_v4();
        change(
            &mut f,
            Uuid::new_v4(),
            &person::Change::Branch {
                thread: origin,
                branch,
                through,
                title: "with file".into(),
            },
        )
        .unwrap();
        let history = f.db.read_messages(&f.person, branch, 0).unwrap();
        let copied = history
            .iter()
            .find(|m| !m.content.attachments.is_empty())
            .unwrap();
        let record =
            f.db.read_attachment(&f.person, branch, attachment.id.as_uuid())
                .unwrap()
                .unwrap();
        assert_eq!(record.thread, a.1);
        assert_eq!(record.attachment.message_id.unwrap().as_uuid(), copied.id);
        assert_eq!(
            serde_json::to_value(&record.attachment).unwrap(),
            copied.content.attachments[0]
        );
        through = history
            .iter()
            .find(|m| m.sender == a.0.to_string())
            .unwrap()
            .id;
        origin = branch;
    }
    assert!(
        f.db.read_attachment(&f.person, f.bots[0].1, Uuid::new_v4())
            .unwrap()
            .is_none()
    );
    assert_eq!(f.count("SELECT count(*) FROM attachments"), 1);
}

#[test]
fn cards_and_activity_project_actual_participants_without_joining() {
    let mut f = Fixture::new(3);
    let a = f.bots[0];
    let b = f.bots[1];
    let c = f.bots[2];
    f.send(format!("[[koru://bot/{}#B]] [[koru://bot/{}#C]]", b.0, c.0));
    let workspace = f.workspace.as_uuid();
    let activity = command(
        &mut f,
        person::Command::Read {
            query: person::Query::Activity { workspace, after: None },
        },
    )
    .unwrap();
    let active = activity["items"].as_array().unwrap();
    assert_eq!(active.len(), 2);
    assert!(active.iter().all(|r| r["bot"] != json!(a.0)));
    let d = f.claim(b.0, a.1);
    let (_, asked) = f.ask(&d, (b.0, a.1), c.0);
    let side: Uuid = serde_json::from_value(asked["thread"].clone()).unwrap();
    let view = command(
        &mut f,
        person::Command::Read {
            query: person::Query::Thread { thread: side },
        },
    )
    .unwrap();
    assert_eq!(view["joined"], false);
    assert_eq!(view["origin"]["bot"], json!(a.0));
    assert_eq!(view["participants"].as_array().unwrap().len(), 2);
    let cards = command(
        &mut f,
        person::Command::Read {
            query: person::Query::Cards {
                thread: a.1,
                after: None,
            },
        },
    )
    .unwrap();
    let card = &cards["items"][0];
    assert_eq!(card["thread"], json!(side));
    assert_eq!(card["bot"], json!(b.0));
    assert_eq!(card["messages"], 1);
    let read_again = command(
        &mut f,
        person::Command::Read {
            query: person::Query::Thread { thread: side },
        },
    )
    .unwrap();
    assert_eq!(read_again["joined"], false);
}

#[test]
fn navigation_pages_are_bounded_and_do_not_repeat_the_cursor() {
    let mut f = Fixture::new(1);
    let a = f.bots[0];
    for i in 0..65 {
        change(
            &mut f,
            Uuid::new_v4(),
            &person::Change::Conversation {
                bot: a.0,
                title: format!("Conversation {i}"),
            },
        )
        .unwrap();
    }
    let page = command(
        &mut f,
        person::Command::Read {
            query: person::Query::Cards {
                thread: a.1,
                after: None,
            },
        },
    )
    .unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 64);
    let after = serde_json::from_value(page["next"].clone()).unwrap();
    let next = command(
        &mut f,
        person::Command::Read {
            query: person::Query::Cards { thread: a.1, after },
        },
    )
    .unwrap();
    assert_eq!(next["items"].as_array().unwrap().len(), 1);
    assert!(next["next"].is_null());
    assert!(
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["id"] != next["items"][0]["id"])
    );
}

#[test]
fn origin_cards_preview_only_the_exact_published_reply_from_the_recipient() {
    let mut f = Fixture::new(2);
    let a = f.bots[0];
    let b = f.bots[1];
    f.send("begin".into());
    let origin = f.claim(a.0, a.1);
    let (_, sent) = f.ask(&origin, a, b.0);
    let side: Uuid = serde_json::from_value(sent["thread"].clone()).unwrap();
    let read = |f: &mut Fixture| {
        command(
            f,
            person::Command::Read {
                query: person::Query::Cards {
                    thread: a.1,
                    after: None,
                },
            },
        )
        .unwrap()
    };
    assert!(read(&mut f)["items"][0]["dispatches"][0]["preview"].is_null());
    let delivery = f.claim(b.0, side);
    let answer = f.publish(&delivery).answer.message;
    let cards = read(&mut f);
    let dispatch = &cards["items"][0]["dispatches"][0];
    let origin_reply: String =
        f.db.connection
            .query_row(
                "SELECT reply_message FROM turns WHERE id=?1",
                [origin.turn.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(dispatch["origin_reply"], json!(origin_reply));
    assert_eq!(dispatch["preview"]["message"], json!(answer));
    assert_eq!(dispatch["preview"]["markdown"], "Synthetic answer");
    assert_eq!(dispatch["preview"]["truncated"], false);
    // A newer, unrelated message in the same thread is not this ask's answer.
    f.db.connection.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) SELECT ?1,thread,sequence+1,sender,kind,CAST(json_set(CAST(payload AS TEXT),'$.body','Unrelated answer') AS BLOB),state,accepted_at_ms+1 FROM messages WHERE id=?2",
        rusqlite::params![Uuid::new_v4().to_string(), answer.to_string()]).unwrap();
    assert_eq!(
        read(&mut f)["items"][0]["dispatches"][0]["preview"]["markdown"],
        "Synthetic answer"
    );
    // Even a mismatched answered_by record must not expose another sender.
    f.db.connection
        .execute(
            "UPDATE messages SET sender=?1 WHERE id=?2",
            rusqlite::params![a.0.to_string(), answer.to_string()],
        )
        .unwrap();
    assert!(read(&mut f)["items"][0]["dispatches"][0]["preview"].is_null());
}
