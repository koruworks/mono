// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn snapshot_restore_failure_reloads_committed_content_and_keeps_the_writer_identity() {
    let mut durable =
        Manifest::create(WorkspaceId::new(), "Cloud name", Timestamp::from_millis(1)).unwrap();
    let page = DocumentId::new();
    durable
        .add_document(page, "Retained page", "page", None)
        .unwrap();
    let version = durable.version();
    let restored = restore_manifest(b"damaged rollback snapshot", 42, || Ok(durable)).unwrap();
    assert!(restored.reloaded);
    assert_eq!(restored.manifest.peer_id(), 42);
    assert_eq!(restored.manifest.version(), version);
    assert_eq!(restored.manifest.name().as_deref(), Some("Cloud name"));
    assert!(!restored.manifest.pinned().unwrap_or(false));
    assert_eq!(restored.manifest.entries().len(), 1);
    let mut manifest = restored.manifest;
    manifest.set_pinned(true).unwrap();
    let reopened = Manifest::from_snapshot(&manifest.export_snapshot().unwrap()).unwrap();
    assert_eq!(reopened.pinned(), Some(true));
    assert_eq!(reopened.entries().len(), 1);
}

#[test]
fn valid_snapshot_avoids_reload_and_failed_reload_never_invents_an_empty_manifest() {
    let manifest =
        Manifest::create(WorkspaceId::new(), "Saved", Timestamp::from_millis(1)).unwrap();
    let restored = restore_manifest(&manifest.export_snapshot().unwrap(), 71, || {
        panic!("a valid snapshot must not touch storage")
    })
    .unwrap();
    assert!(!restored.reloaded);
    assert_eq!(restored.manifest.peer_id(), 71);
    let failed = restore_manifest(b"damaged", 71, || Err(Error::bare(Code::StorageFailed)));
    assert!(matches!(failed, Err(error) if error.code() == Code::StorageFailed));
}
