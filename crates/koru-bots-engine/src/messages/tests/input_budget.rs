// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn a_small_provider_budget_makes_smaller_batches_without_dropping_attachments() {
    let mut f = Fixture::new();
    let attachment = serde_json::json!({"file":"x".repeat(200)});
    for _ in 0..32 {
        let mut envelope = f.envelope(&"body".repeat(100));
        envelope.attachments.push(attachment.clone());
        f.db.accept_message(&f.person, &envelope, 1).unwrap();
    }
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
        assert_eq!(message.content.attachments, vec![attachment.clone()]);
    }
    assert_eq!(f.count("inbox"), 32);
    assert_eq!(f.count("turns"), 1, "failed sizing attempts roll back");
}

#[test]
fn one_attachment_that_cannot_fit_refuses_without_accepting_or_losing_work() {
    let mut f = Fixture::new();
    let mut envelope = f.envelope("work");
    envelope
        .attachments
        .push(serde_json::json!({"file":"x".repeat(5000)}));
    f.db.accept_message(&f.person, &envelope, 1).unwrap();
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
    assert_eq!(f.count("inbox"), 1);
    assert_eq!(f.count("deliveries"), 0);
}
