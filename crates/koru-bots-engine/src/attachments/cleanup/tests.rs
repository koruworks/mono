// SPDX-License-Identifier: MPL-2.0
use super::*;
#[test]
fn collection_preserves_bound_and_recent_files_and_tombstones_unknown_uploads() {
    let mut f = crate::messages::tests::Fixture::new();
    let old = crate::attachments::tests::record(f.thread, 3);
    f.db.stage_attachment(&f.person, &old).unwrap();
    let mut envelope = f.envelope("owned");
    envelope
        .attachments
        .push(serde_json::json!(old.attachment.id.as_uuid()));
    f.db.accept_message(&f.person, &envelope, 2).unwrap();
    assert!(
        !f.db
            .collect_attachment(&f.person, f.thread, old.attachment.id.as_uuid(), 10)
            .unwrap()
    );
    let recent = crate::attachments::tests::record(f.thread, 4);
    f.db.stage_attachment(&f.person, &recent).unwrap();
    assert!(
        !f.db
            .collect_attachment(&f.person, f.thread, recent.attachment.id.as_uuid(), 1)
            .unwrap()
    );
    assert!(
        f.db.collect_attachment(&f.person, f.thread, recent.attachment.id.as_uuid(), 2)
            .unwrap()
    );
    assert_eq!(
        f.db.stage_attachment(&f.person, &recent).unwrap_err(),
        Error::NotAllowed
    );
    let missing = crate::attachments::tests::record(f.thread, 5);
    assert!(
        f.db.collect_attachment(&f.person, f.thread, missing.attachment.id.as_uuid(), 2)
            .unwrap()
    );
    assert_eq!(
        f.db.stage_attachment(&f.person, &missing).unwrap_err(),
        Error::NotAllowed
    );
    let foreign = Person {
        owner: "other".into(),
        sender: f.person.sender.clone(),
    };
    assert_eq!(
        f.db.collect_attachment(&foreign, f.thread, old.attachment.id.as_uuid(), 10)
            .unwrap_err(),
        Error::NotAllowed
    );
}
#[test]
fn expiration_pages_use_stable_cursors_and_include_byte_free_references() {
    let mut f = crate::messages::tests::Fixture::new();
    for _ in 0..70 {
        let record = crate::attachments::tests::record(f.thread, 4);
        f.db.stage_attachment(&f.person, &record).unwrap();
    }
    let first = f.db.expire_attachments(&f.person, None, 2).unwrap();
    assert_eq!(first.garbage.len(), 64);
    assert!(first.next.is_some());
    let second = f.db.expire_attachments(&f.person, first.next, 2).unwrap();
    assert_eq!(second.garbage.len(), 6);
    assert!(second.next.is_none());
    assert!(
        f.db.expire_attachments(&f.person, None, 2)
            .unwrap()
            .garbage
            .is_empty()
    );
}
