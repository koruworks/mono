// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::SqliteBackend;
use koru_store_core::{
    WorkspaceId,
    manifest::Manifest,
    service::{Backend, WorkspaceManifestInspection as Inspection},
};

fn fixture() -> (
    SqliteBackend,
    WorkspaceId,
    DocumentId,
    SqliteWorkspace,
    Manifest,
) {
    let backend = SqliteBackend::in_memory();
    let workspace = WorkspaceId::new();
    let mut repo = backend.create_workspace(workspace).unwrap();
    let document = DocumentId::new();
    repo.create_document(document, DocumentKind::Manifest, at())
        .unwrap();
    let manifest = Manifest::create(workspace, "Workspace", at()).unwrap();
    (backend, workspace, document, repo, manifest)
}

fn commit(
    repo: &mut SqliteWorkspace,
    document: DocumentId,
    manifest: &Manifest,
    bytes: Vec<u8>,
) -> u64 {
    repo.commit_updates(&UpdateBatch {
        document,
        updates: vec![bytes],
        version_vector: manifest.version().encode(),
        frontiers: manifest.frontiers().encode(),
        received_at: at(),
        origins: vec![UpdateOrigin::Store],
    })
    .unwrap()
}

fn checkpoint(repo: &mut SqliteWorkspace, document: DocumentId, manifest: &Manifest, seq: u64) {
    let snapshot = manifest.export_snapshot().unwrap();
    repo.commit_checkpoint(
        document,
        &Checkpoint {
            seq,
            checksum: checksum(&snapshot),
            snapshot,
            version_vector: manifest.version().encode(),
            frontiers: manifest.frontiers().encode(),
            created_at: at(),
        },
    )
    .unwrap();
}

#[test]
fn missing_named_repository_stays_missing_and_provisioned_is_pending() {
    let (backend, workspace, document, repo, _) = fixture();
    let missing = WorkspaceId::new();
    assert!(matches!(
        backend.inspect_workspace_manifest(missing),
        Inspection::Unavailable(_)
    ));
    // Exclusive claim would refuse if inspection had created the repository.
    backend.create_workspace(missing).unwrap();
    assert!(
        matches!(backend.inspect_workspace_manifest(workspace), Inspection::Pending { manifest } if manifest == document)
    );
    assert_eq!(repo.update_count(document).unwrap(), 0);
}

#[test]
fn checkpoint_only_manifest_is_verified_and_read_only() {
    let (backend, workspace, document, mut repo, manifest) = fixture();
    let seq = commit(
        &mut repo,
        document,
        &manifest,
        manifest.export_all_updates().unwrap(),
    );
    checkpoint(&mut repo, document, &manifest, seq);
    repo.connection
        .execute("DELETE FROM loro_updates", [])
        .unwrap();
    let version = repo.schema_version().unwrap();
    let before = repo.document(document).unwrap().unwrap();
    let Inspection::Verified(authority) = backend.inspect_workspace_manifest(workspace) else {
        panic!("checkpoint is committed content")
    };
    assert_eq!(authority.workspace, workspace);
    assert_eq!(authority.documents, vec![document]);
    assert_eq!(authority.name, "Workspace");
    assert_eq!(repo.schema_version().unwrap(), version);
    assert_eq!(
        repo.document(document).unwrap().unwrap().version_vector,
        before.version_vector
    );
    assert_eq!(repo.update_count(document).unwrap(), 0);
}

#[test]
fn unsupported_layout_never_migrates_and_invalid_manifest_never_counts_as_empty() {
    let (backend, workspace, document, mut repo, _) = fixture();
    for version in [WORKSPACE_SCHEMA_VERSION - 1, WORKSPACE_SCHEMA_VERSION + 1] {
        repo.connection
            .pragma_update(None, "user_version", version)
            .unwrap();
        assert!(matches!(
            backend.inspect_workspace_manifest(workspace),
            Inspection::Unavailable(_)
        ));
        assert_eq!(repo.schema_version().unwrap(), version);
    }
    repo.connection
        .pragma_update(None, "user_version", WORKSPACE_SCHEMA_VERSION)
        .unwrap();
    let wrong = Manifest::create(WorkspaceId::new(), "Other", at()).unwrap();
    commit(
        &mut repo,
        document,
        &wrong,
        wrong.export_all_updates().unwrap(),
    );
    assert!(matches!(
        backend.inspect_workspace_manifest(workspace),
        Inspection::Unavailable(_)
    ));
    repo.create_document(DocumentId::new(), DocumentKind::Manifest, at())
        .unwrap();
    assert!(matches!(
        backend.inspect_workspace_manifest(workspace),
        Inspection::Unavailable(_)
    ));
    repo.connection
        .execute("DELETE FROM loro_documents", [])
        .unwrap();
    assert!(matches!(
        backend.inspect_workspace_manifest(workspace),
        Inspection::Unavailable(_)
    ));
}

#[test]
fn recovered_checkpoint_keeps_recovery_even_when_an_older_manifest_is_valid() {
    let (backend, workspace, document, mut repo, manifest) = fixture();
    let first = commit(
        &mut repo,
        document,
        &manifest,
        manifest.export_all_updates().unwrap(),
    );
    checkpoint(&mut repo, document, &manifest, first);
    let version = manifest.version();
    manifest.set_name("Newer").unwrap();
    let second = commit(
        &mut repo,
        document,
        &manifest,
        manifest.export_updates_since(&version).unwrap(),
    );
    checkpoint(&mut repo, document, &manifest, second);
    repo.connection
        .execute(
            "UPDATE loro_checkpoints SET checksum = 'damaged' WHERE seq = ?1",
            [second as i64],
        )
        .unwrap();
    let Inspection::Recovered(recovery) = backend.inspect_workspace_manifest(workspace) else {
        panic!("recovery must not become verified authority")
    };
    assert_eq!(recovery.from_checkpoint, Some(first));
    assert_eq!(recovery.quarantined, vec![format!("checkpoint {second}")]);
    assert_eq!(repo.checkpoints(document).unwrap().len(), 2);
}

#[test]
fn missing_causal_ancestors_remain_pending_and_uncommitted_memory_is_invisible() {
    let (backend, workspace, document, mut repo, manifest) = fixture();
    let version = manifest.version();
    manifest.set_name("Awaiting ancestors").unwrap();
    commit(
        &mut repo,
        document,
        &manifest,
        manifest.export_updates_since(&version).unwrap(),
    );
    assert!(matches!(
        backend.inspect_workspace_manifest(workspace),
        Inspection::Pending { .. }
    ));
    commit(
        &mut repo,
        document,
        &manifest,
        manifest.export_all_updates().unwrap(),
    );
    manifest.set_name("Only in memory").unwrap();
    let Inspection::Verified(authority) = backend.inspect_workspace_manifest(workspace) else {
        panic!("ancestors completed the manifest")
    };
    assert_eq!(authority.name, "Awaiting ancestors");
}
