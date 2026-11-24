// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_core::pages::purge::Coverage;

#[test]
fn closed_store_source_has_known_coverage_and_immutable_replay() {
    let root = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let source = WorkspaceDoc::create(workspace, "Closed Trash document").unwrap();
    let expected = source.version();
    let mut reads = 0;
    let events = prepare(
        workspace,
        document,
        "device",
        vec![(document, None)],
        Some(root.path()),
        |id| {
            assert_eq!(id, document);
            reads += 1;
            Ok(Some(source.export_snapshot().unwrap()))
        },
    )
    .unwrap();
    assert_eq!(reads, 1);
    assert_eq!(Coverage::of(&events), Some(Coverage::Known(expected)));
    let journal = root.path().join("purges");
    write(&journal, &workspace.to_storage(), &events).unwrap();
    let restored = read(&journal, &workspace.to_storage()).unwrap();
    assert_eq!(restored, vec![events.clone()]);
    write(&journal, &workspace.to_storage(), &restored[0]).unwrap();
    assert_eq!(
        read(&journal, &workspace.to_storage()).unwrap(),
        vec![events]
    );
}

#[test]
fn closed_source_includes_durable_outbox_changes_without_consuming_them() {
    let root = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let mut source = WorkspaceDoc::page_genesis(workspace, document).unwrap();
    let stored = source.export_snapshot().unwrap();
    let acked = source.version();
    source
        .set_text(
            koru_core::schema::genesis_seed_id(document),
            "offline changes",
        )
        .unwrap();
    let expected = source.version();
    assert_ne!(expected, acked);
    let buffer = outbox::write(
        Some(root.path()),
        explore::store_workspace(workspace),
        explore::store_document(document),
        source.peer_id(),
        &acked,
        &source.export_updates_since(&acked).unwrap(),
    )
    .unwrap();
    let events = prepare(
        workspace,
        document,
        "device",
        vec![(document, None)],
        Some(root.path()),
        |_| Ok(Some(stored.clone())),
    )
    .unwrap();
    assert_eq!(Coverage::of(&events), Some(Coverage::Known(expected)));
    assert!(
        buffer.exists(),
        "intent preparation does not release the source"
    );
}

#[test]
fn acknowledged_open_version_is_not_replaced_by_an_older_store_copy() {
    let root = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let source = WorkspaceDoc::create(workspace, "acknowledged open writing").unwrap();
    let expected = source.version();
    let events = prepare(
        workspace,
        document,
        "device",
        vec![(document, Some(expected.clone()))],
        Some(root.path()),
        |_| panic!("already captured acknowledged writing must not be reread"),
    )
    .unwrap();
    assert_eq!(Coverage::of(&events), Some(Coverage::Known(expected)));
}

#[test]
fn closed_page_revision_supplies_coverage_before_store_registration() {
    let root = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::new();
    let page = koru_core::ids::PageKey::List(koru_core::ids::ListId::new());
    let document = koru_core::ids::page_document_id(workspace, page, 0);
    let mut source = WorkspaceDoc::page_genesis(workspace, document).unwrap();
    source
        .set_text(
            koru_core::schema::genesis_seed_id(document),
            "unregistered page",
        )
        .unwrap();
    let record = revisions::Record {
        workspace: workspace.to_storage(),
        document: document.to_storage(),
        page: page.to_storage(),
        generation: 0,
        state: "registering".into(),
        seen_listed: false,
        blobs: vec![],
        version: source.version().encode(),
    };
    let dir = outbox::dir(
        Some(root.path()),
        explore::store_workspace(workspace),
        explore::store_document(document),
    )
    .unwrap();
    revisions::WriteRevision::new(
        &dir,
        revisions::RevisionName {
            replica: source.peer_id(),
            rev: 1,
        },
        "test",
        source.export_all_updates().unwrap(),
        vec![],
        &record,
    )
    .run(&mut revisions::DiskFs)
    .unwrap();
    let events = prepare(
        workspace,
        document,
        "device",
        vec![(document, None)],
        Some(root.path()),
        |_| Ok(None),
    )
    .unwrap();
    assert_eq!(
        Coverage::of(&events),
        Some(Coverage::Known(source.version()))
    );
    assert_eq!(
        revisions::discover(&revisions::DiskFs, &dir)
            .unwrap()
            .complete
            .len(),
        1
    );
}

#[test]
fn only_a_missing_local_source_becomes_unknown() {
    let root = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let events = prepare(
        workspace,
        document,
        "device",
        vec![(document, None)],
        Some(root.path()),
        |_| Ok(None),
    )
    .unwrap();
    assert_eq!(Coverage::of(&events), Some(Coverage::Unknown));
    assert!(
        prepare(
            workspace,
            document,
            "device",
            vec![(document, None)],
            Some(root.path()),
            |_| Err("Store unavailable".into())
        )
        .is_err()
    );
    assert!(
        prepare(
            workspace,
            document,
            "device",
            vec![(document, None)],
            Some(root.path()),
            |_| Ok(Some(vec![0, 1, 2]))
        )
        .is_err()
    );
    let dir = outbox::dir(
        Some(root.path()),
        explore::store_workspace(workspace),
        explore::store_document(document),
    )
    .unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("0000000000000001.loro"), b"damaged").unwrap();
    assert!(
        prepare(
            workspace,
            document,
            "device",
            vec![(document, None)],
            Some(root.path()),
            |_| Ok(None)
        )
        .is_err()
    );
}
