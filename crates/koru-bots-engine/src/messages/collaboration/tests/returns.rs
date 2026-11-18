// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn tool_arrival_can_precede_acceptance_without_fabricating_delivery_evidence() {
    let mut f = Fixture::new(2);
    f.send("begin".into());
    let a = f.bots[0];
    let b = f.bots[1];
    let d =
        f.db.claim_messages(
            a.0,
            a.1,
            Uuid::new_v4(),
            Uuid::new_v4(),
            Resume::Rebuilt,
            65536,
            3,
        )
        .unwrap()
        .unwrap();
    let (request, accepted) = f.ask(&d, a, b.0);
    assert_eq!(accepted["status"], "accepted");
    assert_eq!(
        f.count("SELECT count(*) FROM deliveries WHERE state='accepted'"),
        0
    );
    f.db.mark_delivery_unknown(d.turn, d.id).unwrap();
    let identity = f.identity(&d, a.0, a.1);
    assert_eq!(
        ask(&mut f.db, &identity, &request, 5).unwrap(),
        accepted,
        "committed effect keeps its receipt"
    );
    let mut fresh = request;
    fresh.id = Uuid::new_v4();
    assert_eq!(
        ask(&mut f.db, &identity, &fresh, 5).err(),
        Some(Error::NotAllowed)
    );
}

#[test]
fn one_expired_ask_in_a_batch_does_not_expire_the_other_return() {
    let mut f = Fixture::new(2);
    f.send("begin".into());
    let a = f.bots[0];
    let b = f.bots[1];
    let first = f.claim(a.0, a.1);
    let identity = f.identity(&first, a.0, a.1);
    for deadline in [4, 10] {
        ask(
            &mut f.db,
            &identity,
            &Ask {
                id: Uuid::new_v4(),
                to: b.0,
                source_message: first.work[0].id,
                body: "work".into(),
                thread: Some(a.1),
                deadline_ms: Some(deadline),
            },
            4,
        )
        .unwrap();
    }
    let second = f.claim(b.0, a.1);
    assert_eq!(second.work.len(), 2);
    f.publish(&first);
    let reply = f.publish(&second).answer.message;
    let resumed = f.claim(a.0, a.1);
    assert_eq!(resumed.work[0].id, reply);
}

#[test]
fn one_answer_can_return_to_same_bot_in_two_origin_conversations() {
    let mut f = Fixture::new(2);
    f.send("begin".into());
    let a = f.bots[0];
    let b = f.bots[1];
    let first = f.claim(a.0, a.1);
    let sent = f.ask(&first, a, b.0).1;
    let side: Uuid = serde_json::from_value(sent["thread"].clone()).unwrap();
    f.db.connection
        .execute(
            "INSERT INTO participants(thread,participant,kind) VALUES (?1,?2,'person')",
            params![side.to_string(), f.person.sender],
        )
        .unwrap();
    f.db.accept_message(
        &f.person,
        &Envelope {
            id: Uuid::new_v4(),
            to: a.0,
            thread: Some(side),
            in_reply_to: None,
            body: "Join this side".into(),
            attachments: vec![],
            references: vec![],
            deadline_ms: None,
            reply_to: ReplyTo::Thread,
        },
        4,
    )
    .unwrap();
    let joined = f.claim(a.0, side);
    let request = Ask {
        id: Uuid::new_v4(),
        to: b.0,
        source_message: joined.work[0].id,
        body: "Another question".into(),
        thread: Some(side),
        deadline_ms: None,
    };
    let identity = f.identity(&joined, a.0, side);
    ask(&mut f.db, &identity, &request, 4).unwrap();
    f.publish(&first);
    f.publish(&joined);
    let replying = f.claim(b.0, side);
    assert_eq!(replying.work.len(), 2);
    let answer = f.publish(&replying).answer.message;
    let main = f.claim(a.0, a.1);
    let side_delivery = f.claim(a.0, side);
    assert_eq!(main.work[0].id, answer);
    assert_eq!(side_delivery.work[0].id, answer);
    assert_eq!(
        f.db.connection
            .query_row(
                "SELECT count(*) FROM messages WHERE id=?1",
                [answer.to_string()],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        1,
        "answer content is stored once"
    );
}

#[test]
fn publish_failure_rolls_back_answer_and_all_continuation_wakes() {
    let mut f = Fixture::new(2);
    f.send("begin".into());
    let a = f.bots[0];
    let b = f.bots[1];
    let first = f.claim(a.0, a.1);
    let sent = f.ask(&first, a, b.0).1;
    let side = serde_json::from_value(sent["thread"].clone()).unwrap();
    let second = f.claim(b.0, side);
    f.db.connection.execute_batch("CREATE TEMP TRIGGER refuse_wake BEFORE INSERT ON inbox BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let request = Publish {
        command: Uuid::new_v4(),
        turn: second.turn,
        body: "answer".into(),
        outputs: vec![],
    };
    assert!(f.db.publish_reply(&request, 5).is_err());
    assert_eq!(
        f.count("SELECT count(*) FROM obligations WHERE state='answered'"),
        0
    );
    assert_eq!(
        f.count("SELECT count(*) FROM turns WHERE state='running'"),
        2
    );
    f.db.connection
        .execute_batch("DROP TRIGGER refuse_wake;")
        .unwrap();
    let first = f.db.publish_reply(&request, 6).unwrap();
    let replay = f.db.publish_reply(&request, 7).unwrap();
    assert_eq!(first.answer, replay.answer);
    assert!(replay.replayed);
    assert_eq!(
        f.count("SELECT count(*) FROM obligations WHERE state='owed'"),
        1
    );
}
