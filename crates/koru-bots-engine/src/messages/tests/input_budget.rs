// SPDX-License-Identifier: MPL-2.0
use super::*;

fn staged(f: &mut Fixture, length: usize) -> serde_json::Value {
    let record = crate::attachments::tests::record(f.thread, length);
    f.db.stage_attachment(&f.person, &record).unwrap();
    serde_json::json!(record.attachment.id.as_uuid())
}

#[test]
fn provider_file_limit_splits_work_and_omits_only_old_context() {
    let mut f = Fixture::new();
    for _ in 0..2 {
        let mut envelope = f.envelope("six files");
        envelope.attachments = (0..6).map(|_| staged(&mut f, 3)).collect();
        f.db.accept_message(&f.person, &envelope, 1).unwrap();
    }
    let first = f.claim(Resume::Rebuilt);
    assert_eq!(first.work.len(), 1);
    assert_eq!(first.work[0].content.attachments.len(), 6);
    f.db.delivery_accepted(first.turn, first.id).unwrap();
    f.db.publish_reply(&reply(&first), 3).unwrap();
    let second = f.claim(Resume::Rebuilt);
    assert_eq!(second.work.len(), 1);
    assert_eq!(second.work[0].content.attachments.len(), 6);
    assert!(second.older_history_available);
    assert!(
        second
            .context
            .iter()
            .all(|m| m.content.attachments.is_empty())
    );
    assert_eq!(
        f.db.read_messages(&f.person, f.thread, 0)
            .unwrap()
            .iter()
            .map(|m| m.content.attachments.len())
            .sum::<usize>(),
        12
    );
}

#[test]
fn a_small_provider_budget_makes_smaller_batches_without_dropping_attachments() {
    let mut f = Fixture::new();
    for _ in 0..32 {
        let mut envelope = f.envelope(&"body".repeat(100));
        envelope.attachments.push(staged(&mut f, 200));
        f.db.accept_message(&f.person, &envelope, 1).unwrap();
    }
    let saved = f.db.read_messages(&f.person, f.thread, 0).unwrap();
    let delivery =
        f.db.claim_messages(
            f.bot,
            f.thread,
            Uuid::new_v4(),
            Uuid::new_v4(),
            Resume::Rebuilt,
            2500,
            2,
        )
        .unwrap()
        .unwrap();
    assert!(!delivery.work.is_empty() && delivery.work.len() < 32);
    assert!(encode(&delivery).unwrap().len() <= 2500);
    for message in &delivery.work {
        assert_eq!(
            message.content.attachments,
            saved
                .iter()
                .find(|original| original.id == message.id)
                .unwrap()
                .content
                .attachments
        );
    }
    assert_eq!(f.count("inbox"), 32);
    assert_eq!(f.count("turns"), 1, "failed sizing attempts roll back");
}

#[test]
fn one_attachment_that_cannot_fit_is_retained_and_refused_without_wedging_later_work() {
    let mut f = Fixture::new();
    let mut envelope = f.envelope("work");
    envelope.attachments.push(staged(&mut f, 3000));
    let accepted = f.db.accept_message(&f.person, &envelope, 1).unwrap().answer;
    let saved = f.db.read_messages(&f.person, f.thread, 0).unwrap()[0]
        .content
        .attachments
        .clone();
    assert_eq!(
        f.db.claim_messages(
            f.bot,
            f.thread,
            Uuid::new_v4(),
            Uuid::new_v4(),
            Resume::Rebuilt,
            1024,
            2
        )
        .err(),
        Some(Error::InputTooSmall)
    );
    assert_eq!(f.count("turns"), 0);
    assert_eq!(f.count("inbox"), 0);
    assert_eq!(f.count("deliveries"), 0);
    let state: String =
        f.db.connection
            .query_row(
                "SELECT state FROM obligations WHERE message=?1",
                [accepted.message.to_string()],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(state, "refused");
    let messages = f.db.read_messages(&f.person, f.thread, 0).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].content.attachments, saved);
    assert_eq!(messages[1].kind, "setup_note");
    assert_eq!(messages[1].content.in_reply_to, Some(accepted.message));
    assert_eq!(
        messages[1].content.references[0]["notice"],
        "input_too_large"
    );
    let later = f.send("small later request");
    let delivery =
        f.db.claim_messages(
            f.bot,
            f.thread,
            Uuid::new_v4(),
            Uuid::new_v4(),
            Resume::Rebuilt,
            2048,
            3,
        )
        .unwrap()
        .unwrap();
    assert_eq!(delivery.work.len(), 1);
    assert_eq!(delivery.work[0].id, later.message);
}

#[test]
fn failed_refusal_notice_keeps_the_original_obligation_owed() {
    let mut f = Fixture::new();
    let mut envelope = f.envelope("work");
    envelope.attachments.push(staged(&mut f, 3000));
    let accepted = f.db.accept_message(&f.person, &envelope, 1).unwrap().answer;
    f.db.connection.execute_batch("CREATE TEMP TRIGGER fail_notice BEFORE INSERT ON messages WHEN NEW.kind='setup_note' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert_eq!(
        f.db.claim_messages(
            f.bot,
            f.thread,
            Uuid::new_v4(),
            Uuid::new_v4(),
            Resume::Rebuilt,
            1024,
            2
        )
        .err(),
        Some(Error::StorageUnavailable)
    );
    assert_eq!(f.count("inbox"), 1);
    assert_eq!(f.count("turns"), 0);
    assert_eq!(f.count("messages"), 1);
    let state: String =
        f.db.connection
            .query_row(
                "SELECT state FROM obligations WHERE message=?1",
                [accepted.message.to_string()],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(state, "owed");
}
