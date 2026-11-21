// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::routines::{Change, tests::Fixture};

fn result(body: &str) -> (Fixture, review::Item) {
    let mut f = Fixture::new();
    let routine = f.create();
    let key = f
        .change(
            Change::RunNow {
                bot: f.bot,
                routine: routine.id,
                revision: routine.revision,
            },
            10,
        )
        .occurrence
        .unwrap()
        .id;
    let delivery = f.claim(&routine, 11);
    f.db.publish_reply(
        &Publish {
            command: Uuid::new_v4(),
            turn: delivery.turn,
            body: body.into(),
            outputs: vec![],
        },
        12,
    )
    .unwrap();
    let item = f.db.routine_review(&f.who, f.bot, key, 0).unwrap().item;
    (f, item)
}
fn envelope(f: &Fixture, item: &review::Item) -> Envelope {
    Envelope {
        id: Uuid::new_v4(),
        to: f.bot,
        thread: Some(f.main),
        in_reply_to: Some(item.card),
        body: "Use the original result to draft a reply".into(),
        attachments: vec![],
        references: vec![],
        deadline_ms: None,
        reply_to: ReplyTo::Thread,
    }
}

#[test]
fn accepted_replies_are_atomic_repeatable_and_win_both_skip_orders() {
    for skip_first in [false, true] {
        let (mut f, item) = result("Exact result");
        let send = envelope(&f, &item);
        assert!(read(&f.db.connection, f.bot, f.main, item.occurrence, 0).is_err());
        if skip_first {
            f.db.skip_routine_review(&f.who, Uuid::new_v4(), f.bot, item.occurrence, 15)
                .unwrap();
        }
        f.db.connection.execute_batch("CREATE TEMP TRIGGER refuse_context BEFORE INSERT ON inbox BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        assert!(f.db.accept_message(&f.who, &send, 20).is_err());
        assert_eq!(
            f.db.routine_review(&f.who, f.bot, item.occurrence, 0)
                .unwrap()
                .item
                .review
                .as_deref(),
            Some(if skip_first { "skipped" } else { "pending" })
        );
        f.db.connection
            .execute_batch("DROP TRIGGER refuse_context;")
            .unwrap();
        let first = f.db.accept_message(&f.who, &send, 21).unwrap().answer;
        let retry = f.db.accept_message(&f.who, &send, 22).unwrap();
        assert!(retry.replayed);
        assert_eq!(retry.answer.message, first.message);
        let mut changed = send.clone();
        changed.body.push('!');
        assert!(f.db.accept_message(&f.who, &changed, 23).is_err());
        if !skip_first {
            f.db.skip_routine_review(&f.who, Uuid::new_v4(), f.bot, item.occurrence, 24)
                .unwrap();
        }
        let mut again = send.clone();
        again.id = Uuid::new_v4();
        let second = f.db.accept_message(&f.who, &again, 25).unwrap().answer;
        assert_ne!(first.message, second.message);
        let handled: String =
            f.db.connection
                .query_row(
                    "SELECT handled_by_message FROM routine_occurrences WHERE id=?1",
                    [item.occurrence.to_string()],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(handled, first.message.to_string());
        assert_eq!(f.db.routine_reviews(&f.who, f.bot, None).unwrap().count, 0);
        let reopened =
            f.db.routine_review(&f.who, f.bot, item.occurrence, 0)
                .unwrap();
        assert_eq!(reopened.item.review.as_deref(), Some("handled"));
        assert_eq!(reopened.text, "Exact result");
        assert_eq!(
            read(&f.db.connection, f.bot, f.main, item.occurrence, 0)
                .unwrap()
                .text,
            "Exact result"
        );
        let messages = f.db.read_messages(&f.who, f.main, 0).unwrap();
        assert_eq!(
            messages
                .iter()
                .find(|m| m.id == first.message)
                .unwrap()
                .content
                .in_reply_to,
            Some(item.card)
        );
        assert_eq!(
            messages
                .iter()
                .find(|m| m.id == second.message)
                .unwrap()
                .content
                .in_reply_to,
            Some(item.card)
        );
    }
}

#[test]
fn forged_missing_foreign_and_redirected_result_references_are_refused() {
    let (mut f, item) = result("Private result");
    let mut send = envelope(&f, &item);
    let other = Person {
        owner: "other".into(),
        sender: "person".into(),
    };
    assert!(f.db.accept_message(&other, &send, 20).is_err());
    assert!(read(&f.db.connection, Uuid::new_v4(), f.main, item.occurrence, 0).is_err());
    assert!(read(&f.db.connection, f.bot, Uuid::new_v4(), item.occurrence, 0).is_err());
    send.in_reply_to = Some(item.reply);
    assert!(f.db.accept_message(&f.who, &send, 20).is_err());
    send.in_reply_to = Some(Uuid::new_v4());
    assert!(f.db.accept_message(&f.who, &send, 20).is_err());
    send.in_reply_to = Some(item.card);
    let guest = Uuid::new_v4();
    send.body = format!("[[koru://bot/{guest}]] act on this");
    assert!(f.db.accept_message(&f.who, &send, 20).is_err());
    send.body = "act on this".into();
    f.db.connection
        .execute(
            "UPDATE routine_occurrences SET reply=NULL WHERE id=?1",
            [item.occurrence.to_string()],
        )
        .unwrap();
    assert!(f.db.accept_message(&f.who, &send, 20).is_err());
    assert!(
        f.db.routine_review(&f.who, f.bot, item.occurrence, 0)
            .is_err()
    );
    assert!(
        !f.db
            .read_messages(&f.who, f.main, 0)
            .unwrap()
            .iter()
            .any(|m| m.kind == "words")
    );
}

#[test]
fn long_escaped_result_fits_input_and_bound_tool_reconstructs_it_exactly() {
    use crate::authority::{Call, Mint, Worker};
    let body = "é\n\"".repeat(24_576); // exactly 98,304 UTF-8 bytes, with JSON escaping
    assert_eq!(body.len(), 98_304);
    let (mut f, item) = result(&body);
    let send = envelope(&f, &item);
    let accepted = f.db.accept_message(&f.who, &send, 20).unwrap().answer;
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
            21,
        )
        .unwrap();
    let secret = issued.secret.unwrap();
    let delivery =
        f.db.claim_session_messages(worker, issued.session, Resume::Rebuilt, 32_768, 22)
            .unwrap()
            .unwrap();
    assert!(encode(&delivery).unwrap().len() <= 32_768);
    let input = delivery
        .work
        .iter()
        .find(|m| m.id == accepted.message)
        .unwrap();
    let context = input.routine_context.as_ref().unwrap();
    assert_eq!(context.occurrence, item.occurrence);
    assert_eq!(context.card, item.card);
    assert_eq!(context.reply, item.reply);
    assert!(context.partial && context.excerpt.len() <= crate::limits::MAX_ROUTINE_EXCERPT_BYTES);
    assert!(body.starts_with(&context.excerpt));
    assert_eq!(input.content.body, send.body);
    let mut tight = delivery.clone();
    crate::messages::history::bound(&mut tight, 4096).unwrap();
    assert!(encode(&tight).unwrap().len() <= 4096);
    let narrowed = tight
        .work
        .iter()
        .find(|m| m.id == accepted.message)
        .unwrap()
        .routine_context
        .as_ref()
        .unwrap();
    assert_eq!(narrowed.reply, item.reply);
    assert!(narrowed.partial && narrowed.excerpt.len() < context.excerpt.len());
    assert!(body.starts_with(&narrowed.excerpt));
    f.db.delivery_accepted(delivery.turn, delivery.id).unwrap();
    let mut full = String::new();
    let mut offset = 0;
    loop {
        let args = serde_json::json!({"occurrence":item.occurrence,"offset":offset});
        let call = Call::parse(
            Uuid::new_v4(),
            "read_routine_result".into(),
            &serde_json::to_vec(&args).unwrap(),
        )
        .unwrap();
        let binding =
            f.db.bind_call(worker, &secret, &call, Uuid::new_v4(), 23)
                .unwrap();
        let value = crate::own_tools::execute(&mut f.db, &secret, binding.id, &call, 23).unwrap();
        let text = value["text"].as_str().unwrap();
        assert!(text.len() <= crate::limits::MAX_ROUTINE_RESULT_CHUNK_BYTES);
        assert!(encode(&value).unwrap().len() <= crate::limits::MAX_PAYLOAD_BYTES);
        full.push_str(text);
        let Some(next) = value["next"].as_u64() else {
            break;
        };
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(full, body);
    let mut second = send.clone();
    second.id = Uuid::new_v4();
    f.db.accept_message(&f.who, &second, 24).unwrap();
    let steer =
        f.db.offer_steer(delivery.turn, 32_768, 25)
            .unwrap()
            .unwrap();
    assert!(encode(&steer).unwrap().len() <= 32_768);
    assert_eq!(
        steer.work[0].routine_context.as_ref().unwrap().occurrence,
        item.occurrence
    );
    assert!(steer.work[0].routine_context.as_ref().unwrap().partial);
}

#[test]
fn informational_and_legacy_results_can_be_handled_without_requeueing() {
    for state in [None, Some("not_required")] {
        let (mut f, item) = result("Retained result");
        f.db.connection
            .execute(
                "UPDATE routine_occurrences SET review=?2 WHERE id=?1",
                params![item.occurrence.to_string(), state],
            )
            .unwrap();
        assert_eq!(f.db.routine_reviews(&f.who, f.bot, None).unwrap().count, 0);
        f.db.accept_message(&f.who, &envelope(&f, &item), 20)
            .unwrap();
        assert_eq!(
            f.db.routine_review(&f.who, f.bot, item.occurrence, 0)
                .unwrap()
                .item
                .review
                .as_deref(),
            Some("handled")
        );
        assert_eq!(f.db.routine_reviews(&f.who, f.bot, None).unwrap().count, 0);
    }
}
