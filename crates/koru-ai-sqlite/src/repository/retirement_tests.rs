// SPDX-License-Identifier: MPL-2.0

use super::tests::{chat, staged, turn};
use super::*;

fn retained_rows(repository: &Repository, chat: ChatId) -> Vec<Vec<rusqlite::types::Value>> {
    let mut rows = Vec::new();
    for (table, key) in [
        ("bots", "bot_id"),
        ("bot_chats", "chat_id"),
        ("chats", "id"),
        ("messages", "chat_id"),
        ("inference_attempts", "chat_id"),
        ("message_attachments", "chat_id"),
    ] {
        let mut statement = repository
            .connection
            .prepare(&format!(
                "SELECT * FROM {table} WHERE {key} = ?1 ORDER BY rowid"
            ))
            .unwrap();
        let count = statement.column_count();
        rows.extend(
            statement
                .query_map([chat.to_storage()], |row| {
                    (0..count)
                        .map(|index| row.get(index))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .map(Result::unwrap),
        );
    }
    rows
}

#[test]
fn ordinary_recovery_and_cleanup_leave_retired_storage_unchanged() {
    let mut repository = Repository::in_memory().unwrap();
    let ordinary = ChatId::new();
    let retired = ChatId::new();
    for id in [ordinary, retired] {
        repository.create_chat(&chat(id, 0)).unwrap();
    }
    let ordinary_turn = turn(ordinary, Uuid::new_v4(), 1);
    let retired_turn = turn(retired, Uuid::new_v4(), 1);
    repository.begin_turn(&ordinary_turn).unwrap();
    repository.begin_turn(&retired_turn).unwrap();
    let ordinary_attachment = staged(ordinary, "ordinary.png", 2);
    let retained_attachment = staged(retired, "retired.png", 2);
    repository.stage_attachment(&ordinary_attachment).unwrap();
    repository.stage_attachment(&retained_attachment).unwrap();
    repository.retire_chat_for_fixture(retired).unwrap();
    let before = retained_rows(&repository, retired);
    let now = Timestamp::from_millis(i64::MAX / 2);

    let recovered = repository.recover_interrupted(now).unwrap();
    assert_eq!((recovered.attempts, recovered.messages), (1, 1));
    assert_eq!(
        repository
            .attempt(ordinary_turn.attempt.id)
            .unwrap()
            .unwrap()
            .status,
        AttemptStatus::Interrupted
    );
    assert_eq!(
        repository
            .attempt(retired_turn.attempt.id)
            .unwrap()
            .unwrap()
            .status,
        AttemptStatus::Pending
    );
    assert_eq!(repository.remove_staged_attachments_before(now).unwrap(), 1);
    assert_eq!(repository.clear_usage().unwrap().attempts, 1);
    assert_eq!(repository.clear_history().unwrap(), 1);
    assert_eq!(retained_rows(&repository, retired), before);
    assert!(
        repository
            .attachment(retained_attachment.attachment.id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn retired_ownership_blocks_ordinary_reads_and_mutations_without_decoding_bot_data() {
    let mut repository = Repository::in_memory().unwrap();
    let retired = ChatId::new();
    repository.create_chat(&chat(retired, 0)).unwrap();
    let pending = turn(retired, Uuid::new_v4(), 1);
    repository.begin_turn(&pending).unwrap();
    let attachment = staged(retired, "retired.png", 2);
    repository.stage_attachment(&attachment).unwrap();
    repository.retire_chat_for_fixture(retired).unwrap();
    let before = retained_rows(&repository, retired);

    assert!(repository.is_retired_bot_chat(retired).unwrap());
    assert!(repository.chat(retired).unwrap().is_none());
    assert!(repository.chats().unwrap().is_empty());
    assert!(repository.chat_overviews().unwrap().is_empty());
    assert!(
        repository
            .search(&AiSearchQuery {
                text: "Question".into(),
                limit: 10,
                cursor: None,
                visibility: ChatAccessLens::default(),
                profile: None,
                removed_profiles: vec![],
            })
            .unwrap()
            .hits
            .is_empty()
    );
    assert!(!repository.rename_chat(retired, "Changed").unwrap());
    assert!(
        repository
            .replace_chat_tool_policy(retired, ToolMode::Manual, &[], Timestamp::now())
            .unwrap()
            .is_none()
    );
    assert!(!repository.delete_chat(retired).unwrap());
    assert_eq!(repository.delete_chats(&[retired]).unwrap(), 0);
    assert!(
        repository
            .begin_turn(&turn(retired, Uuid::new_v4(), 3))
            .is_err()
    );
    assert!(
        repository
            .apply_event(
                &AiEvent::TextDelta {
                    attempt_id: pending.attempt.id,
                    text: "Must stay unread".into(),
                    at: Timestamp::now(),
                },
                &[]
            )
            .is_err()
    );
    assert!(
        repository
            .append_user_message(&pending.user_message)
            .is_err()
    );
    assert!(
        repository
            .stage_attachment(&staged(retired, "new.png", 3))
            .is_err()
    );
    assert_eq!(
        repository
            .remove_staged_attachment(retired, attachment.attachment.id)
            .unwrap(),
        RemovedAttachment::NotFound
    );
    assert_eq!(retained_rows(&repository, retired), before);
}
