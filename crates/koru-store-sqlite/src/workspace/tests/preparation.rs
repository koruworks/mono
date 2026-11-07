// SPDX-License-Identifier: MPL-2.0

//! Actual SQLite transaction faults, entirely in named memory.
use super::*;
use crate::{SqliteBackend, SqliteCatalog};
use koru_store_core::ports::Catalog;
use koru_store_core::service::{Backend, CreateProfileWithWorkspace, FirstWorkspace, Store};
use koru_store_core::{Locale, ProfileId, WorkspaceId};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

mod pins;
mod replacement;

struct BackendState {
    backend: SqliteBackend,
    created: RefCell<Vec<WorkspaceId>>,
    deleted: RefCell<Vec<WorkspaceId>>,
    fault: Cell<Option<&'static str>>,
    cleanup_fails: bool,
}

#[derive(Clone)]
struct FaultBackend(Rc<BackendState>);

impl FaultBackend {
    fn new(fault: Option<&'static str>, cleanup_fails: bool) -> Self {
        Self(Rc::new(BackendState {
            backend: SqliteBackend::in_memory(),
            created: RefCell::new(vec![]),
            deleted: RefCell::new(vec![]),
            fault: Cell::new(fault),
            cleanup_fails,
        }))
    }
}

impl Backend for FaultBackend {
    type Workspace = SqliteWorkspace;
    type Blobs = crate::blobs::SqliteBlobs;
    fn open_workspace(&self, id: WorkspaceId) -> Result<SqliteWorkspace> {
        self.0.backend.open_workspace(id)
    }
    fn inspect_workspace_manifest(
        &self,
        id: WorkspaceId,
    ) -> koru_store_core::service::WorkspaceManifestInspection {
        self.0.backend.inspect_workspace_manifest(id)
    }
    fn create_workspace(&self, id: WorkspaceId) -> Result<SqliteWorkspace> {
        if self.0.fault.get() == Some("open") {
            return Err(Error::new(Code::StorageFailed, "preparation fault"));
        }
        let repo = self.0.backend.create_workspace(id)?;
        self.0.created.borrow_mut().push(id);
        match self.0.fault.get() {
            Some("document") => repo.connection.execute_batch(
                "CREATE TRIGGER fail BEFORE INSERT ON loro_documents BEGIN SELECT RAISE(ABORT, 'preparation fault'); END;"),
            Some("journal") => repo.connection.execute_batch(
                "CREATE TRIGGER fail BEFORE INSERT ON loro_updates BEGIN SELECT RAISE(ABORT, 'preparation fault'); END;"),
            Some("reindex") => repo.connection.execute_batch("DROP TABLE documents;"),
            _ => Ok(()),
        }.unwrap();
        Ok(repo)
    }
    fn delete_workspace(&self, id: WorkspaceId) -> Result<()> {
        self.0.deleted.borrow_mut().push(id);
        if self.0.cleanup_fails {
            return Err(Error::new(Code::StorageFailed, "cleanup fault"));
        }
        self.0.backend.delete_workspace(id)
    }
    fn objects(&self) -> Result<Self::Blobs> {
        Backend::objects(&self.0.backend)
    }
}

fn request() -> CreateProfileWithWorkspace {
    CreateProfileWithWorkspace {
        id: ProfileId::new(),
        name: "Owner".into(),
        locale: Locale::default(),
        purpose: None,
        avatar: None,
        icon: None,
        color: None,
        workspace: FirstWorkspace {
            name: "First".into(),
            icon: Some("house".into()),
            color: Some("blue".into()),
        },
    }
}

fn catalog() -> (SqliteCatalog, Connection) {
    let name = koru_sqlite::MemoryName::fresh("creation-catalog");
    let catalog = SqliteCatalog::in_named_memory(&name).unwrap();
    let control = koru_sqlite::open_memory(&name).unwrap();
    (catalog, control)
}

fn assert_unpublished(
    store: &mut Store<SqliteCatalog, FaultBackend>,
    control: &Connection,
    id: ProfileId,
) {
    assert!(store.profiles().unwrap().is_empty());
    assert!(store.workspaces_including_deleted().unwrap().is_empty());
    assert!(
        store.take_committed().is_empty(),
        "unpublished storage emitted a commit"
    );
    for table in [
        "profile_identity_crdts",
        "profile_private_crdts",
        "document_locations",
        "local_transport_modes",
    ] {
        let count: i64 = control
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "partial publication in {table}");
    }
    let count: i64 = control
        .query_row(
            "SELECT COUNT(*) FROM store_state WHERE key = ?1",
            [format!("profile_origin:{id}")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn preparation_failures_publish_nothing_and_cleanup_only_owned_storage() {
    for fault in ["open", "document", "journal", "reindex"] {
        for cleanup_fails in [false, true] {
            let backend = FaultBackend::new(Some(fault), cleanup_fails);
            let retained = WorkspaceId::new();
            let retained_repo = backend.0.backend.create_workspace(retained).unwrap();
            let (catalog, control) = catalog();
            let mut store = Store::open(catalog, backend.clone()).unwrap();
            let request = request();
            let error = store
                .create_profile_with_workspace(&request, at())
                .unwrap_err();
            assert!(!error.message().contains("cleanup fault"));
            assert_unpublished(&mut store, &control, request.id);
            assert_eq!(*backend.0.deleted.borrow(), *backend.0.created.borrow());
            assert!(!backend.0.deleted.borrow().contains(&retained));
            assert!(backend.0.backend.create_workspace(retained).is_err());
            drop(retained_repo);
        }
    }
}

#[test]
fn each_publication_boundary_rolls_back_profile_units_origin_workspace_and_location() {
    for table in [
        "profiles",
        "profile_identity_crdts",
        "profile_private_crdts",
        "store_state",
        "workspaces",
        "local_transport_modes",
        "document_locations",
    ] {
        let (catalog, control) = catalog();
        let backend = FaultBackend::new(None, false);
        let mut store = Store::open(catalog, backend.clone()).unwrap();
        control.execute_batch(&format!("CREATE TRIGGER fail BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT, 'publication fault'); END;")).unwrap();
        let request = request();
        assert!(
            store.create_profile_with_workspace(&request, at()).is_err(),
            "{table}"
        );
        assert_unpublished(&mut store, &control, request.id);
        assert_eq!(*backend.0.created.borrow(), *backend.0.deleted.borrow());
        assert_eq!(backend.0.created.borrow().len(), 1);
    }
}

#[test]
fn exact_creation_retry_is_stable_and_conflicting_or_changed_identity_refuses() {
    let (catalog, control) = catalog();
    let backend = FaultBackend::new(None, false);
    let mut store = Store::open(catalog, backend.clone()).unwrap();
    let request = request();
    let created = store.create_profile_with_workspace(&request, at()).unwrap();
    let retry = store.create_profile_with_workspace(&request, at()).unwrap();
    assert_eq!(created.profile.id, request.id);
    assert_eq!(created.workspace.id, retry.workspace.id);
    assert_eq!(backend.0.created.borrow().len(), 1);
    assert_eq!(store.take_committed().len(), 1);
    let mut conflict = request.clone();
    conflict.name = "Different".into();
    assert_eq!(
        store
            .create_profile_with_workspace(&conflict, at())
            .unwrap_err()
            .code(),
        Code::ProfileExists
    );
    conflict = request.clone();
    conflict.workspace.name = "Different".into();
    assert_eq!(
        store
            .create_profile_with_workspace(&conflict, at())
            .unwrap_err()
            .code(),
        Code::ProfileExists
    );
    control
        .execute(
            "UPDATE profiles SET name = 'Renamed' WHERE id = ?1",
            [request.id.to_storage()],
        )
        .unwrap();
    assert_eq!(
        store
            .create_profile_with_workspace(&request, at())
            .unwrap_err()
            .code(),
        Code::ProfileExists
    );
    control
        .execute(
            "UPDATE profiles SET name = 'Owner', removed_at = 42 WHERE id = ?1",
            [request.id.to_storage()],
        )
        .unwrap();
    assert_eq!(
        store
            .create_profile_with_workspace(&request, at())
            .unwrap_err()
            .code(),
        Code::ProfileExists
    );
}

#[test]
fn existing_provisioned_identity_is_not_a_creation_retry() {
    let (mut catalog, _) = catalog();
    let request = request();
    let mut profile =
        koru_store_core::Profile::new(&request.name, Locale::default(), at()).unwrap();
    profile.id = request.id;
    catalog.put_profile(&profile).unwrap();
    let mut store = Store::open(catalog, FaultBackend::new(None, false)).unwrap();
    // Legacy grandfathering is not the explicit created-here origin marker.
    assert_eq!(
        store
            .create_profile_with_workspace(&request, at())
            .unwrap_err()
            .code(),
        Code::ProfileExists
    );
}

#[test]
fn retry_refuses_a_renamed_archived_or_trashed_first_workspace() {
    for assignment in ["name = 'Renamed'", "archived = 1", "deleted_at = 42"] {
        let (catalog, control) = catalog();
        let mut store = Store::open(catalog, FaultBackend::new(None, false)).unwrap();
        let request = request();
        store.create_profile_with_workspace(&request, at()).unwrap();
        control
            .execute_batch(&format!("UPDATE workspaces SET {assignment}"))
            .unwrap();
        assert_eq!(
            store
                .create_profile_with_workspace(&request, at())
                .unwrap_err()
                .code(),
            Code::ProfileExists
        );
        assert_eq!(store.workspaces_including_deleted().unwrap().len(), 1);
    }
}

#[test]
fn combined_creation_and_retry_survive_reopening_the_store() {
    let name = koru_sqlite::MemoryName::fresh("creation-restart");
    let catalog = SqliteCatalog::in_named_memory(&name).unwrap();
    let _keeper = koru_sqlite::open_memory(&name).unwrap();
    let backend = FaultBackend::new(None, false);
    let mut store = Store::open(catalog, backend.clone()).unwrap();
    let request = request();
    let created = store.create_profile_with_workspace(&request, at()).unwrap();
    let manifest = store.manifest_id(created.workspace.id).unwrap();
    drop(store);
    let mut reopened = Store::open(
        SqliteCatalog::in_named_memory(&name).unwrap(),
        backend.clone(),
    )
    .unwrap();
    let retry = reopened
        .create_profile_with_workspace(&request, at())
        .unwrap();
    assert_eq!(retry.workspace.id, created.workspace.id);
    assert_eq!(
        reopened.manifest_id(created.workspace.id).unwrap(),
        manifest
    );
    let repo = backend.open_workspace(created.workspace.id).unwrap();
    assert!(repo.update_count(manifest).unwrap() > 0);
    assert!(matches!(
        koru_store_core::recovery::open_document(&repo, manifest).unwrap(),
        koru_store_core::ports::Opened::Normal(_)
    ));
    assert_eq!(retry.workspace.icon, request.workspace.icon);
    assert_eq!(retry.workspace.color, request.workspace.color);
    assert_eq!(backend.0.created.borrow().len(), 1);
}

#[test]
fn ordinary_workspace_creation_also_publishes_only_after_preparation() {
    let (catalog, control) = catalog();
    let backend = FaultBackend::new(None, false);
    let mut store = Store::open(catalog, backend.clone()).unwrap();
    let profile = store
        .create_profile("Owner", None, None, Locale::default(), at())
        .unwrap();
    control.execute_batch("CREATE TRIGGER fail BEFORE INSERT ON document_locations BEGIN SELECT RAISE(ABORT, 'publication fault'); END;").unwrap();
    assert!(store.create_workspace(profile.id, "First", at()).is_err());
    assert!(store.workspaces_including_deleted().unwrap().is_empty());
    assert!(store.take_committed().is_empty());
    assert_eq!(*backend.0.created.borrow(), *backend.0.deleted.borrow());
}
