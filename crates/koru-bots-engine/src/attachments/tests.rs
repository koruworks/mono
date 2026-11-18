// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::{ChatAttachmentId, Timestamp};

pub(crate) fn record(thread: Uuid, path_length: usize) -> Record {
    Record {
        thread,
        attachment: ChatAttachment {
            id: ChatAttachmentId::new(),
            message_id: None,
            ordinal: None,
            kind: AttachmentKind::File,
            source: AttachmentSource::Path,
            file_name: "fixture.txt".into(),
            media_type: "text/plain".into(),
            byte_count: 10,
            dimensions: None,
            origin: None,
            reference_path: Some(format!("C:/fixture/{}.txt", "x".repeat(path_length))),
            created_at: Timestamp::from_millis(1),
        },
        content_blake3: blake3::hash(b"fixture").to_hex().to_string(),
        has_sent_copy: false,
    }
}

#[test]
fn metadata_replay_removal_and_identity_conflicts_are_durable() {
    let mut f = crate::messages::tests::Fixture::new();
    let mut file = record(f.thread, 5);
    let id = file.attachment.id.as_uuid();
    f.db.stage_attachment(&f.person, &file).unwrap();
    file.attachment.created_at = Timestamp::from_millis(9);
    assert_eq!(
        f.db.stage_attachment(&f.person, &file)
            .unwrap()
            .attachment
            .created_at,
        Timestamp::from_millis(1)
    );
    file.attachment.file_name = "changed.txt".into();
    assert_eq!(
        f.db.stage_attachment(&f.person, &file).unwrap_err(),
        Error::Conflict
    );
    f.db.remove_attachment(&f.person, f.thread, id).unwrap();
    f.db.remove_attachment(&f.person, f.thread, id).unwrap();
    assert!(
        f.db.read_attachment(&f.person, f.thread, id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.db.stage_attachment(&f.person, &file).unwrap_err(),
        Error::NotAllowed
    );
    let fresh = record(f.thread, 3);
    f.db.remove_attachment(&f.person, f.thread, fresh.attachment.id.as_uuid())
        .unwrap();
    assert_eq!(
        f.db.stage_attachment(&f.person, &fresh).unwrap_err(),
        Error::NotAllowed
    );
}

#[test]
fn accepting_files_is_atomic_scoped_and_sent_files_cannot_be_removed_or_reused() {
    let mut f = crate::messages::tests::Fixture::new();
    let file = record(f.thread, 5);
    let id = file.attachment.id.as_uuid();
    f.db.stage_attachment(&f.person, &file).unwrap();
    let stranger = Person {
        owner: "other".into(),
        sender: f.person.sender.clone(),
    };
    assert_eq!(
        f.db.read_attachment(&stranger, f.thread, id).unwrap_err(),
        Error::NotAllowed
    );
    let mut envelope = f.envelope("with file");
    envelope.attachments = vec![serde_json::json!(id)];
    f.db.connection.execute_batch("CREATE TEMP TRIGGER fail_inbox BEFORE INSERT ON inbox BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(f.db.accept_message(&f.person, &envelope, 2).is_err());
    assert!(
        f.db.read_attachment(&f.person, f.thread, id)
            .unwrap()
            .unwrap()
            .attachment
            .message_id
            .is_none()
    );
    assert_eq!(f.count("messages"), 0);
    f.db.connection
        .execute_batch("DROP TRIGGER fail_inbox;")
        .unwrap();
    let accepted = f.db.accept_message(&f.person, &envelope, 3).unwrap().answer;
    assert!(
        f.db.accept_message(&f.person, &envelope, 4)
            .unwrap()
            .replayed
    );
    let saved =
        f.db.read_attachment(&f.person, f.thread, id)
            .unwrap()
            .unwrap();
    assert_eq!(
        saved.attachment.message_id,
        Some(MessageId::from_uuid(accepted.message))
    );
    assert_eq!(saved.attachment.ordinal, Some(0));
    assert_eq!(
        f.db.read_messages(&f.person, f.thread, 0).unwrap()[0]
            .content
            .attachments[0],
        serde_json::to_value(saved.attachment).unwrap()
    );
    assert_eq!(
        f.db.remove_attachment(&f.person, f.thread, id),
        Err(Error::NotAllowed)
    );
    envelope.id = Uuid::new_v4();
    assert_eq!(
        f.db.accept_message(&f.person, &envelope, 5).unwrap_err(),
        Error::NotAllowed
    );
    assert_eq!(f.count("messages"), 1);
}

#[test]
fn duplicates_unknown_ids_and_message_byte_limit_refuse_without_partial_binding() {
    let mut f = crate::messages::tests::Fixture::new();
    let mut envelope = f.envelope("files");
    let mut files = Vec::new();
    for _ in 0..3 {
        let mut file = record(f.thread, 5);
        file.attachment.byte_count = MAX_ATTACHMENT_BYTES;
        f.db.stage_attachment(&f.person, &file).unwrap();
        files.push(file.attachment.id.as_uuid());
    }
    for ids in [
        vec![files[0], files[0]],
        vec![files[0], Uuid::new_v4()],
        files.clone(),
    ] {
        envelope.attachments = ids.into_iter().map(|id| serde_json::json!(id)).collect();
        assert!(f.db.accept_message(&f.person, &envelope, 3).is_err());
        assert_eq!(f.count("messages"), 0);
    }
    for id in files {
        assert!(
            f.db.read_attachment(&f.person, f.thread, id)
                .unwrap()
                .unwrap()
                .attachment
                .message_id
                .is_none()
        );
    }
}

#[test]
fn metadata_and_expanded_messages_are_bounded_before_acknowledgement() {
    let mut f = crate::messages::tests::Fixture::new();
    let mut file = record(f.thread, 3000);
    file.attachment.media_type = "x".repeat(17 * 1024);
    assert_eq!(
        f.db.stage_attachment(&f.person, &file).unwrap_err(),
        Error::InvalidRequest
    );
    file.attachment.media_type = "text/plain".into();
    f.db.stage_attachment(&f.person, &file).unwrap();
    let mut envelope = f.envelope(&"x".repeat(crate::person::MAX_COMMAND_BYTES - 1000));
    envelope.attachments = vec![serde_json::json!(file.attachment.id.as_uuid())];
    assert_eq!(
        f.db.accept_message(&f.person, &envelope, 2).unwrap_err(),
        Error::InvalidRequest
    );
    assert_eq!(f.count("messages"), 0);
    assert!(
        f.db.read_attachment(&f.person, f.thread, file.attachment.id.as_uuid())
            .unwrap()
            .unwrap()
            .attachment
            .message_id
            .is_none()
    );
}
