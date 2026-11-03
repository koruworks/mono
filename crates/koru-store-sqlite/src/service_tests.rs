// SPDX-License-Identifier: MPL-2.0

//! Service-level coverage for blob provenance: which record answers
//! `blob_origin`. The repo halves are pinned beside `SqliteWorkspace`; this
//! pins their composition — a held blob's own attributes first, the
//! advertisement a peer sent second.

use koru_store_core::model::{Blob, BlobAttribute, BlobHash, Timestamp, blob_attr};
use koru_store_core::service::Store;
use koru_store_core::{Locale, WorkspaceId};

use crate::{SqliteBackend, SqliteCatalog};

mod account_provisioning;
#[cfg(feature = "disk-tests")]
mod cloud_restart;
mod document_private;
mod manifest_origin;
mod objects;
mod profile_projection;
mod transport_modes;

/// Every file in the one object store, by name. The assertion that a
/// cross-Workspace association copied nothing is a count of these.
fn objects_on_disk(root: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join("blobs").join("objects"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

fn at() -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000)
}

fn store() -> (tempfile::TempDir, Store<SqliteCatalog, SqliteBackend>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(
        SqliteCatalog::in_memory().unwrap(),
        SqliteBackend::new(dir.path()),
    )
    .unwrap();
    (dir, store)
}

fn workspace(store: &mut Store<SqliteCatalog, SqliteBackend>) -> WorkspaceId {
    let profile = store
        .create_profile("Work", None, None, Locale::default(), at())
        .unwrap();
    store.create_workspace(profile.id, "ws", at()).unwrap().id
}

fn advertised(hash: &BlobHash, origin: &str) -> Blob {
    let mut attributes = std::collections::BTreeMap::new();
    attributes.insert(
        blob_attr::ORIGIN_DEVICE.to_string(),
        BlobAttribute::Text(origin.to_string()),
    );
    Blob {
        hash: hash.clone(),
        size: 10,
        mime: "image/png".into(),
        filename: None,
        created_at: at(),
        attributes,
    }
}

#[test]
fn an_advertised_origin_names_a_blob_we_never_held() {
    let (_dir, mut store) = store();
    let workspace = workspace(&mut store);
    let hash = BlobHash::from_digest("b3", &"ab".repeat(32)).unwrap();

    assert_eq!(store.blob_origin(workspace, &hash).unwrap(), None);
    let recorded = store
        .record_blob_origins(workspace, &[advertised(&hash, "device-a")], at())
        .unwrap();
    assert_eq!(recorded, 1);
    assert_eq!(
        store.blob_origin(workspace, &hash).unwrap(),
        Some("device-a".to_string())
    );

    // A re-advertisement is the same fact, not a new one — and provenance is
    // never last-touch.
    let recorded = store
        .record_blob_origins(workspace, &[advertised(&hash, "device-b")], at())
        .unwrap();
    assert_eq!(recorded, 0);
    assert_eq!(
        store.blob_origin(workspace, &hash).unwrap(),
        Some("device-a".to_string())
    );
}

#[test]
fn a_held_blobs_own_attributes_answer_first() {
    let (_dir, mut store) = store();
    let workspace = workspace(&mut store);
    // Bytes we hold: the put path, where the hash comes back from the bytes.
    let held = store
        .put_blob(
            workspace,
            b"0123456789",
            "image/png",
            None,
            at(),
            Some("device-a"),
        )
        .unwrap();
    // An advertisement for the same hash names an origin too; the held row is
    // the record, so the advertisement table gains nothing.
    let blob = advertised(&held.hash, "device-a");
    let recorded = store
        .record_blob_origins(workspace, std::slice::from_ref(&blob), at())
        .unwrap();
    assert_eq!(recorded, 0);
    assert_eq!(
        store.blob_origin(workspace, &held.hash).unwrap(),
        Some("device-a".to_string())
    );
}

#[test]
fn an_advertisement_without_an_origin_records_nothing() {
    let (_dir, mut store) = store();
    let workspace = workspace(&mut store);
    let mut blob = advertised(
        &BlobHash::from_digest("b3", &"ef".repeat(32)).unwrap(),
        "device-a",
    );
    blob.attributes.clear();
    let recorded = store.record_blob_origins(workspace, &[blob], at()).unwrap();
    assert_eq!(recorded, 0);
}

#[test]
fn associating_a_sibling_workspace_is_durable_and_copies_no_bytes() {
    let (_dir, mut store) = store();
    let profile = store
        .create_profile("Work", None, None, Locale::default(), at())
        .unwrap();
    let source = store
        .create_workspace(profile.id, "Source", at())
        .unwrap()
        .id;
    let target = store
        .create_workspace(profile.id, "Target", at())
        .unwrap()
        .id;
    let source_blob = store
        .put_blob(
            source,
            b"image bytes",
            "image/png",
            Some("diagram.png"),
            at(),
            Some("device-a"),
        )
        .unwrap();

    let before = objects_on_disk(_dir.path());
    let associated = store
        .associate_blob(source, target, &source_blob.hash)
        .unwrap();

    assert_eq!(
        associated, source_blob,
        "metadata and provenance follow the association"
    );
    assert_eq!(
        store.get_blob(target, &associated.hash).unwrap(),
        b"image bytes"
    );
    assert_eq!(
        objects_on_disk(_dir.path()),
        before,
        "one object serves both workspaces; nothing was copied"
    );
}

#[test]
fn a_legacy_reference_repairs_only_from_the_same_profile() {
    let (_dir, mut store) = store();
    let owner = store
        .create_profile("Owner", None, None, Locale::default(), at())
        .unwrap();
    let source = store.create_workspace(owner.id, "Source", at()).unwrap().id;
    let target = store.create_workspace(owner.id, "Target", at()).unwrap().id;
    let sibling = store
        .put_blob(source, b"sibling", "image/png", None, at(), None)
        .unwrap();
    assert!(
        store
            .repair_blob_association(target, &sibling.hash)
            .unwrap()
    );
    assert_eq!(store.get_blob(target, &sibling.hash).unwrap(), b"sibling");

    let other = store
        .create_profile("Other", None, None, Locale::default(), at())
        .unwrap();
    let foreign = store
        .create_workspace(other.id, "Foreign", at())
        .unwrap()
        .id;
    let foreign_blob = store
        .put_blob(foreign, b"foreign", "image/png", None, at(), None)
        .unwrap();
    assert!(
        !store
            .repair_blob_association(target, &foreign_blob.hash)
            .unwrap()
    );
}
