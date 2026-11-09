// SPDX-License-Identifier: MPL-2.0

//! One object per store root, and the Workspace row as the permission.
//!
//! These are the claims of `ADR-000-blobobjectworker`'s *"One local object,
//! and the association is the permission"*, written as assertions rather than
//! prose. Two of them are here
//! specifically because the failure they describe is silent: a Workspace
//! reading a picture it was never associated with, and a legacy file quietly
//! thrown away by the thing that was meant to preserve it.

use std::path::Path;

use koru_store_core::bundle::ImportMode;
use koru_store_core::errors::Code;

use super::*;

/// A store on a real directory with a real catalog beside it, so it can be
/// closed and opened again. The in-memory catalog the other tests use cannot
/// answer the question these ask.
fn durable_store(directory: &tempfile::TempDir) -> Store<SqliteCatalog, SqliteBackend> {
    Store::open(
        SqliteCatalog::open(&directory.path().join("catalog.db")).unwrap(),
        SqliteBackend::new(directory.path()),
    )
    .unwrap()
}

/// Put an object back into the layout an older build wrote: the bytes under
/// the Workspace's own `blobs/` directory, and nothing in the object store.
///
/// A rename rather than a copy, deliberately — the whole point of the fixture
/// is that the object store does *not* hold these bytes yet.
fn demote_to_legacy(root: &Path, workspace: WorkspaceId, hash: &BlobHash) {
    let backend = SqliteBackend::new(root);
    let legacy = backend.legacy_blobs_dir(workspace);
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::rename(
        backend.objects().objects_dir().join(hash.file_name()),
        legacy.join(hash.file_name()),
    )
    .unwrap();
}

fn legacy_file(root: &Path, workspace: WorkspaceId, hash: &BlobHash) -> std::path::PathBuf {
    SqliteBackend::new(root)
        .legacy_blobs_dir(workspace)
        .join(hash.file_name())
}

// ------------------------------------------------- the association is the key

#[test]
fn one_object_serves_two_associated_workspaces_and_nobody_else() {
    let (directory, mut store) = store();
    let owner = store
        .create_profile("Owner", None, None, Locale::default(), at())
        .unwrap();
    let first = store.create_workspace(owner.id, "First", at()).unwrap().id;
    let second = store.create_workspace(owner.id, "Second", at()).unwrap().id;
    let bystander = store
        .create_workspace(owner.id, "Bystander", at())
        .unwrap()
        .id;

    let blob = store
        .put_blob(first, b"one picture", "image/png", None, at(), None)
        .unwrap();
    store.associate_blob(first, second, &blob.hash).unwrap();

    assert_eq!(
        objects_on_disk(directory.path()),
        vec![blob.hash.file_name()],
        "two associated workspaces, one file"
    );
    assert_eq!(store.get_blob(first, &blob.hash).unwrap(), b"one picture");
    assert_eq!(store.get_blob(second, &blob.hash).unwrap(), b"one picture");

    // The file is right there and grants nothing. A third workspace of the
    // same Profile has no row, so it is refused — this is the whole reason
    // `object_present` and `workspace_has_blob` are two questions.
    assert!(store.object_present(&blob.hash).unwrap());
    assert!(!store.workspace_has_blob(bystander, &blob.hash).unwrap());
    assert_eq!(
        store.get_blob(bystander, &blob.hash).unwrap_err().code(),
        Code::BlobNotFound
    );
    assert_eq!(
        store
            .open_blob(bystander, &blob.hash)
            .err()
            .expect("an unassociated workspace may not stream it either")
            .code(),
        Code::BlobNotFound
    );
    assert_eq!(
        store
            .read_blob_range(bystander, &blob.hash, 0, 4)
            .unwrap_err()
            .code(),
        Code::BlobNotFound
    );
}

#[test]
fn another_profiles_workspace_is_refused_and_repair_will_not_reach_it() {
    let (_directory, mut store) = store();
    let owner = store
        .create_profile("Owner", None, None, Locale::default(), at())
        .unwrap();
    let held = store.create_workspace(owner.id, "Held", at()).unwrap().id;
    let stranger = store
        .create_profile("Stranger", None, None, Locale::default(), at())
        .unwrap();
    let theirs = store
        .create_workspace(stranger.id, "Theirs", at())
        .unwrap()
        .id;

    let blob = store
        .put_blob(held, b"not yours", "image/png", None, at(), None)
        .unwrap();

    assert!(store.object_present(&blob.hash).unwrap());
    assert_eq!(
        store.get_blob(theirs, &blob.hash).unwrap_err().code(),
        Code::BlobNotFound
    );
    assert!(
        !store.repair_blob_association(theirs, &blob.hash).unwrap(),
        "repair searches one Profile's workspaces and never another's"
    );
    assert_eq!(
        store
            .associate_blob(held, theirs, &blob.hash)
            .unwrap_err()
            .code(),
        Code::InvalidRequest
    );
}

// --------------------------------------------------------- the lazy migration

#[test]
fn a_legacy_file_promotes_on_first_read_and_is_never_moved() {
    let (directory, mut store) = store();
    let workspace = workspace(&mut store);
    let blob = store
        .put_blob(
            workspace,
            b"an older build wrote this",
            "image/png",
            None,
            at(),
            None,
        )
        .unwrap();
    demote_to_legacy(directory.path(), workspace, &blob.hash);
    assert!(objects_on_disk(directory.path()).is_empty());

    assert_eq!(
        store.get_blob(workspace, &blob.hash).unwrap(),
        b"an older build wrote this"
    );
    assert_eq!(
        objects_on_disk(directory.path()),
        vec![blob.hash.file_name()],
        "the first read promoted it"
    );
    // **The old file is still there.** That is what makes this rung
    // reversible: an older build opening this store still finds its bytes.
    assert!(
        legacy_file(directory.path(), workspace, &blob.hash).exists(),
        "the legacy file was moved rather than copied"
    );

    // Idempotent: a second read is an ordinary hit and changes nothing.
    assert_eq!(
        store.get_blob(workspace, &blob.hash).unwrap(),
        b"an older build wrote this"
    );
    assert_eq!(
        objects_on_disk(directory.path()),
        vec![blob.hash.file_name()]
    );
    assert!(legacy_file(directory.path(), workspace, &blob.hash).exists());
    assert!(
        store.recoveries().is_empty(),
        "an ordinary promotion is not news: {:?}",
        store.recoveries()
    );
}

#[test]
fn a_legacy_candidate_that_does_not_hash_to_its_name_is_refused_and_kept() {
    let (directory, mut store) = store();
    let workspace = workspace(&mut store);
    let blob = store
        .put_blob(workspace, b"the real bytes", "image/png", None, at(), None)
        .unwrap();
    demote_to_legacy(directory.path(), workspace, &blob.hash);
    // Somebody else's bytes, under a name that promises these ones.
    let candidate = legacy_file(directory.path(), workspace, &blob.hash);
    std::fs::write(&candidate, b"something else entirely").unwrap();

    assert_eq!(
        store.get_blob(workspace, &blob.hash).unwrap_err().code(),
        Code::BlobNotFound,
        "those bytes are not here, and saying they are would be the bug"
    );
    assert!(
        objects_on_disk(directory.path()).is_empty(),
        "nothing was installed under a name it does not have"
    );
    assert_eq!(
        std::fs::read(&candidate).unwrap(),
        b"something else entirely",
        "the candidate is the only copy of whatever it holds; it stays"
    );
    // Visible, not swallowed: this is what the debug route reports.
    let recoveries = store.recoveries().to_vec();
    assert!(
        recoveries
            .iter()
            .any(|line| line.contains(blob.hash.as_str()) && line.contains("left where it is")),
        "{recoveries:?}"
    );
    // And the staging directory is not left holding the rejected copy.
    let staging = SqliteBackend::new(directory.path()).objects().staging_dir();
    assert!(
        std::fs::read_dir(&staging)
            .into_iter()
            .flatten()
            .flatten()
            .next()
            .is_none(),
        "a refused promotion leaves no pending file"
    );
}

#[test]
fn repair_promotes_a_siblings_legacy_bytes_without_copying_them() {
    let (directory, mut store) = store();
    let owner = store
        .create_profile("Owner", None, None, Locale::default(), at())
        .unwrap();
    let source = store.create_workspace(owner.id, "Source", at()).unwrap().id;
    let target = store.create_workspace(owner.id, "Target", at()).unwrap().id;
    let blob = store
        .put_blob(source, b"a shared diagram", "image/png", None, at(), None)
        .unwrap();
    demote_to_legacy(directory.path(), source, &blob.hash);

    assert!(store.repair_blob_association(target, &blob.hash).unwrap());
    assert_eq!(
        store.get_blob(target, &blob.hash).unwrap(),
        b"a shared diagram"
    );
    assert_eq!(
        objects_on_disk(directory.path()),
        vec![blob.hash.file_name()],
        "the sibling's bytes were promoted once, not copied per workspace"
    );
    assert!(legacy_file(directory.path(), source, &blob.hash).exists());
}

// ------------------------------------------------------------- moves and rows

#[test]
fn moving_documents_between_workspaces_copies_no_bytes() {
    let (directory, mut store) = store();
    let owner = store
        .create_profile("Owner", None, None, Locale::default(), at())
        .unwrap();
    let source = store.create_workspace(owner.id, "Source", at()).unwrap().id;
    let target = store.create_workspace(owner.id, "Target", at()).unwrap().id;
    let (document, _) = store
        .create_document(source, "Page", "document", None, None, at())
        .unwrap();
    let blob = store
        .put_blob(source, b"a photograph", "image/png", None, at(), None)
        .unwrap();
    let before = objects_on_disk(directory.path());
    assert_eq!(before, vec![blob.hash.file_name()]);

    store
        .transfer_documents(source, target, document, None, 0, at())
        .unwrap();

    assert_eq!(
        objects_on_disk(directory.path()),
        before,
        "a cross-workspace move writes a row; it does not duplicate the file"
    );
    assert_eq!(store.get_blob(target, &blob.hash).unwrap(), b"a photograph");
}

// ------------------------------------------------------------------- restarts

#[test]
fn a_displayed_image_survives_a_restart() {
    // The failure this pins is not theoretical: a hash-keyed decoded cache hid
    // a Workspace-scoped read miss until the next launch, so the picture was
    // there all session and gone the following morning.
    let directory = tempfile::tempdir().unwrap();
    let (workspace, hash) = {
        let mut store = durable_store(&directory);
        let profile = store
            .create_profile("Owner", None, None, Locale::default(), at())
            .unwrap();
        let workspace = store.create_workspace(profile.id, "Work", at()).unwrap().id;
        let blob = store
            .put_blob(workspace, b"a picture", "image/png", None, at(), None)
            .unwrap();
        (workspace, blob.hash)
    };

    let mut reopened = durable_store(&directory);
    assert!(reopened.workspace_has_blob(workspace, &hash).unwrap());
    assert_eq!(reopened.get_blob(workspace, &hash).unwrap(), b"a picture");
}

#[test]
fn a_legacy_image_survives_a_restart_and_promotes_once() {
    let directory = tempfile::tempdir().unwrap();
    let (workspace, hash) = {
        let mut store = durable_store(&directory);
        let profile = store
            .create_profile("Owner", None, None, Locale::default(), at())
            .unwrap();
        let workspace = store.create_workspace(profile.id, "Work", at()).unwrap().id;
        let blob = store
            .put_blob(workspace, b"an old picture", "image/png", None, at(), None)
            .unwrap();
        (workspace, blob.hash)
    };
    demote_to_legacy(directory.path(), workspace, &hash);

    let mut reopened = durable_store(&directory);
    assert_eq!(
        reopened.get_blob(workspace, &hash).unwrap(),
        b"an old picture"
    );
    assert_eq!(objects_on_disk(directory.path()), vec![hash.file_name()]);
    assert!(legacy_file(directory.path(), workspace, &hash).exists());
}

// ------------------------------------------------------------ export / import

#[test]
fn a_bundle_carries_the_image_out_of_a_legacy_layout_and_back_in() {
    let (source_dir, mut source) = store();
    let workspace = workspace(&mut source);
    let (_document, _) = source
        .create_document(workspace, "Page", "document", None, None, at())
        .unwrap();
    let blob = source
        .put_blob(workspace, b"exported pixels", "image/png", None, at(), None)
        .unwrap();
    demote_to_legacy(source_dir.path(), workspace, &blob.hash);
    assert!(objects_on_disk(source_dir.path()).is_empty());

    let bundle = source.export_workspace(workspace, at()).unwrap();
    assert!(
        bundle
            .blobs
            .iter()
            .any(|(hash, bytes)| *hash == blob.hash && bytes == b"exported pixels"),
        "the export reached the legacy layout"
    );
    assert!(
        legacy_file(source_dir.path(), workspace, &blob.hash).exists(),
        "exporting did not consume the old file"
    );

    // A second store, so this is a restore rather than a deduplicating no-op.
    let (target_dir, mut target) = store();
    let owner = target
        .create_profile("Owner", None, None, Locale::default(), at())
        .unwrap();
    let restored = target
        .import_workspace(&bundle, ImportMode::Restore, owner.id, at())
        .unwrap()
        .workspace;

    assert_eq!(
        target.get_blob(restored, &blob.hash).unwrap(),
        b"exported pixels"
    );
    assert_eq!(
        objects_on_disk(target_dir.path()),
        vec![blob.hash.file_name()],
        "restore installs the object once, then writes the association"
    );
}
