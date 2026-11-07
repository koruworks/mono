// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_core::model::{DocumentKind, Timestamp};
use koru_store_core::ports::WorkspaceRepo;

fn at() -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000)
}

#[test]
fn a_workspaces_paths_are_all_derived_from_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let backend = SqliteBackend::new(dir.path());
    let id = WorkspaceId::new();

    let shown = backend.workspace_dir(id).to_string_lossy().to_string();
    assert!(shown.contains(&id.to_storage()), "{shown}");
    assert!(
        backend
            .workspace_db(id)
            .starts_with(backend.workspace_dir(id))
    );
    assert!(
        backend
            .legacy_blobs_dir(id)
            .starts_with(backend.workspace_dir(id))
    );
}

#[test]
fn the_object_store_is_one_directory_for_the_whole_root() {
    // The change B1 makes, stated as a path: a picture in two Workspaces is
    // one file, and nothing about where it lives names a Workspace.
    let dir = tempfile::tempdir().unwrap();
    let backend = SqliteBackend::new(dir.path());
    let (a, b) = (WorkspaceId::new(), WorkspaceId::new());

    assert_eq!(backend.objects_dir(), dir.path().join("blobs"));
    assert!(!backend.objects_dir().starts_with(backend.workspace_dir(a)));
    assert!(!backend.objects_dir().starts_with(backend.workspace_dir(b)));
    // And the legacy directories remain per Workspace, which is why promotion
    // has to be asked per Workspace rather than swept.
    assert_ne!(backend.legacy_blobs_dir(a), backend.legacy_blobs_dir(b));
}

#[test]
fn a_path_component_is_ascii_whatever_the_workspace_is_called() {
    // Names are translatable, renameable, and full of characters that are legal
    // on one filesystem and not another. Ids are 36 ASCII characters.
    let dir = tempfile::tempdir().unwrap();
    let backend = SqliteBackend::new(dir.path());
    let id = WorkspaceId::new();
    let component = backend
        .workspace_dir(id)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(component.is_ascii(), "{component}");
    assert_eq!(component, id.to_storage());
}

#[test]
fn opening_a_workspace_that_has_never_existed_creates_it() {
    let dir = tempfile::tempdir().unwrap();
    let backend = SqliteBackend::new(dir.path());
    let id = WorkspaceId::new();

    let mut repo = backend.open_workspace(id).unwrap();
    assert!(backend.workspace_db(id).exists());
    assert!(repo.documents().unwrap().is_empty());

    // And it is a real workspace database, not an empty file.
    let document = koru_store_core::ids::DocumentId::new();
    repo.create_document(document, DocumentKind::Manifest, at())
        .unwrap();
    assert!(repo.document(document).unwrap().is_some());
}

#[test]
fn reopening_a_workspace_finds_what_was_there() {
    let dir = tempfile::tempdir().unwrap();
    let backend = SqliteBackend::new(dir.path());
    let id = WorkspaceId::new();
    let document = koru_store_core::ids::DocumentId::new();
    {
        let mut repo = backend.open_workspace(id).unwrap();
        repo.create_document(document, DocumentKind::Manifest, at())
            .unwrap();
    }
    let repo = backend.open_workspace(id).unwrap();
    assert!(repo.document(document).unwrap().is_some());
}

#[test]
fn two_workspaces_do_not_share_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let backend = SqliteBackend::new(dir.path());
    let (a, b) = (WorkspaceId::new(), WorkspaceId::new());
    assert_ne!(backend.workspace_dir(a), backend.workspace_dir(b));
}

#[test]
fn the_same_workspace_id_under_two_roots_is_two_replicas() {
    // What makes a workspace able to exist in stable and nightly at once: same
    // id, different root, and nothing in the path says which channel it is.
    let one = tempfile::tempdir().unwrap();
    let two = tempfile::tempdir().unwrap();
    let id = WorkspaceId::new();
    let a = SqliteBackend::new(one.path());
    let b = SqliteBackend::new(two.path());

    assert_ne!(a.workspace_db(id), b.workspace_db(id));
    assert_eq!(
        a.workspace_dir(id).file_name(),
        b.workspace_dir(id).file_name(),
        "the id is the same on both sides"
    );
}

#[test]
fn a_memory_workspace_is_the_same_database_every_time_it_is_opened() {
    let backend = SqliteBackend::in_memory();
    let id = WorkspaceId::new();
    let document = koru_store_core::ids::DocumentId::new();
    {
        let mut repo = backend.open_workspace(id).unwrap();
        repo.create_document(document, DocumentKind::Manifest, at())
            .unwrap();
    }
    let repo = backend.open_workspace(id).unwrap();
    assert!(repo.document(document).unwrap().is_some());
    // Another Workspace is another database.
    let other = backend.open_workspace(WorkspaceId::new()).unwrap();
    assert!(other.document(document).unwrap().is_none());
}

#[test]
fn deleting_a_memory_workspace_takes_its_database_with_it() {
    let backend = SqliteBackend::in_memory();
    let id = WorkspaceId::new();
    let document = koru_store_core::ids::DocumentId::new();
    backend
        .open_workspace(id)
        .unwrap()
        .create_document(document, DocumentKind::Manifest, at())
        .unwrap();
    backend.delete_workspace(id).unwrap();
    let reopened = backend.open_workspace(id).unwrap();
    assert!(reopened.document(document).unwrap().is_none());
}

#[test]
fn a_memory_backend_keeps_no_file_bytes_and_says_so() {
    let backend = SqliteBackend::in_memory();
    let refused = Backend::objects(&backend).err().expect("no blob store");
    assert_eq!(
        refused.code(),
        koru_store_core::Code::BlobTransferUnavailable
    );
    assert_eq!(
        backend.sweep_pending_objects(at()).unwrap(),
        crate::blobs::PendingSweep::default()
    );
    assert!(matches!(
        backend
            .promote_legacy_object(
                WorkspaceId::new(),
                &BlobHash::from_digest("b3", &"a".repeat(64)).unwrap()
            )
            .unwrap(),
        LegacyPromotion::NoCandidate
    ));
}

/// A whole Store over the in-memory catalog and backend: a Workspace, a
/// document, an edit, a second open of the Workspace, and nothing written
/// anywhere but memory.
#[test]
fn a_store_in_memory_creates_edits_and_reads_back() {
    use koru_store_core::service::Store;
    let catalog =
        crate::SqliteCatalog::in_named_memory(&koru_sqlite::MemoryName::fresh("catalog.db"))
            .unwrap();
    let mut store = Store::open(catalog, SqliteBackend::in_memory()).unwrap();
    let profile = store
        .create_profile(
            "Person",
            None,
            None,
            koru_store_core::Locale::parse("en").unwrap(),
            at(),
        )
        .unwrap();
    let workspace = store.create_workspace(profile.id, "Work", at()).unwrap();
    let (document, _) = store
        .create_document(workspace.id, "Plan", "page", None, None, at())
        .unwrap();
    let doc = loro::LoroDoc::new();
    doc.import(&store.snapshot(workspace.id, document).unwrap())
        .unwrap();
    doc.get_map("probe").insert("k", "v").unwrap();
    doc.commit();
    store
        .apply_update(
            workspace.id,
            document,
            doc.export(loro::ExportMode::all_updates()).unwrap(),
            at(),
            koru_store_core::ports::UpdateOrigin::Store,
        )
        .unwrap();
    store.flush(workspace.id, document, at()).unwrap();
    let titles: Vec<_> = store
        .documents(workspace.id)
        .unwrap()
        .into_iter()
        .map(|entry| entry.title)
        .collect();
    assert!(titles.contains(&"Plan".to_owned()), "{titles:?}");
    let back = loro::LoroDoc::new();
    back.import(&store.snapshot(workspace.id, document).unwrap())
        .unwrap();
    assert_eq!(
        back.get_map("probe")
            .get("k")
            .unwrap()
            .into_value()
            .unwrap(),
        loro::LoroValue::from("v")
    );
}
