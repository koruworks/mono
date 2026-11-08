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

// The maintainer permits new empty WAL coordination files. Existing files
// still must be byte-identical, and no other new persisted file is allowed.
fn assert_read_only(
    before: &BTreeMap<String, (u64, String)>,
    after: &BTreeMap<String, (u64, String)>,
) {
    for (path, bytes) in before {
        assert_eq!(
            after.get(path),
            Some(bytes),
            "existing file changed: {path}"
        );
    }
    for (path, (size, hash)) in after {
        if !before.contains_key(path) {
            assert!(
                path.ends_with("-wal") && *size == 0,
                "unexpected new file: {path}"
            );
            assert_eq!(hash, &blake3::hash(&[]).to_hex().to_string());
        }
    }
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
    assert_read_only(&before, &inventory(root));
    assert!(!backend.workspace_dir(missing).exists());
    println!(
        "disk read-only: missing stays absent; Live/Trash verified; schemas {}/{} refused; {} persisted files byte-identical",
        WORKSPACE_SCHEMA_VERSION - 1,
        WORKSPACE_SCHEMA_VERSION + 1,
        before.len()
    );
}

#[test]
fn authority_inventory_reports_capture_and_total_time_with_live_wal() {
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
        let mut longest_lock_ms = 0.0_f64;
        for _ in 0..5 {
            let started = Instant::now();
            let ids = {
                let guard = store.lock().unwrap();
                let began = Instant::now();
                let ids = guard.workspace_authority_ids(owner).unwrap();
                longest_lock_ms = longest_lock_ms.max(began.elapsed().as_secs_f64() * 1000.0);
                ids
            };
            let mut authority = Vec::new();
            for id in ids {
                let read = {
                    let guard = store.lock().unwrap();
                    let began = Instant::now();
                    let read = guard.prepare_workspace_authority(owner, id).unwrap();
                    longest_lock_ms = longest_lock_ms.max(began.elapsed().as_secs_f64() * 1000.0);
                    read
                };
                // Same boundary as the HTTP route: another writer can acquire
                // the mutex before replay finishes, regardless of inventory size.
                assert!(store.try_lock().is_ok());
                authority.push(read.finish().unwrap());
            }
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(authority.len(), count);
            assert!(authority.iter().all(|row| matches!(
                row.authority,
                WorkspaceAuthorityState::Read { applied: true, .. }
            )));
        }
        samples.sort_by(f64::total_cmp);
        println!(
            "AUTHORITY_READ inventory={count} total_median_ms={:.3} total_max_ms={:.3} longest_lock_ms={longest_lock_ms:.3} samples=5 (detached replay, live WAL)",
            samples[2], samples[4]
        );
    }
    assert_read_only(&before, &inventory(root));
}

/// Explicit disk lane: a detached WAL snapshot must not observe a later
/// lifecycle commit, even when decoding starts after that commit.
#[test]
fn detached_wal_inspection_keeps_its_captured_lifecycle() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(
        SqliteCatalog::open(&directory.path().join("catalog.db")).unwrap(),
        SqliteBackend::new(directory.path()),
    )
    .unwrap();
    let owner = store
        .create_profile("Owner", None, None, Locale::default(), Timestamp::now())
        .unwrap();
    let workspace = store
        .create_workspace(owner.id, "Before", Timestamp::now())
        .unwrap();
    let read = store
        .prepare_workspace_authority(owner.id, workspace.id)
        .unwrap();
    store
        .delete_workspace(workspace.id, Timestamp::now())
        .unwrap();
    let before = read.finish().unwrap();
    assert!(matches!(
        before.authority,
        WorkspaceAuthorityState::Read {
            deleted_at: None,
            applied: true,
            ..
        }
    ));
    let after = store
        .prepare_workspace_authority(owner.id, workspace.id)
        .unwrap()
        .finish()
        .unwrap();
    assert!(matches!(
        after.authority,
        WorkspaceAuthorityState::Read {
            deleted_at: Some(_),
            applied: true,
            ..
        }
    ));
}
