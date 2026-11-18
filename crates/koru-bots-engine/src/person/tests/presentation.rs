// SPDX-License-Identifier: MPL-2.0
use super::*;

fn view(f: &Fixture) -> ThreadView {
    serde_json::from_value(
        read::query(
            &f.db,
            &f.who,
            Query::Thread {
                thread: f.bot.main_thread,
            },
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn the_thread_view_distinguishes_queued_messages_from_a_started_reply() {
    let mut f = Fixture::new();
    let main = f.bot.main_thread;
    let accepted = f.send(main);
    let queued = view(&f);
    assert_eq!(queued.waiting_messages, 1);
    assert!(queued.turns.is_empty());
    let delivery =
        f.db.claim_messages(
            f.bot.id,
            main,
            Uuid::new_v4(),
            Uuid::new_v4(),
            messages::Resume::Rebuilt,
            65536,
            10,
        )
        .unwrap()
        .unwrap();
    let started = view(&f);
    assert_eq!(started.waiting_messages, 0);
    assert_eq!(started.turns.len(), 1);
    let turn = &started.turns[0];
    assert_eq!(turn.id, delivery.turn.id);
    assert_eq!(turn.created_at_ms, 10);
    assert_ne!(turn.reply_message, Some(accepted.message));
    assert!(turn.reply_message.is_some());
    f.send(main);
    assert_eq!(view(&f).waiting_messages, 1);
    f.db.delivery_accepted(delivery.turn, delivery.id).unwrap();
    let answer =
        f.db.publish_reply(
            &messages::Publish {
                command: Uuid::new_v4(),
                turn: delivery.turn,
                body: "done".into(),
                outputs: vec![],
            },
            12,
        )
        .unwrap();
    assert_eq!(Some(answer.answer.message), turn.reply_message);
    assert!(view(&f).turns.is_empty());
    assert_eq!(view(&f).waiting_messages, 1);
}
