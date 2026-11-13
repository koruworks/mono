// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::Timestamp;

fn row(chat: ChatId, ordinal: u32, role: ChatRole) -> Message {
    Message {
        id: MessageId::new(),
        chat_id: chat,
        ordinal,
        role,
        state: MessageState::Complete,
        markdown: "task".into(),
        origin_message_id: None,
        provider: None,
        requested_model: None,
        resolved_model: None,
        error_class: None,
        created_at: Timestamp::from_millis(1),
        completed_at: Some(Timestamp::from_millis(2)),
    }
}
fn resolve(selected: &Message, task: &str, rows: &[Message]) -> Result<Option<Message>, AiError> {
    original_bot_task_user(
        selected,
        task,
        |id| Ok(rows.iter().find(|row| row.id == id).cloned()),
        |chat, ordinal| {
            Ok(rows
                .iter()
                .find(|row| row.chat_id == chat && row.ordinal == ordinal)
                .cloned())
        },
    )
}

#[test]
fn task_anchor_preserves_user_and_assistant_only_retry_identity() {
    let chat = ChatId::new();
    let user = row(chat, 0, ChatRole::User);
    let failed = row(chat, 1, ChatRole::Assistant);
    let mut retry = row(chat, 4, ChatRole::Assistant);
    retry.origin_message_id = Some(failed.id);
    let later = row(chat, 2, ChatRole::User);
    let rows = vec![user.clone(), failed.clone(), later, retry.clone()];
    assert_eq!(
        resolve(&failed, "task", &rows).unwrap().unwrap().id,
        user.id
    );
    assert_eq!(resolve(&retry, "task", &rows).unwrap().unwrap().id, user.id);
    assert_eq!(
        resolve(&retry, "answer-only", &rows)
            .unwrap()
            .map_or(retry.id, |m| m.id),
        retry.id
    );
    let zero = row(chat, 0, ChatRole::Assistant);
    assert!(
        resolve(&zero, "answer-only", &[zero.clone()])
            .unwrap()
            .is_none()
    );
}

#[test]
fn task_anchor_refuses_missing_foreign_and_non_decreasing_ancestry() {
    let chat = ChatId::new();
    let prior = row(chat, 1, ChatRole::Assistant);
    let mut selected = row(chat, 2, ChatRole::Assistant);
    selected.origin_message_id = Some(prior.id);
    assert!(resolve(&selected, "task", &[]).is_err());
    for bad in [
        Message {
            chat_id: ChatId::new(),
            ..prior.clone()
        },
        Message {
            role: ChatRole::User,
            ..prior.clone()
        },
        Message {
            ordinal: 2,
            ..prior.clone()
        },
        Message {
            origin_message_id: Some(selected.id),
            ..prior.clone()
        },
    ] {
        assert!(resolve(&selected, "task", &[bad, selected.clone()]).is_err());
    }
    assert!(resolve(&row(chat, 1, ChatRole::User), "task", &[]).is_err());
    assert!(
        original_bot_task_user(
            &prior,
            "task",
            |_| Ok(None),
            |_, _| Ok(Some(row(ChatId::new(), 0, ChatRole::User)))
        )
        .is_err()
    );
}
