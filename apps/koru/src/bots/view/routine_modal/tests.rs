// SPDX-License-Identifier: MPL-2.0
use super::*;

fn command(id: Uuid, name: &str) -> wire::Command {
    wire::Command::Create {
        id,
        bot: BotId::from_uuid(Uuid::new_v4()),
        expected_bot_revision: 1,
        name: name.into(),
        objective: "Summarize changes".into(),
        recurrence: koru_recurrence::RecurrenceRule::floating(
            koru_recurrence::CronExpression::parse("0 9 * * *").unwrap(),
        ),
    }
}
#[test]
fn uncertainty_retries_the_exact_body_and_rejects_late_answers() {
    let mut creation = Creation::default();
    let id = Uuid::new_v4();
    let first = creation.begin(command(id, "Original")).unwrap();
    assert!(creation.begin(command(Uuid::new_v4(), "Other")).is_none());
    assert!(!creation.received(Uuid::new_v4(), Err(Refusal::unanswered())));
    assert!(creation.busy);
    assert!(creation.received(id, Err(Refusal::unanswered())));
    let retry = creation
        .begin(command(Uuid::new_v4(), "Changed draft"))
        .unwrap();
    assert_eq!(retry.0, id);
    assert_eq!(
        serde_json::to_value(first.1).unwrap(),
        serde_json::to_value(retry.1).unwrap()
    );
}
#[test]
fn definite_refusal_allows_a_corrected_new_request() {
    let mut creation = Creation::default();
    let id = Uuid::new_v4();
    creation.begin(command(id, "Original")).unwrap();
    creation.received(id, Err(Refusal::code("revision_conflict")));
    assert!(creation.draft().is_none());
    assert!(matches!(creation.answer, Some(Err(_))));
    let next = Uuid::new_v4();
    assert_eq!(creation.begin(command(next, "Corrected")).unwrap().0, next);
}

#[test]
fn expired_receipt_cannot_silently_mint_a_duplicate_creation() {
    let mut creation = Creation::default();
    let id = Uuid::new_v4();
    creation.begin(command(id, "Original")).unwrap();
    creation.received(id, Err(Refusal::code("expired")));
    assert_eq!(
        creation.begin(command(Uuid::new_v4(), "Other")).unwrap().0,
        id
    );
}
