// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{SqliteBackend, SqliteCatalog};
use koru_store_core::{
    Locale, ProfileId, WorkspaceId,
    ids::RemovalAttemptId,
    profile_removal::{RemovalKind, RemovalMark, removal_key},
    service::{
        Backend, EnsuredWorkspace, Store, WorkspaceAuthorityState, WorkspaceManifestInspection,
    },
};
use std::rc::Rc;

#[derive(Clone)]
struct Memory(Rc<SqliteBackend>);
impl Backend for Memory {
    type Workspace = SqliteWorkspace;
    type Blobs = crate::SqliteBlobs;
    fn open_workspace(&self, id: WorkspaceId) -> Result<Self::Workspace> {
        self.0.open_workspace(id)
    }
    fn create_workspace(&self, id: WorkspaceId) -> Result<Self::Workspace> {
        self.0.create_workspace(id)
    }
    fn inspect_workspace_manifest(&self, id: WorkspaceId) -> WorkspaceManifestInspection {
        self.0.inspect_workspace_manifest(id)
    }
    fn prepare_workspace_manifest_inspection(
        &self,
        id: WorkspaceId,
    ) -> Box<dyn FnOnce() -> WorkspaceManifestInspection + Send> {
        self.0.prepare_workspace_manifest_inspection(id)
    }
    fn delete_workspace(&self, id: WorkspaceId) -> Result<()> {
        self.0.delete_workspace(id)
    }
    fn objects(&self) -> Result<Self::Blobs> {
        Backend::objects(self.0.as_ref())
    }
}

struct Fixture {
    store: Store<SqliteCatalog, Memory>,
    backend: Memory,
    catalog: koru_sqlite::MemoryName,
    control: Connection,
    owner: ProfileId,
}
impl Fixture {
    fn new() -> Self {
        let backend = Memory(Rc::new(SqliteBackend::in_memory()));
        let catalog = koru_sqlite::MemoryName::fresh("availability");
        let control = koru_sqlite::open_memory(&catalog).unwrap();
        let mut store = Store::open(
            SqliteCatalog::in_named_memory(&catalog).unwrap(),
            backend.clone(),
        )
        .unwrap();
        let owner = store
            .create_profile("Owner", None, None, Locale::default(), at())
            .unwrap()
            .id;
        Self {
            store,
            backend,
            catalog,
            control,
            owner,
        }
    }
    fn create(&mut self) -> WorkspaceId {
        self.store
            .create_workspace(self.owner, "Existing", at())
            .unwrap()
            .id
    }
    fn select(&mut self) -> EnsuredWorkspace {
        self.store
            .ensure_live_workspace(self.owner, "Default", at())
            .unwrap()
    }
    fn reopen(&mut self) {
        self.store = Store::open(
            SqliteCatalog::in_named_memory(&self.catalog).unwrap(),
            self.backend.clone(),
        )
        .unwrap();
    }
    fn listed(&self) -> usize {
        self.store.workspaces_including_deleted().unwrap().len()
    }
}

/// **Nothing is manufactured here any more** (root-catalog Plan 1.3,
/// TECHNICAL.final §3.10, §6.3): an empty Profile is a valid state that
/// answers [`EnsuredWorkspace::Empty`] on open and after a restart alike,
/// and a Workspace the person creates explicitly is then the selection.
#[test]
fn an_empty_profile_stays_empty_and_an_explicit_workspace_is_selected_across_a_reopen() {
    let mut f = Fixture::new();
    assert!(matches!(f.select(), EnsuredWorkspace::Empty));
    f.reopen();
    assert!(matches!(f.select(), EnsuredWorkspace::Empty));
    assert_eq!(f.listed(), 0, "selection created a workspace");
    let workspace = f.create();
    assert!(
        matches!(f.select(), EnsuredWorkspace::Active { workspace: found, .. } if found.id == workspace)
    );
    f.reopen();
    assert!(
        matches!(f.select(), EnsuredWorkspace::Active { workspace: found, .. } if found.id == workspace)
    );
    assert_eq!(f.listed(), 1);
}

#[test]
fn healthy_content_wins_over_an_unreadable_neighbor_but_uncertainty_alone_never_creates() {
    let mut f = Fixture::new();
    let damaged = f.create();
    f.backend.delete_workspace(damaged).unwrap();
    for cold in [false, true] {
        if cold {
            f.reopen();
        }
        assert!(
            matches!(f.select(), EnsuredWorkspace::Uncertain { issues } if issues[0].workspace == damaged)
        );
        assert_eq!(f.listed(), 1);
    }
    let healthy = f.create();
    let EnsuredWorkspace::Active { workspace, issues } = f.select() else {
        panic!("healthy workspace must remain usable")
    };
    assert_eq!(workspace.id, healthy);
    assert_eq!(issues[0].workspace, damaged);
    assert_eq!(f.listed(), 2);
}

#[test]
fn unapplied_and_recovered_rows_are_never_empty_profiles() {
    let mut f = Fixture::new();
    let workspace = f.create();
    let repo = f.backend.open_workspace(workspace).unwrap();
    repo.connection
        .execute("DELETE FROM loro_updates", [])
        .unwrap();
    assert!(
        matches!(f.select(), EnsuredWorkspace::Pending { workspaces } if workspaces == vec![workspace])
    );
    assert_eq!(f.listed(), 1);
    drop(repo);
    let mut g = Fixture::new();
    let recovered = g.create();
    let repo = g.backend.open_workspace(recovered).unwrap();
    repo.connection
        .execute("UPDATE loro_updates SET update_bytes = X'00'", [])
        .unwrap();
    let EnsuredWorkspace::Uncertain { issues } = g.select() else {
        panic!("recovery must stay visible")
    };
    assert!(issues[0].recovery.is_some());
    assert_eq!(g.listed(), 1);
}

#[test]
fn ordinary_trash_needs_no_location_and_stale_live_locations_are_repaired() {
    let mut f = Fixture::new();
    let trashed = f.create();
    f.store.delete_workspace(trashed, at()).unwrap();
    let authority = f.store.workspace_authority(f.owner).unwrap();
    assert!(!authority[0].listing_differs);
    assert!(matches!(
        authority[0].authority,
        WorkspaceAuthorityState::Read {
            deleted_at: Some(_),
            applied: true,
            ..
        }
    ));
    // Verified Trash is simply not a live Workspace, and no default is
    // manufactured for the emptied Profile (root-catalog Plan 1.3).
    assert!(matches!(f.select(), EnsuredWorkspace::Empty));
    let workspace = f.store.create_workspace(f.owner, "Live", at()).unwrap();
    f.control
        .execute("DELETE FROM document_locations", [])
        .unwrap();
    assert!(
        f.store
            .workspace_authority(f.owner)
            .unwrap()
            .iter()
            .any(|w| w.workspace.id == workspace.id && w.listing_differs)
    );
    // A pure authority read has left the missing live route missing.
    let locations: i64 = f
        .control
        .query_row("SELECT COUNT(*) FROM document_locations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(locations, 0);
    assert!(
        matches!(f.select(), EnsuredWorkspace::Active { workspace: found, .. } if found.id == workspace.id)
    );
    assert!(
        f.store
            .workspace_authority(f.owner)
            .unwrap()
            .iter()
            .all(|w| !w.listing_differs)
    );
    assert_eq!(f.listed(), 2);
}

#[test]
fn failed_live_routing_repair_is_listing_pending_and_rolls_back_projection() {
    let mut f = Fixture::new();
    let workspace = f.create();
    f.control
        .execute("DELETE FROM document_locations", [])
        .unwrap();
    f.control
        .execute("UPDATE workspaces SET name = 'Stale'", [])
        .unwrap();
    f.control.execute_batch("CREATE TRIGGER deny BEFORE INSERT ON document_locations BEGIN SELECT RAISE(ABORT, 'routing denied'); END;").unwrap();
    for cold in [false, true] {
        if cold {
            f.reopen();
        }
        assert!(
            matches!(f.select(), EnsuredWorkspace::ListingPending { issues } if issues[0].workspace == workspace)
        );
        assert_eq!(f.store.workspace(workspace).unwrap().name, "Stale");
        assert_eq!(f.listed(), 1);
    }
    f.control.execute_batch("DROP TRIGGER deny").unwrap();
    assert!(
        matches!(f.select(), EnsuredWorkspace::Active { workspace: found, .. } if found.name == "Existing")
    );
}

#[test]
fn removal_mark_wins_over_live_pending_and_unreadable_content_even_after_expiry() {
    let mut f = Fixture::new();
    f.create();
    let missing = f.create();
    f.backend.delete_workspace(missing).unwrap();
    let mark = RemovalMark {
        attempt: RemovalAttemptId::new(),
        kind: RemovalKind::Local,
        started_at: Timestamp::from_millis(0),
        last_step_at: Timestamp::from_millis(0),
        stopped: false,
    };
    f.control
        .execute(
            "INSERT INTO store_state(key, value) VALUES (?1, ?2)",
            params![removal_key(f.owner), serde_json::to_string(&mark).unwrap()],
        )
        .unwrap();
    assert!(!mark.lease_live(at()));
    assert!(matches!(f.select(), EnsuredWorkspace::Removing { mark: found } if found == mark));
    f.reopen();
    assert!(matches!(f.select(), EnsuredWorkspace::Removing { mark: found } if found == mark));
    assert_eq!(f.listed(), 2);
}

#[test]
fn prerequisite_flush_failure_refuses_selection_without_replacement_or_projection() {
    let mut f = Fixture::new();
    let workspace = f.create();
    let document = f.store.manifest_id(workspace).unwrap();
    let manifest =
        koru_store_core::Manifest::from_snapshot(&f.store.snapshot(workspace, document).unwrap())
            .unwrap();
    let version = manifest.version();
    manifest.set_name("Incoming").unwrap();
    f.store
        .apply_update(
            workspace,
            document,
            manifest.export_updates_since(&version).unwrap(),
            at(),
            UpdateOrigin::Store,
        )
        .unwrap();
    let repo = f.backend.open_workspace(workspace).unwrap();
    repo.connection.execute_batch("CREATE TRIGGER deny BEFORE INSERT ON loro_updates BEGIN SELECT RAISE(ABORT, 'flush denied'); END;").unwrap();
    assert!(
        f.store
            .ensure_live_workspace(f.owner, "Default", at())
            .is_err()
    );
    assert_eq!(f.store.workspace(workspace).unwrap().name, "Existing");
    assert_eq!(f.listed(), 1);
    repo.connection.execute_batch("DROP TRIGGER deny").unwrap();
    assert!(
        matches!(f.select(), EnsuredWorkspace::Active { workspace, .. } if workspace.name == "Incoming")
    );
}

#[test]
fn verified_live_authority_repairs_stale_trash_and_archive_without_a_default() {
    let mut f = Fixture::new();
    let workspace = f.create();
    f.control
        .execute("UPDATE workspaces SET deleted_at = 1, archived = 1", [])
        .unwrap();
    f.control
        .execute("DELETE FROM document_locations", [])
        .unwrap();
    f.reopen();
    let overlay = f.store.workspace_authority(f.owner).unwrap();
    assert!(overlay[0].listing_differs);
    assert!(matches!(
        overlay[0].authority,
        WorkspaceAuthorityState::Read {
            applied: true,
            archived: false,
            deleted_at: None,
            ..
        }
    ));
    let EnsuredWorkspace::Active {
        workspace: found, ..
    } = f.select()
    else {
        panic!("durable live authority is restored")
    };
    assert_eq!(found.id, workspace);
    assert!(!found.archived);
    assert!(found.deleted_at.is_none());
    assert_eq!(f.listed(), 1);
    assert!(!f.store.workspace_authority(f.owner).unwrap()[0].listing_differs);
}

/// The prerequisite flush makes a queued manifest deletion durable in the
/// middle of selection. Since catalog 13 the root unit is the only lifecycle
/// authority, so that durable manifest value is a **hint** and changes
/// nothing — the authority rule itself is pinned in
/// `preparation::lifecycle::an_incoming_manifest_deletion_is_a_hint_that_cannot_trash_a_unit_holding_workspace`.
/// Selection continues over the flush, and the root deletion that does trash
/// the only Workspace leaves the Profile empty instead of manufacturing a
/// successor.
#[test]
fn a_flushed_manifest_deletion_hint_neither_trashes_the_row_nor_produces_a_successor() {
    let mut f = Fixture::new();
    let workspace = f.create();
    let document = f.store.manifest_id(workspace).unwrap();
    let manifest =
        koru_store_core::Manifest::from_snapshot(&f.store.snapshot(workspace, document).unwrap())
            .unwrap();
    let version = manifest.version();
    manifest.set_workspace_deleted_at(Some(at())).unwrap();
    f.store
        .apply_update(
            workspace,
            document,
            manifest.export_updates_since(&version).unwrap(),
            at(),
            UpdateOrigin::Store,
        )
        .unwrap();
    let before = f.store.workspace_authority(f.owner).unwrap();
    assert!(
        matches!(
            before[0].authority,
            WorkspaceAuthorityState::Read {
                applied: true,
                deleted_at: None,
                ..
            }
        ),
        "queued deletion is not durable authority"
    );
    assert!(
        matches!(f.select(), EnsuredWorkspace::Active { workspace: found, .. } if found.id == workspace)
    );
    assert!(
        f.store
            .workspace_including_deleted(workspace)
            .unwrap()
            .deleted_at
            .is_none(),
        "the manifest hint trashed the row"
    );
    assert_eq!(f.listed(), 1);

    // The root fact is what trashes it, and the emptied Profile stays empty.
    f.store.delete_workspace(workspace, at()).unwrap();
    assert!(matches!(f.select(), EnsuredWorkspace::Empty));
    assert!(
        f.store
            .workspace_including_deleted(workspace)
            .unwrap()
            .deleted_at
            .is_some()
    );
    assert_eq!(f.listed(), 1);
}

#[test]
fn detached_authority_read_allows_another_workspace_write_before_decode() {
    let mut f = Fixture::new();
    let id = f.create();
    let store = std::sync::Mutex::new(f.store);
    let read = {
        store
            .lock()
            .unwrap()
            .prepare_workspace_authority(f.owner, id)
            .unwrap()
    };
    let (resume, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        wait.recv().unwrap();
        read.finish().unwrap()
    });
    // Keep the captured reader alive while a different workspace commits.
    // This uses only named in-memory databases, with no Store server or disk.
    let other = store
        .try_lock()
        .unwrap()
        .create_workspace(f.owner, "Other", at())
        .unwrap();
    assert_ne!(other.id, id);
    resume.send(()).unwrap();
    let row = worker.join().unwrap();
    assert_eq!(row.workspace.id, id);
    assert!(!row.listing_differs);
    assert!(matches!(
        row.authority,
        WorkspaceAuthorityState::Read { applied: true, .. }
    ));
}

#[test]
fn authority_capture_rechecks_owner_after_enumeration() {
    let mut f = Fixture::new();
    let id = f.create();
    let ids = f.store.workspace_authority_ids(f.owner).unwrap();
    assert_eq!(ids, vec![id]);
    let other = f
        .store
        .create_profile("Other", None, None, Locale::default(), at())
        .unwrap();
    f.store.move_workspace(id, other.id).unwrap();
    assert!(
        matches!(f.store.prepare_workspace_authority(f.owner, id), Err(error)
        if error.code() == koru_store_core::Code::WorkspaceNotFound)
    );
    let row = f
        .store
        .prepare_workspace_authority(other.id, id)
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(row.workspace.owner, other.id);
}
