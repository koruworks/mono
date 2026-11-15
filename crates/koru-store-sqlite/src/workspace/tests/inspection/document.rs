// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_core::service::DocumentInspectionBudget;

fn budget() -> DocumentInspectionBudget {
    DocumentInspectionBudget {
        payload_bytes: 1_048_576,
        journal_rows: 100,
        metadata_bytes: 4096,
    }
}
fn write(repo: &mut SqliteWorkspace, id: DocumentId, doc: &loro::LoroDoc, bytes: Vec<u8>) -> u64 {
    repo.commit_updates(&UpdateBatch {
        document: id,
        updates: vec![bytes],
        version_vector: doc.state_vv().encode(),
        frontiers: doc.state_frontiers().encode(),
        received_at: at(),
        origins: vec![UpdateOrigin::Store],
    })
    .unwrap()
}
fn content(repo: &mut SqliteWorkspace, id: DocumentId, text: &str) -> loro::LoroDoc {
    repo.create_document(id, DocumentKind::Document, at())
        .unwrap();
    let doc = loro::LoroDoc::new();
    doc.get_text("body").insert(0, text).unwrap();
    let bytes = doc.export(loro::ExportMode::all_updates()).unwrap();
    write(repo, id, &doc, bytes);
    doc
}
#[test]
fn detached_unfiled_documents_keep_exact_workspace_and_never_adopt() {
    let (backend, a, _, mut repo_a, _) = fixture();
    let b = WorkspaceId::new();
    let mut repo_b = backend.create_workspace(b).unwrap();
    let id = DocumentId::new();
    content(&mut repo_a, id, "Workspace A");
    content(&mut repo_b, id, "Workspace B");
    for (workspace, expected) in [(a, "Workspace A"), (b, "Workspace B")] {
        let detached = backend.prepare_document_inspection(workspace, id, budget())().unwrap();
        assert_eq!(detached.workspace(), workspace);
        assert_eq!(detached.document(), id);
        assert_eq!(detached.into_doc().get_text("body").to_string(), expected);
    }
    for repo in [&repo_a, &repo_b] {
        assert_eq!(
            repo.connection
                .query_row("SELECT count(*) FROM documents", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(repo.update_count(id).unwrap(), 1);
    }
}
#[test]
fn detached_missing_fresh_pending_and_wrong_positions_are_refused_without_repair() {
    let (backend, workspace, manifest, mut repo, _) = fixture();
    assert!(
        backend.prepare_document_inspection(WorkspaceId::new(), DocumentId::new(), budget())()
            .is_err()
    );
    assert_eq!(
        backend.prepare_document_inspection(workspace, DocumentId::new(), budget())()
            .unwrap_err()
            .code(),
        Code::DocumentNotFound
    );
    assert_eq!(
        backend.prepare_document_inspection(workspace, manifest, budget())()
            .unwrap_err()
            .code(),
        Code::InvalidRequest
    );
    let fresh = DocumentId::new();
    repo.create_document(fresh, DocumentKind::Document, at())
        .unwrap();
    assert!(backend.prepare_document_inspection(workspace, fresh, budget())().is_err());
    let id = DocumentId::new();
    repo.create_document(id, DocumentKind::Document, at())
        .unwrap();
    let doc = loro::LoroDoc::new();
    doc.get_text("body").insert(0, "parent").unwrap();
    let prior = doc.oplog_vv();
    doc.get_text("body").insert(6, " child").unwrap();
    write(
        &mut repo,
        id,
        &doc,
        doc.export(loro::ExportMode::updates(&prior)).unwrap(),
    );
    assert_eq!(
        backend.prepare_document_inspection(workspace, id, budget())()
            .unwrap_err()
            .code(),
        Code::CorruptState
    );
    let valid = DocumentId::new();
    content(&mut repo, valid, "valid");
    repo.connection
        .execute(
            "UPDATE loro_documents SET version_vector=?1 WHERE id=?2",
            params![loro::VersionVector::default().encode(), valid.to_storage()],
        )
        .unwrap();
    assert_eq!(
        backend.prepare_document_inspection(workspace, valid, budget())()
            .unwrap_err()
            .code(),
        Code::CorruptState
    );
    assert_eq!(repo.update_count(id).unwrap(), 1);
    assert_eq!(
        repo.document(fresh).unwrap().unwrap().version_vector,
        Vec::<u8>::new()
    );
}
#[test]
fn detached_replay_bounds_bytes_rows_and_metadata_before_loading_content() {
    let (backend, workspace, _, mut repo, _) = fixture();
    let id = DocumentId::new();
    let doc = content(&mut repo, id, "first");
    let prior = doc.oplog_vv();
    doc.get_text("body").insert(5, " second").unwrap();
    write(
        &mut repo,
        id,
        &doc,
        doc.export(loro::ExportMode::updates(&prior)).unwrap(),
    );
    for limits in [
        DocumentInspectionBudget {
            payload_bytes: 1,
            ..budget()
        },
        DocumentInspectionBudget {
            journal_rows: 1,
            ..budget()
        },
        DocumentInspectionBudget {
            metadata_bytes: 1,
            ..budget()
        },
    ] {
        assert_eq!(
            backend.prepare_document_inspection(workspace, id, limits)()
                .unwrap_err()
                .code(),
            Code::PayloadTooLarge
        );
    }
    assert_eq!(
        backend.prepare_document_inspection(workspace, id, budget())()
            .unwrap()
            .into_doc()
            .get_text("body")
            .to_string(),
        "first second"
    );
}
#[test]
fn detached_checkpoint_damage_never_uses_an_older_recovery_prefix() {
    let (backend, workspace, _, mut repo, _) = fixture();
    let id = DocumentId::new();
    let doc = content(&mut repo, id, "checkpoint");
    let snapshot = doc.export(loro::ExportMode::Snapshot).unwrap();
    repo.commit_checkpoint(
        id,
        &Checkpoint {
            seq: 1,
            checksum: checksum(&snapshot),
            snapshot,
            version_vector: doc.state_vv().encode(),
            frontiers: doc.state_frontiers().encode(),
            created_at: at(),
        },
    )
    .unwrap();
    assert!(backend.prepare_document_inspection(workspace, id, budget())().is_ok());
    repo.connection
        .execute(
            "UPDATE loro_checkpoints SET checksum='broken' WHERE document_id=?1",
            [id.to_storage()],
        )
        .unwrap();
    assert_eq!(
        backend.prepare_document_inspection(workspace, id, budget())()
            .unwrap_err()
            .code(),
        Code::CorruptState
    );
    assert_eq!(repo.checkpoints(id).unwrap().len(), 1);
    assert_eq!(repo.update_count(id).unwrap(), 1);
}

#[test]
fn detached_unknown_schema_is_refused_without_migration() {
    let (backend, workspace, _, mut repo, _) = fixture();
    let id = DocumentId::new();
    content(&mut repo, id, "existing");
    for version in [WORKSPACE_SCHEMA_VERSION - 1, WORKSPACE_SCHEMA_VERSION + 1] {
        repo.connection
            .pragma_update(None, "user_version", version)
            .unwrap();
        assert!(backend.prepare_document_inspection(workspace, id, budget())().is_err());
        assert_eq!(repo.schema_version().unwrap(), version);
    }
}

#[test]
fn detached_metadata_limits_charge_sqlite_bytes_including_nul_and_multibyte_text() {
    let (backend, workspace, _, mut repo, _) = fixture();
    let id = DocumentId::new();
    let doc = content(&mut repo, id, "bounded");
    let snapshot = doc.export(loro::ExportMode::Snapshot).unwrap();
    repo.commit_checkpoint(
        id,
        &Checkpoint {
            seq: 1,
            checksum: checksum(&snapshot),
            snapshot,
            version_vector: doc.state_vv().encode(),
            frontiers: doc.state_frontiers().encode(),
            created_at: at(),
        },
    )
    .unwrap();
    for bad in [
        format!("document\0{}", "x".repeat(20_000)),
        "\u{20ac}".repeat(2000),
    ] {
        repo.connection
            .execute(
                "UPDATE loro_documents SET kind=?1 WHERE id=?2",
                params![bad, id.to_storage()],
            )
            .unwrap();
        assert_eq!(
            backend.prepare_document_inspection(workspace, id, budget())()
                .unwrap_err()
                .code(),
            Code::PayloadTooLarge
        );
    }
    repo.connection
        .execute(
            "UPDATE loro_documents SET kind='document' WHERE id=?1",
            [id.to_storage()],
        )
        .unwrap();
    for bad in [
        format!("hash\0{}", "x".repeat(20_000)),
        "\u{20ac}".repeat(2000),
    ] {
        repo.connection
            .execute(
                "UPDATE loro_checkpoints SET checksum=?1 WHERE document_id=?2",
                params![bad, id.to_storage()],
            )
            .unwrap();
        assert_eq!(
            backend.prepare_document_inspection(workspace, id, budget())()
                .unwrap_err()
                .code(),
            Code::PayloadTooLarge
        );
    }
    // SQLite is not STRICT: a TEXT value in a nominal BLOB column must use
    // its byte length before the typed decoder sees the invalid storage type.
    repo.connection
        .execute(
            "UPDATE loro_documents SET version_vector=?1 WHERE id=?2",
            params![format!("\0{}", "x".repeat(20_000)), id.to_storage()],
        )
        .unwrap();
    assert_eq!(
        backend.prepare_document_inspection(workspace, id, budget())()
            .unwrap_err()
            .code(),
        Code::PayloadTooLarge
    );
}
