// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{SqliteBackend, SqliteCatalog};
use koru_store_core::{
    Locale,
    service::{Backend, Store, WorkspaceAuthorityState},
};
use std::{collections::BTreeMap, path::Path, sync::Mutex, time::Instant};

fn inventory(root: &Path) -> BTreeMap<String, (u64, String)> {
    fn walk(root: &Path, directory: &Path, files: &mut BTreeMap<String, (u64, String)>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, files);
            } else {
                // SQLite's shared-memory coordination is not persisted content.
                // Database and WAL bytes are both checked, including new files.
                if path.to_string_lossy().ends_with("-shm") {
                    continue;
                }
                let bytes = std::fs::read(&path).unwrap();
                files.insert(
                    path.strip_prefix(root).unwrap().to_string_lossy().into(),
                    (
                        bytes.len() as u64,
                        blake3::hash(&bytes).to_hex().to_string(),
                    ),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    files
}

#[test]
fn disk_inspection_keeps_missing_unsupported_and_trash_storage_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let backend = SqliteBackend::new(root);
    let missing = WorkspaceId::new();
    assert!(matches!(
        backend.inspect_workspace_manifest(missing),
        WorkspaceManifestInspection::Unavailable(_)
    ));
    assert!(inventory(root).is_empty());
    assert!(!backend.workspace_dir(missing).exists());
    let mut store = Store::open(
        SqliteCatalog::open(&root.join("catalog.db")).unwrap(),
        SqliteBackend::new(root),
    )
    .unwrap();
    let profile = store
        .create_profile(
            "Disk fixture",
            None,
            None,
            Locale::default(),
            Timestamp::now(),
        )
        .unwrap();
    let healthy = store
        .create_workspace(profile.id, "Live", Timestamp::now())
        .unwrap();
    let trashed = store
        .create_workspace(profile.id, "Trash", Timestamp::now())
        .unwrap();
    store
        .delete_workspace(trashed.id, Timestamp::now())
        .unwrap();
    let older = store
        .create_workspace(profile.id, "Older", Timestamp::now())
        .unwrap();
    let newer = store
        .create_workspace(profile.id, "Newer", Timestamp::now())
        .unwrap();
    drop(store);
    for (workspace, version) in [
        (older.id, WORKSPACE_SCHEMA_VERSION - 1),
        (newer.id, WORKSPACE_SCHEMA_VERSION + 1),
    ] {
        Connection::open(backend.workspace_db(workspace))
            .unwrap()
            .pragma_update(None, "user_version", version)
            .unwrap();
    }
    let before = inventory(root);
    assert!(matches!(
        backend.inspect_workspace_manifest(healthy.id),
        WorkspaceManifestInspection::Verified(_)
    ));
    assert!(
        matches!(backend.inspect_workspace_manifest(trashed.id), WorkspaceManifestInspection::Verified(v) if v.deleted_at.is_some())
    );
    for workspace in [older.id, newer.id, missing] {
        assert!(matches!(
            backend.inspect_workspace_manifest(workspace),
            WorkspaceManifestInspection::Unavailable(_)
        ));
    }
    assert_eq!(
        inventory(root),
        before,
        "inspection changed persisted files"
    );
    assert!(!backend.workspace_dir(missing).exists());
    println!(
        "disk read-only: missing stays absent; Live/Trash verified; schemas {}/{} refused; {} persisted files byte-identical",
        WORKSPACE_SCHEMA_VERSION - 1,
        WORKSPACE_SCHEMA_VERSION + 1,
        before.len()
    );
}

#[test]
fn authority_inventory_cost_is_measured_under_the_store_lock_with_live_wal() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let mut store = Store::open(
        SqliteCatalog::open(&root.join("catalog.db")).unwrap(),
        SqliteBackend::new(root),
    )
    .unwrap();
    let mut owners = Vec::new();
    for count in [1, 10, 100] {
        let profile = store
            .create_profile("Inventory", None, None, Locale::default(), Timestamp::now())
            .unwrap();
        for _ in 0..count {
            store
                .create_workspace(profile.id, "Workspace", Timestamp::now())
                .unwrap();
        }
        owners.push((profile.id, count));
    }
    let store = Mutex::new(store);
    let before = inventory(root);
    for (owner, count) in owners {
        let mut samples = Vec::new();
        for _ in 0..5 {
            let guard = store.lock().unwrap();
            let started = Instant::now();
            let authority = guard.workspace_authority(owner).unwrap();
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(authority.len(), count);
            assert!(authority.iter().all(|row| matches!(
                row.authority,
                WorkspaceAuthorityState::Read { applied: true, .. }
            )));
        }
        samples.sort_by(f64::total_cmp);
        println!(
            "AUTHORITY_READ inventory={count} median_ms={:.3} max_ms={:.3} samples=5 (Store lock, live WAL)",
            samples[2], samples[4]
        );
    }
    assert_eq!(
        inventory(root),
        before,
        "authority read changed database or WAL bytes"
    );
}
