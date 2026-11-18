// SPDX-License-Identifier: MPL-2.0
use super::*;

fn running() -> (Fixture, Delivery) {
    let mut f = Fixture::new();
    f.send("first");
    let first = f.claim(Resume::Rebuilt);
    f.db.delivery_accepted(first.turn, first.id).unwrap();
    (f, first)
}
#[test]
fn a_steer_that_never_reaches_the_agent_is_not_consumed_by_publish() {
    let (mut f, first) = running();
    let message = f.send("later");
    let steer = f.db.offer_steer(first.turn, 64 * 1024, 3).unwrap().unwrap();
    assert_eq!(steer.turn, first.turn);
    assert_eq!(steer.work.len(), 1);
    assert_eq!(steer.work[0].id, message.message);
    assert!(
        f.db.offer_steer(first.turn, 64 * 1024, 3)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.db.publish_reply(&reply(&first), 4)
            .unwrap()
            .answer
            .answered,
        1
    );
    assert_eq!(f.state(message.message), "delivered");
    let next = f.claim(Resume::Rebuilt);
    assert_eq!(next.work[0].id, message.message);
    assert!(next.uncertain);
}
#[test]
fn an_explicit_steer_acknowledgement_is_required_even_after_reply_text() {
    let (mut f, first) = running();
    let message = f.send("later");
    let steer = f.db.offer_steer(first.turn, 64 * 1024, 3).unwrap().unwrap();
    f.db.mark_delivery_unknown(first.turn, steer.id).unwrap();
    assert_eq!(
        f.db.publish_reply(&reply(&first), 4)
            .unwrap()
            .answer
            .answered,
        1
    );
    f.db.delivery_accepted(first.turn, steer.id).unwrap();
    assert_eq!(f.state(message.message), "delivered");
    let next = f.claim(Resume::Resumed {
        found_deliveries: vec![steer.id],
    });
    assert!(next.work.is_empty());
    assert_eq!(next.resume_accepted, vec![steer.id]);
    assert_eq!(
        f.db.publish_reply(&reply(&next), 5)
            .unwrap()
            .answer
            .answered,
        1
    );
}
#[test]
fn accepted_steer_and_initial_input_publish_together() {
    let (mut f, first) = running();
    let message = f.send("later");
    let steer = f.db.offer_steer(first.turn, 64 * 1024, 3).unwrap().unwrap();
    f.db.delivery_accepted(first.turn, steer.id).unwrap();
    assert_eq!(
        f.db.publish_reply(&reply(&first), 4)
            .unwrap()
            .answer
            .answered,
        2
    );
    assert_eq!(f.state(message.message), "answered");
}
#[test]
fn rejected_steer_stays_owed_and_is_not_reoffered_to_the_same_turn() {
    let (mut f, first) = running();
    let mut envelope = f.envelope("attachment waits");
    let file = crate::attachments::tests::record(f.thread, 5);
    f.db.stage_attachment(&f.person, &file).unwrap();
    envelope.attachments = vec![serde_json::json!(file.attachment.id.as_uuid())];
    let message = f.db.accept_message(&f.person, &envelope, 3).unwrap().answer;
    let steer = f.db.offer_steer(first.turn, 64 * 1024, 3).unwrap().unwrap();
    f.db.reject_steer(first.turn, steer.id).unwrap();
    f.db.reject_steer(first.turn, steer.id).unwrap();
    assert_eq!(f.state(message.message), "owed");
    assert!(
        f.db.offer_steer(first.turn, 64 * 1024, 3)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.db.delivery_accepted(first.turn, steer.id).err(),
        Some(Error::StaleTurn)
    );
    assert_eq!(
        f.db.publish_reply(&reply(&first), 4)
            .unwrap()
            .answer
            .answered,
        1
    );
    let next = f.claim(Resume::Rebuilt);
    let saved =
        f.db.read_attachment(&f.person, f.thread, file.attachment.id.as_uuid())
            .unwrap()
            .unwrap();
    assert_eq!(
        next.work[0].content.attachments,
        vec![serde_json::to_value(saved.attachment).unwrap()]
    );
}
#[test]
fn stop_and_replacement_fence_steering_and_offer_rolls_back_as_one_unit() {
    let (mut f, first) = running();
    f.send("later");
    f.db.connection.execute_batch("CREATE TEMP TRIGGER reject_offer BEFORE INSERT ON delivery_obligations BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(f.db.offer_steer(first.turn, 64 * 1024, 3).is_err());
    assert_eq!(f.count("deliveries"), 1);
    f.db.connection
        .execute_batch("DROP TRIGGER reject_offer;")
        .unwrap();
    let steer = f.db.offer_steer(first.turn, 64 * 1024, 3).unwrap().unwrap();
    let mut stale = first.turn;
    stale.worker = Uuid::new_v4();
    assert_eq!(
        f.db.reject_steer(stale, steer.id).err(),
        Some(Error::StaleTurn)
    );
    f.db.stop_turn(&f.person, Uuid::new_v4(), first.turn, 4)
        .unwrap();
    assert_eq!(
        f.db.offer_steer(first.turn, 64 * 1024, 4).err(),
        Some(Error::StaleTurn)
    );
    assert_eq!(
        f.db.reject_steer(first.turn, steer.id).err(),
        Some(Error::StaleTurn)
    );
}
