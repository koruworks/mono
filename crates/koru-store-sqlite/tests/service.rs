// SPDX-License-Identifier: MPL-2.0

//! **The store's operations, against real storage.**
//!
//! `koru-store-core`'s `service.rs` has no tests beside it because every
//! operation needs a `Backend`, and the only honest one is this crate's. A fake
//! would prove the service agrees with a fake.

use koru_store_core::errors::Code;
use koru_store_core::fixtures::{BIDI_LINE, SAMPLES};
use koru_store_core::ids::{DocumentId, HistoryEntryId, ProfileId, WorkspaceId};
use koru_store_core::lifecycle::LiveDocument;
use koru_store_core::manifest::Manifest;
use koru_store_core::model::{Profile, Timestamp};
use koru_store_core::ports::{
    Catalog, HistoryEntry, HistoryReason, LocalProfileRemoval, LocalProfileRestoration,
    SeedDocument, WorkspaceRepo,
};
use koru_store_core::service::{Backend, Store};
use koru_store_core::text::Locale;
use koru_store_core::{
    ProfileAccountBinding, ProfileAccountBindingOutcome, ProfileAccountClearOutcome,
};
use koru_store_sqlite::{SqliteBackend, SqliteCatalog, SqliteWorkspace};

type TestStore = Store<SqliteCatalog, SqliteBackend>;

fn at(ms: i64) -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000 + ms)
}

struct Fixture {
    dir: tempfile::TempDir,
    store: TestStore,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self {
            store: Self::open_in(dir.path()),
            dir,
        }
    }

    fn open_in(root: &std::path::Path) -> TestStore {
        let catalog = SqliteCatalog::open(&root.join("catalog.db")).unwrap();
        Store::open(catalog, SqliteBackend::new(root)).unwrap()
    }

    /// Close and reopen, as a restart would.
    fn restart(&mut self) {
        self.store = Self::open_in(self.dir.path());
    }

    fn a_workspace(&mut self) -> WorkspaceId {
        let profile = self
            .store
            .create_profile("Owner", None, None, Locale::parse("en").unwrap(), at(0))
            .unwrap();
        self.store
            .create_workspace(profile.id, "Workspace", at(0))
            .unwrap()
            .id
    }
}

/// A client's own replica of a document, as one would really have.
struct Client {
    manifest: Manifest,
    /// What it has already sent.
    sent: loro::VersionVector,
}

impl Client {
    /// Start from the store's snapshot, the way a joining client does.
    fn joining(store: &mut TestStore, ws: WorkspaceId, doc: DocumentId) -> Self {
        let snapshot = store.snapshot(ws, doc).unwrap();
        let manifest = Manifest::from_snapshot(&snapshot).unwrap();
        let sent = manifest.version();
        Self { manifest, sent }
    }

    /// Make a change and produce the update to send.
    fn add(&mut self, title: &str) -> Vec<u8> {
        self.manifest
            .add_document(DocumentId::new(), title, "page", None)
            .unwrap();
        let update = self.manifest.export_updates_since(&self.sent).unwrap();
        self.sent = self.manifest.version();
        update
    }

    fn rename(&mut self, name: &str) -> Vec<u8> {
        self.manifest.set_name(name).unwrap();
        let update = self.manifest.export_updates_since(&self.sent).unwrap();
        self.sent = self.manifest.version();
        update
    }
}

fn titles(store: &mut TestStore, ws: WorkspaceId) -> Vec<String> {
    store
        .documents(ws)
        .unwrap()
        .into_iter()
        .map(|e| e.title)
        .collect()
}

#[test]
fn an_imported_document_is_born_trashed_at_the_source_time() {
    let mut fixture = Fixture::new();
    let workspace = fixture.a_workspace();
    let document = DocumentId::new();
    let source_time = at(44);
    let doc = loro::LoroDoc::new();
    doc.get_text("body").insert(0, "deleted note").unwrap();
    doc.commit();
    let snapshot = doc.export(loro::ExportMode::Snapshot).unwrap();

    let (created, delta) = fixture
        .store
        .import_document_at_with_mime_and_trash(
            workspace,
            document,
            "Deleted note",
            "page",
            None,
            None,
            None,
            Some(source_time),
            snapshot,
            at(50),
        )
        .unwrap();

    assert_eq!(created, document);
    assert!(
        !delta.is_empty(),
        "creation returns one manifest publication"
    );
    assert_eq!(
        fixture
            .store
            .documents(workspace)
            .unwrap()
            .into_iter()
            .find(|entry| entry.id == document)
            .unwrap()
            .trashed_at,
        Some(source_time)
    );
    fixture.restart();
    assert_eq!(
        fixture
            .store
            .documents(workspace)
            .unwrap()
            .into_iter()
            .find(|entry| entry.id == document)
            .unwrap()
            .trashed_at,
        Some(source_time)
    );
}

#[test]
fn replica_provisioning_preserves_ids_and_accepts_each_snapshot_durably() {
    let mut source = Fixture::new();
    let workspace = source.a_workspace();
    let (document, _) = source
        .store
        .create_document(workspace, "Roadmap", "page", None, None, at(1))
        .unwrap();
    let live = loro::LoroDoc::new();
    live.import(&source.store.snapshot(workspace, document).unwrap())
        .unwrap();
    live.get_text("t").insert(0, "bootstrap payload").unwrap();
    live.commit();
    source
        .store
        .apply_update(
            workspace,
            document,
            live.export(loro::ExportMode::all_updates()).unwrap(),
            at(1),
            None,
        )
        .unwrap();
    source.store.flush(workspace, document, at(1)).unwrap();
    let workspace_record = source.store.workspace(workspace).unwrap();
    let profile = source.store.profile(workspace_record.owner).unwrap();
    let manifest = source.store.manifest_id(workspace).unwrap();
    let manifest_snapshot = source.store.snapshot(workspace, manifest).unwrap();
    let document_snapshot = source.store.snapshot(workspace, document).unwrap();

    let mut target = Fixture::new();
    target
        .store
        .provision_replica(&profile, &workspace_record, manifest, at(2))
        .unwrap();
    // A retry after a crash is a no-op, not a second workspace/manifest.
    target
        .store
        .provision_replica(&profile, &workspace_record, manifest, at(2))
        .unwrap();
    target
        .store
        .apply_update(workspace, manifest, manifest_snapshot, at(3), None)
        .unwrap();
    assert!(
        target
            .store
            .flush(workspace, manifest, at(3))
            .unwrap()
            .is_some()
    );
    // Restart halfway through bootstrap. Inventory/version negotiation must be
    // able to resume from the already durable manifest rather than starting a
    // second replica.
    target.restart();
    target
        .store
        .provision_replica(&profile, &workspace_record, manifest, at(4))
        .unwrap();
    target
        .store
        .provision_replica_document(workspace, document, at(4))
        .unwrap();
    target
        .store
        .provision_replica_document(workspace, document, at(4))
        .unwrap();
    target
        .store
        .apply_update(workspace, document, document_snapshot, at(5), None)
        .unwrap();
    assert!(
        target
            .store
            .flush(workspace, document, at(5))
            .unwrap()
            .is_some()
    );

    assert_eq!(target.store.manifest_id(workspace).unwrap(), manifest);
    assert_eq!(target.store.documents(workspace).unwrap()[0].id, document);
    assert_eq!(target.store.locate(document).unwrap(), Some(workspace));
    assert_eq!(
        target.store.version(workspace, document).unwrap(),
        source.store.version(workspace, document).unwrap()
    );
}

#[test]
fn replica_provisioning_repairs_a_missing_location_for_existing_bytes() {
    let mut f = Fixture::new();
    let workspace = f.a_workspace();
    let document = DocumentId::new();
    f.store
        .provision_replica_document(workspace, document, at(1))
        .unwrap();

    // The catalog row is derived and may be absent after a crash or an older
    // repair bug even though the authoritative workspace database is intact.
    let mut catalog = SqliteCatalog::open(&f.dir.path().join("catalog.db")).unwrap();
    catalog.forget_location(document).unwrap();
    assert_eq!(f.store.locate(document).unwrap(), None);

    f.store
        .provision_replica_document(workspace, document, at(2))
        .expect("an existing ordinary document is an idempotent retry");
    assert_eq!(f.store.locate(document).unwrap(), Some(workspace));
}

#[test]
fn replica_provisioning_repairs_a_location_that_points_at_nothing() {
    // The phantom row behind "exists on one device, NotFound on another":
    // the catalog claims the document is here, the workspace database has
    // never heard of it. Honoring the row failed every sync pass with
    // `document_not_found` forever; the projection must lose to reality.
    let mut f = Fixture::new();
    let workspace = f.a_workspace();
    let document = DocumentId::new();
    {
        let mut catalog = SqliteCatalog::open(&f.dir.path().join("catalog.db")).unwrap();
        catalog.record_location(document, workspace).unwrap();
    }

    f.store
        .provision_replica_document(workspace, document, at(1))
        .expect("a location pointing at nothing repairs instead of failing");
    assert_eq!(f.store.locate(document).unwrap(), Some(workspace));
    // The empty replica physically exists now, so a peer's updates have
    // somewhere to land.
    assert!(f.store.version(workspace, document).is_ok());
}

#[test]
fn replica_provisioning_repairs_a_phantom_location_one_workspace_away() {
    // Same phantom, recorded under a *different* workspace: the owner check
    // must not take the catalog's word when the owner's own database has no
    // such document.
    let mut f = Fixture::new();
    let workspace = f.a_workspace();
    let other = f.a_workspace();
    let document = DocumentId::new();
    {
        let mut catalog = SqliteCatalog::open(&f.dir.path().join("catalog.db")).unwrap();
        catalog.record_location(document, other).unwrap();
    }

    f.store
        .provision_replica_document(workspace, document, at(1))
        .expect("a phantom owner repairs instead of failing");
    assert_eq!(f.store.locate(document).unwrap(), Some(workspace));
}

#[test]
fn locating_a_document_repairs_a_location_whose_recorded_owner_disclaims_it() {
    let mut f = Fixture::new();
    let workspace = f.a_workspace();
    let other = f.a_workspace();
    let (document, _) = f
        .store
        .create_document(workspace, "Roadmap", "page", None, None, at(1))
        .unwrap();
    // Corrupt the projection: recorded under a workspace whose manifest has
    // never heard of it.
    {
        let mut catalog = SqliteCatalog::open(&f.dir.path().join("catalog.db")).unwrap();
        catalog.record_location(document, other).unwrap();
    }
    // Routing checks the indexed owner against its manifest and physical
    // document row before returning it. The stale row is then rebuilt from
    // the real owner's manifest, without requiring somebody to open that
    // workspace first.
    assert_eq!(f.store.locate(document).unwrap(), Some(workspace));
}

#[test]
fn a_recorded_owner_keeps_its_claim_when_its_manifest_lists_the_document() {
    // The cross-workspace move in flight the stale-owner guard exists for:
    // two manifests name the document, so the projection must not be
    // "repaired" away from the recorded owner.
    let mut f = Fixture::new();
    let workspace = f.a_workspace();
    let other = f.a_workspace();
    let (document, _) = f
        .store
        .create_document(workspace, "Roadmap", "page", None, None, at(1))
        .unwrap();
    // Add the same document to the other workspace's manifest, as the first
    // half of a move would publish it.
    let manifest_id = f.store.manifest_id(other).unwrap();
    let snapshot = f.store.snapshot(other, manifest_id).unwrap();
    let mut manifest = Manifest::from_snapshot(&snapshot).unwrap();
    let before = manifest.version();
    manifest
        .add_document_at(document, "Roadmap", "page", None, None)
        .unwrap();
    let update = manifest.export_updates_since(&before).unwrap();
    f.store
        .apply_update(other, manifest_id, update, at(2), None)
        .unwrap();
    f.store.flush(other, manifest_id, at(2)).unwrap();
    {
        let mut catalog = SqliteCatalog::open(&f.dir.path().join("catalog.db")).unwrap();
        catalog.record_location(document, other).unwrap();
    }

    f.restart();
    f.store.documents(workspace).unwrap();
    assert_eq!(
        f.store.locate(document).unwrap(),
        Some(other),
        "a move in flight keeps the recorded owner"
    );
}

#[test]
fn workspace_appearance_replicates_through_the_manifest() {
    // Icon, color, pinned and archived are shared workspace metadata: they
    // live in the manifest CRDT, and the catalog row on every replica is
    // only their local projection. A gap here is how a phone ends up
    // showing one profile's workspaces plain while another's are styled.
    let mut source = Fixture::new();
    let workspace = source.a_workspace();
    source
        .store
        .set_workspace_appearance(workspace, Some("🌴"), Some("purple"), true, false)
        .unwrap();
    let workspace_record = source.store.workspace(workspace).unwrap();
    let profile = source.store.profile(workspace_record.owner).unwrap();
    let manifest = source.store.manifest_id(workspace).unwrap();
    let manifest_snapshot = source.store.snapshot(workspace, manifest).unwrap();

    let mut target = Fixture::new();
    target
        .store
        .provision_replica(&profile, &workspace_record, manifest, at(2))
        .unwrap();
    target
        .store
        .apply_update(workspace, manifest, manifest_snapshot, at(3), None)
        .unwrap();
    target.store.flush(workspace, manifest, at(3)).unwrap();

    let replica = target.store.workspace(workspace).unwrap();
    assert_eq!(replica.icon.as_deref(), Some("🌴"));
    assert_eq!(replica.color.as_deref(), Some("purple"));
    assert!(replica.pinned);
    assert!(!replica.archived);
}

#[test]
fn opening_a_workspace_rebuilds_a_stale_document_index() {
    // The entries index is a disposable projection of the manifest, but only
    // manifest commits rebuilt it: a crash or an old-version bug could leave
    // it empty while the manifest — and sync, which compares version vectors,
    // not the listing — said everything was fine. A workspace that showed
    // zero documents forever with sync reporting complete. Opening the
    // workspace now rebuilds the index from the manifest.
    let mut f = Fixture::new();
    let workspace = f.a_workspace();
    let (document, _) = f
        .store
        .create_document(workspace, "Roadmap", "page", None, None, at(1))
        .unwrap();
    assert_eq!(f.store.documents(workspace).unwrap().len(), 1);

    // Wipe the derived listing the way the corruption actually looked:
    // manifest intact, index empty.
    {
        use koru_store_core::ports::DocumentIndex as _;
        use koru_store_core::service::Backend as _;
        let mut repo = SqliteBackend::new(f.dir.path())
            .open_workspace(workspace)
            .unwrap();
        repo.reindex(&[]).unwrap();
    }
    f.restart();
    let entries = f.store.documents(workspace).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, document);
}

#[test]
fn a_workspace_row_marked_deleted_without_manifest_state_is_cleared_on_open() {
    // The field case: a device showed a workspace as deleted while its
    // manifest — the authority — never recorded one. The reconcile used to
    // act only on `Some` projections, so an unset manifest could never clear
    // a stale row and the workspace stayed in the trash forever.
    let mut f = Fixture::new();
    let workspace = f.a_workspace();
    f.store
        .create_document(workspace, "Roadmap", "page", None, None, at(1))
        .unwrap();
    {
        let mut catalog = SqliteCatalog::open(&f.dir.path().join("catalog.db")).unwrap();
        catalog
            .set_workspace_deleted(workspace, Some(at(2)), &[])
            .unwrap();
    }
    f.restart();
    assert!(
        f.store
            .workspace_including_deleted(workspace)
            .unwrap()
            .deleted_at
            .is_some()
    );

    // Opening the workspace — as a sync digest computation does, including
    // deleted ones — reconciles the row from the manifest, which has no
    // deletion state: not deleted.
    f.store.workspace_sync_digest(workspace).unwrap();
    assert_eq!(
        f.store
            .workspace_including_deleted(workspace)
            .unwrap()
            .deleted_at,
        None
    );
}

#[test]
fn manifest_replication_applies_archived_deleted_and_restored_lifecycle() {
    let mut source = Fixture::new();
    let workspace = source.a_workspace();
    let manifest = source.store.manifest_id(workspace).unwrap();
    let profile = source
        .store
        .profile(source.store.workspace(workspace).unwrap().owner)
        .unwrap();
    let archived = source
        .store
        .set_workspace_appearance(workspace, None, None, false, true)
        .unwrap();

    let mut target = Fixture::new();
    target
        .store
        .provision_replica(&profile, &archived, manifest, at(1))
        .unwrap();
    sync_manifest(
        &mut source.store,
        &mut target.store,
        workspace,
        manifest,
        at(1),
    );
    assert!(target.store.workspace(workspace).unwrap().archived);

    source.store.delete_workspace(workspace, at(2)).unwrap();
    sync_manifest(
        &mut source.store,
        &mut target.store,
        workspace,
        manifest,
        at(3),
    );
    let replicated = target.store.workspace_including_deleted(workspace).unwrap();
    assert_eq!(replicated.deleted_at, Some(at(2)));
    assert!(replicated.archived);
    assert!(
        target.store.profiles().is_ok(),
        "a remote delete must not panic and poison the Store lock"
    );

    source.store.restore_workspace(workspace).unwrap();
    sync_manifest(
        &mut source.store,
        &mut target.store,
        workspace,
        manifest,
        at(4),
    );
    let replicated = target.store.workspace(workspace).unwrap();
    assert_eq!(replicated.deleted_at, None);
    assert!(replicated.archived);
    assert!(
        target
            .store
            .workspaces()
            .unwrap()
            .iter()
            .any(|row| row.id == workspace),
        "the restored workspace returns to the active catalog"
    );
}

fn sync_manifest(
    source: &mut TestStore,
    target: &mut TestStore,
    workspace: WorkspaceId,
    manifest: DocumentId,
    received_at: Timestamp,
) {
    let target_version = target.version(workspace, manifest).unwrap();
    let update = source
        .updates_since(workspace, manifest, &target_version)
        .unwrap();
    target
        .apply_update(workspace, manifest, update, received_at, None)
        .unwrap();
    assert!(
        target
            .flush(workspace, manifest, received_at)
            .unwrap()
            .is_some()
    );
}

#[test]
fn workspace_sync_digest_repairs_a_stale_leaf_after_restart() {
    let mut fixture = Fixture::new();
    let workspace = fixture.a_workspace();
    let before = fixture.store.workspace_sync_digest(workspace).unwrap();

    // Leave the persisted digest stale on purpose: the manifest commit is
    // durable, but no digest request runs before the process restarts and its
    // in-memory dirty set is lost.
    fixture
        .store
        .rename_workspace(workspace, "Renamed offline", at(5))
        .unwrap();
    fixture.restart();

    let repaired = fixture.store.workspace_sync_digest(workspace).unwrap();
    assert_ne!(repaired, before);
    assert_eq!(
        fixture.store.workspace_sync_digest(workspace).unwrap(),
        repaired,
        "once repaired, the derived root is stable"
    );
}

#[test]
fn workspace_sync_digest_repairs_membership_across_delete_and_restore() {
    let mut fixture = Fixture::new();
    let workspace = fixture.a_workspace();
    fixture
        .store
        .create_document(workspace, "Keep", "page", None, None, at(1))
        .unwrap();
    fixture.store.workspace_sync_digest(workspace).unwrap();

    fixture.store.delete_workspace(workspace, at(2)).unwrap();
    fixture.store.workspace_sync_digest(workspace).unwrap();
    let catalog = SqliteCatalog::open(&fixture.dir.path().join("catalog.db")).unwrap();
    assert_eq!(catalog.sync_document_digests(workspace).unwrap().len(), 1);
    drop(catalog);

    fixture.store.restore_workspace(workspace).unwrap();
    fixture.store.workspace_sync_digest(workspace).unwrap();
    let catalog = SqliteCatalog::open(&fixture.dir.path().join("catalog.db")).unwrap();
    assert_eq!(catalog.sync_document_digests(workspace).unwrap().len(), 2);
}

#[test]
fn replica_provisioning_refuses_identity_collisions_without_overwriting() {
    let mut fixture = Fixture::new();
    let workspace = fixture.a_workspace();
    let record = fixture.store.workspace(workspace).unwrap();
    let profile = fixture.store.profile(record.owner).unwrap();
    let manifest = fixture.store.manifest_id(workspace).unwrap();
    let mut stale_descriptor = profile.clone();
    stale_descriptor.name = "Older bootstrap name".into();
    fixture
        .store
        .provision_replica(&stale_descriptor, &record, manifest, at(1))
        .unwrap();

    let mut conflicting = profile.clone();
    conflicting.created_at = at(99);

    let error = fixture
        .store
        .provision_replica(&conflicting, &record, manifest, at(1))
        .unwrap_err();

    assert_eq!(error.code(), Code::ProfileExists);
    assert_eq!(
        fixture.store.profile(profile.id).unwrap().name,
        profile.name
    );
}

// ------------------------------------------------------------------ opening

#[test]
fn opening_a_store_touches_the_catalog_and_nothing_else() {
    // Lazy by construction: start-up must cost the same on a store holding one
    // workspace and one holding a thousand.
    let f = Fixture::new();
    assert_eq!(f.store.open_workspaces(), 0);
    assert_eq!(f.store.open_documents(), 0);
    assert!(!f.dir.path().join("workspaces").exists());
}

#[test]
fn a_workspace_is_opened_only_when_something_asks_for_it() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    assert_eq!(f.store.open_workspaces(), 1, "creating opens it");

    f.restart();
    assert_eq!(f.store.open_workspaces(), 0, "a restart opens nothing");
    let _ = f.store.documents(ws).unwrap();
    assert_eq!(f.store.open_workspaces(), 1, "listing opened it");
}

#[test]
fn the_peer_id_is_stable_across_restarts() {
    // Without this a `LoroDoc`'s peer is random per construction — including
    // every reload — so a store restarted a hundred times would carry a
    // hundred peers in every version vector it ever encoded.
    let mut f = Fixture::new();
    let first = f.store.peer_id();
    f.restart();
    assert_eq!(f.store.peer_id(), first);
    assert_ne!(first, 0);
}

// ------------------------------------------------------------------ profiles

#[test]
fn a_profile_needs_no_account_and_survives_a_restart() {
    let mut f = Fixture::new();
    let profile = f
        .store
        .create_profile("Trabajo", None, None, Locale::parse("es").unwrap(), at(0))
        .unwrap();
    assert_eq!(profile.account, None);

    f.restart();
    assert_eq!(f.store.profile(profile.id).unwrap().name, "Trabajo");
    assert_eq!(f.store.profiles().unwrap().len(), 1);
}

#[test]
fn clearing_a_personal_dictionary_commits_once_and_survives_restart() {
    let mut f = Fixture::new();
    let profile = f
        .store
        .create_profile("Owner", None, None, Locale::parse("en-US").unwrap(), at(0))
        .unwrap();
    f.store
        .set_dictionary_word(profile.id, "en-US", "alpha", true)
        .unwrap();
    f.store
        .set_dictionary_word(profile.id, "de", "beta", true)
        .unwrap();
    let _ = f.store.take_dictionary_committed();

    assert!(
        f.store
            .clear_personal_dictionary(profile.id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.store.take_dictionary_committed(),
        [koru_store_core::DictionaryCommitted {
            profile: profile.id
        }]
    );
    assert!(
        f.store
            .clear_personal_dictionary(profile.id)
            .unwrap()
            .is_empty()
    );
    assert!(f.store.take_dictionary_committed().is_empty());

    f.restart();
    assert!(f.store.personal_dictionary(profile.id).unwrap().is_empty());
}

#[test]
fn a_profile_catalog_row_converges_from_profile_crdt_updates() {
    use koru_store_core::profile_units::ProfileUnitRef;
    let mut source = Fixture::new();
    let profile = source
        .store
        .create_profile(
            "Trabajo",
            Some("Personal"),
            Some("color:purple"),
            Locale::parse("es").unwrap(),
            at(0),
        )
        .unwrap();
    let mut target = Fixture::new();
    target.store.provision_replica_profile(&profile).unwrap();
    assert_eq!(
        target.store.profile_version(profile.id).unwrap_err().code(),
        Code::VersionUnavailable
    );
    let target_version = target
        .store
        .profile_unit_version(ProfileUnitRef::Identity {
            profile: profile.id,
        })
        .unwrap();
    let update = source
        .store
        .profile_updates_since(profile.id, &target_version)
        .unwrap();

    target
        .store
        .apply_profile_updates(profile.id, &[update])
        .unwrap();
    assert_eq!(target.store.profile(profile.id).unwrap(), profile);
    target.restart();
    assert_eq!(target.store.profile(profile.id).unwrap(), profile);
}

#[test]
fn a_profile_rename_is_durable_and_replicates_as_crdt_updates() {
    use koru_store_core::profile_units::ProfileUnitRef;
    let mut source = Fixture::new();
    let profile = source
        .store
        .create_profile("Trabajo", None, None, Locale::parse("es").unwrap(), at(0))
        .unwrap();
    let mut target = Fixture::new();
    target.store.provision_replica_profile(&profile).unwrap();
    let before_rename = target
        .store
        .profile_unit_version(ProfileUnitRef::Identity {
            profile: profile.id,
        })
        .unwrap();
    let update = source
        .store
        .profile_updates_since(profile.id, &before_rename)
        .unwrap();
    target
        .store
        .apply_profile_updates(profile.id, &[update])
        .unwrap();

    let renamed = source
        .store
        .rename_profile(profile.id, " Personal ")
        .unwrap();
    assert_eq!(renamed.name, "Personal");
    source.restart();
    assert_eq!(source.store.profile(profile.id).unwrap().name, "Personal");

    // The rename is ordinary profile CRDT history, so an already-provisioned
    // replica converges on it through the same updates_since/apply pair as
    // any other profile change.
    let target_version = target.store.profile_version(profile.id).unwrap();
    let update = source
        .store
        .profile_updates_since(profile.id, &target_version)
        .unwrap();
    target
        .store
        .apply_profile_updates(profile.id, &[update])
        .unwrap();
    assert_eq!(target.store.profile(profile.id).unwrap().name, "Personal");

    // The same limits as creation: an empty name is refused and nothing moves.
    assert!(source.store.rename_profile(profile.id, "   ").is_err());
    assert_eq!(source.store.profile(profile.id).unwrap().name, "Personal");
}

#[test]
fn an_unknown_profile_is_not_found() {
    let f = Fixture::new();
    assert_eq!(
        f.store.profile(ProfileId::new()).unwrap_err().code(),
        Code::ProfileNotFound
    );
}

// ------------------------------------------------------------------ workspaces

/// One small edit committed and flushed, the way interactive typing lands.
fn drip(
    f: &mut Fixture,
    ws: WorkspaceId,
    doc: DocumentId,
    live: &loro::LoroDoc,
    sent: &mut loro::VersionVector,
    text: &str,
    at_ms: i64,
) {
    live.get_text("t").insert(0, text).unwrap();
    live.commit();
    let update = live.export(loro::ExportMode::updates(&*sent)).unwrap();
    *sent = live.oplog_vv();
    f.store
        .apply_update(ws, doc, update, at(at_ms), None)
        .unwrap();
    f.store.flush(ws, doc, at(at_ms)).unwrap();
}

fn a_document_with_a_replica(f: &mut Fixture, ws: WorkspaceId) -> (DocumentId, loro::LoroDoc) {
    let (doc, _) = f
        .store
        .create_document(ws, "Diary", "page", None, None, at(1))
        .unwrap();
    let live = loro::LoroDoc::new();
    live.import(&f.store.snapshot(ws, doc).unwrap()).unwrap();
    live.set_peer_id(9).unwrap();
    (doc, live)
}

/// The checkpoint policy accumulates across flushes. It used to compare each
/// 5ms commit batch against thresholds meant to be cumulative, so a document
/// edited in small steps — every manifest — never earned a checkpoint and its
/// journal grew forever.
#[test]
fn small_flushes_accumulate_into_a_checkpoint() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let (doc, live) = a_document_with_a_replica(&mut f, ws);
    f.store.checkpoints = koru_store_core::CheckpointPolicy {
        every_updates: 3,
        every_bytes: u64::MAX,
    };
    let mut sent = live.oplog_vv();
    for i in 0..5 {
        drip(&mut f, ws, doc, &live, &mut sent, "x", 2 + i);
    }
    let report = f.store.document_report(ws, doc).unwrap();
    assert!(
        report.latest_checkpoint.is_some(),
        "five one-update flushes must cross an every-3-updates policy"
    );
    // A checkpoint is client-facing: it appears in the document's history
    // with its own reason, so a person scanning versions can anchor on it.
    let history = f.store.history_entries(ws, doc, None, 50).unwrap();
    assert!(
        history
            .iter()
            .any(|entry| entry.reason == koru_store_core::ports::HistoryReason::Checkpoint),
        "the checkpoint must appear in the document history"
    );
}

/// The byte half of the policy accumulates the same way: updates each below
/// the threshold must still checkpoint once their sum crosses it.
#[test]
fn byte_heavy_flushes_accumulate_into_a_checkpoint() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let (doc, live) = a_document_with_a_replica(&mut f, ws);
    f.store.checkpoints = koru_store_core::CheckpointPolicy {
        every_updates: u64::MAX,
        every_bytes: 4096,
    };
    let mut sent = live.oplog_vv();
    let big = "x".repeat(3000);
    drip(&mut f, ws, doc, &live, &mut sent, &big, 2);
    drip(&mut f, ws, doc, &live, &mut sent, &big, 3);
    let report = f.store.document_report(ws, doc).unwrap();
    assert!(
        report.latest_checkpoint.is_some(),
        "two ~3KB updates must cross a 4KB cumulative byte policy"
    );
}

/// A document nobody edits again never flushes, so it would never earn its
/// first checkpoint. Loading is the repair point: the journal was just
/// replayed in full, and the overdue checkpoint makes that replay the last
/// one of its size.
#[test]
fn loading_takes_the_checkpoint_a_drip_fed_journal_never_earned() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let (doc, live) = a_document_with_a_replica(&mut f, ws);
    // Nothing checkpoints while the journal is written.
    f.store.checkpoints = koru_store_core::CheckpointPolicy {
        every_updates: u64::MAX,
        every_bytes: u64::MAX,
    };
    let mut sent = live.oplog_vv();
    for i in 0..5 {
        drip(&mut f, ws, doc, &live, &mut sent, "x", 2 + i);
    }
    assert!(
        f.store
            .document_report(ws, doc)
            .unwrap()
            .latest_checkpoint
            .is_none(),
        "the disabled policy must not have checkpointed"
    );

    f.restart();
    f.store.checkpoints = koru_store_core::CheckpointPolicy {
        every_updates: 3,
        every_bytes: u64::MAX,
    };
    // `document_report` loads the document, and loading takes the overdue
    // checkpoint without waiting for an edit that may never come.
    let report = f.store.document_report(ws, doc).unwrap();
    assert!(
        report.latest_checkpoint.is_some(),
        "loading a five-update journal under an every-3 policy must checkpoint"
    );
}

#[test]
fn a_workspace_comes_with_a_manifest() {
    // A workspace without a manifest is not a workspace, so the two are one
    // operation.
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let manifest = f.store.manifest_id(ws).unwrap();
    assert!(f.store.snapshot(ws, manifest).is_ok());
    assert!(titles(&mut f.store, ws).is_empty());
}

#[test]
fn a_workspace_for_a_missing_profile_is_refused_and_leaves_nothing_behind() {
    let mut f = Fixture::new();
    assert_eq!(
        f.store
            .create_workspace(ProfileId::new(), "Orphan", at(0))
            .unwrap_err()
            .code(),
        Code::ProfileNotFound
    );
    assert!(f.store.workspaces().unwrap().is_empty());
    assert!(
        !f.dir.path().join("workspaces").exists(),
        "a database was left"
    );
}

#[test]
fn an_unknown_workspace_is_not_found() {
    let mut f = Fixture::new();
    assert_eq!(
        f.store.documents(WorkspaceId::new()).unwrap_err().code(),
        Code::WorkspaceNotFound
    );
}

#[test]
fn a_workspace_and_its_manifest_survive_a_restart() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let manifest = f.store.manifest_id(ws).unwrap();

    f.restart();
    assert_eq!(f.store.manifest_id(ws).unwrap(), manifest, "the id moved");
    assert_eq!(f.store.workspace(ws).unwrap().name, "Workspace");
}

#[test]
fn deleting_and_restoring_a_workspace_preserves_every_byte_and_archive_state() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let document = f
        .store
        .create_document(ws, "Keep me", "page", None, None, at(1))
        .unwrap()
        .0;
    f.store
        .set_workspace_appearance(ws, None, None, false, true)
        .unwrap();

    let deleted = f.store.delete_workspace(ws, at(2)).unwrap();
    assert_eq!(deleted.deleted_at, Some(at(2)));
    assert!(deleted.archived, "delete must not consume the archive flag");
    assert!(f.store.workspaces().unwrap().is_empty());
    assert_eq!(f.store.locate(document).unwrap(), None);
    assert_eq!(f.store.workspaces_including_deleted().unwrap(), [deleted]);

    f.restart();
    let restored = f.store.restore_workspace(ws).unwrap();
    assert_eq!(restored.deleted_at, None);
    assert!(restored.archived);
    assert_eq!(f.store.locate(document).unwrap(), Some(ws));
    assert_eq!(titles(&mut f.store, ws), ["Keep me"]);
}

#[test]
fn candidate_retry_publishes_hidden_seed_after_receipt_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = {
        let mut store = Fixture::open_in(dir.path());
        let profile = store
            .create_profile("Owner", None, None, Locale::parse("en").unwrap(), at(0))
            .unwrap();
        store
            .create_workspace(profile.id, "Workspace", at(0))
            .unwrap()
            .id
    };
    let candidate = DocumentId::new();
    let original = LiveDocument::empty(candidate);
    original
        .doc()
        .get_text("body")
        .insert(0, "original")
        .unwrap();
    let original_update = original.export_all_updates().unwrap();
    {
        // The content-first transaction committed, but the process died before
        // Store could add the manifest entry.
        let backend = SqliteBackend::new(dir.path());
        let mut repo = SqliteWorkspace::open(&backend.workspace_db(workspace)).unwrap();
        repo.seed_document(&SeedDocument {
            candidate,
            recover_candidate: true,
            scope: "local:delegate".into(),
            key: "stable-schedule-id".into(),
            request_hash: "first".into(),
            initial_update: original_update.clone(),
            version_vector: original.encoded_version(),
            frontiers: original.encoded_frontiers(),
            created_at: at(1),
            expires_before: at(-7 * 24 * 60 * 60 * 1_000),
        })
        .unwrap();
    }

    let retry = LiveDocument::empty(candidate);
    retry
        .doc()
        .get_text("body")
        .insert(0, "later body")
        .unwrap();
    let eight_days_later = at(8 * 24 * 60 * 60 * 1_000);
    let mut store = Fixture::open_in(dir.path());
    let created = store
        .create_seeded_document(
            workspace,
            "Schedule",
            "koru-schedule",
            None,
            None,
            "local:delegate",
            "stable-schedule-id",
            "different",
            Some(candidate),
            retry.export_all_updates().unwrap(),
            eight_days_later,
        )
        .unwrap();

    assert!(created.replayed);
    assert_eq!(created.document, candidate);
    assert_eq!(created.initial_update, original_update);
    assert_eq!(store.documents(workspace).unwrap()[0].id, candidate);
    let mut opened = LiveDocument::empty(candidate);
    opened
        .import(&store.snapshot(workspace, candidate).unwrap())
        .unwrap();
    assert_eq!(opened.doc().get_text("body").to_string(), "original");
}

#[test]
fn deleted_workspace_keeps_candidate_identity_after_restart_and_receipt_expiry() {
    let mut f = Fixture::new();
    let profile = f
        .store
        .create_profile("Owner", None, None, Locale::parse("en").unwrap(), at(0))
        .unwrap();
    let original_workspace = f
        .store
        .create_workspace(profile.id, "Original", at(0))
        .unwrap()
        .id;
    let other_workspace = f
        .store
        .create_workspace(profile.id, "Other", at(0))
        .unwrap()
        .id;
    let candidate = DocumentId::new();
    let original = LiveDocument::empty(candidate);
    original
        .doc()
        .get_text("body")
        .insert(0, "original")
        .unwrap();
    f.store
        .create_seeded_document(
            original_workspace,
            "Schedule",
            "koru-schedule",
            None,
            None,
            "local:delegate",
            "stable-schedule-id",
            "first",
            Some(candidate),
            original.export_all_updates().unwrap(),
            at(1),
        )
        .unwrap();
    f.store.delete_workspace(original_workspace, at(2)).unwrap();
    f.restart();

    let retry = LiveDocument::empty(candidate);
    retry
        .doc()
        .get_text("body")
        .insert(0, "replacement")
        .unwrap();
    let error = match f.store.create_seeded_document(
        other_workspace,
        "Schedule",
        "koru-schedule",
        None,
        None,
        "local:delegate",
        "stable-schedule-id",
        "different",
        Some(candidate),
        retry.export_all_updates().unwrap(),
        at(8 * 24 * 60 * 60 * 1_000),
    ) {
        Ok(_) => panic!("a deleted workspace lost its candidate identity"),
        Err(error) => error,
    };

    assert_eq!(error.code(), Code::IdempotencyConflict);
    assert!(f.store.documents(other_workspace).unwrap().is_empty());
    f.store.restore_workspace(original_workspace).unwrap();
    let documents = f.store.documents(original_workspace).unwrap();
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].id, candidate);
}

#[test]
fn candidate_seed_fails_closed_when_two_physical_workspaces_already_own_it() {
    let dir = tempfile::tempdir().unwrap();
    let (first_workspace, second_workspace, target_workspace) = {
        let mut store = Fixture::open_in(dir.path());
        let profile = store
            .create_profile("Owner", None, None, Locale::parse("en").unwrap(), at(0))
            .unwrap();
        let mut create = |name| store.create_workspace(profile.id, name, at(0)).unwrap().id;
        (create("First"), create("Second"), create("Target"))
    };
    let candidate = DocumentId::new();
    let original = LiveDocument::empty(candidate);
    original
        .doc()
        .get_text("body")
        .insert(0, "original")
        .unwrap();
    let initial_update = original.export_all_updates().unwrap();
    let backend = SqliteBackend::new(dir.path());
    for (workspace, key) in [
        (first_workspace, "first-root"),
        (second_workspace, "second-root"),
    ] {
        let mut repo = SqliteWorkspace::open(&backend.workspace_db(workspace)).unwrap();
        repo.seed_document(&SeedDocument {
            candidate,
            recover_candidate: true,
            scope: "local:delegate".into(),
            key: key.into(),
            request_hash: key.into(),
            initial_update: initial_update.clone(),
            version_vector: original.encoded_version(),
            frontiers: original.encoded_frontiers(),
            created_at: at(1),
            expires_before: at(-7 * 24 * 60 * 60 * 1_000),
        })
        .unwrap();
    }

    let retry = LiveDocument::empty(candidate);
    let mut store = Fixture::open_in(dir.path());
    let error = match store.create_seeded_document(
        target_workspace,
        "Schedule",
        "koru-schedule",
        None,
        None,
        "local:delegate",
        "stable-schedule-id",
        "retry",
        Some(candidate),
        retry.export_all_updates().unwrap(),
        at(2),
    ) {
        Ok(_) => panic!("two physical owners were accepted"),
        Err(error) => error,
    };

    assert_eq!(error.code(), Code::IdempotencyConflict);
    assert!(store.documents(target_workspace).unwrap().is_empty());
}

fn account_binding(id: &str) -> ProfileAccountBinding {
    ProfileAccountBinding::parse_identity_id(id).unwrap()
}

#[test]
fn profile_account_binding_is_an_idempotent_durable_compare_and_set() {
    let mut f = Fixture::new();
    let workspace = f.a_workspace();
    let before_workspace = f.store.workspace(workspace).unwrap();
    let profile = f.store.profile(before_workspace.owner).unwrap();
    let document = f
        .store
        .create_document(workspace, "Binding keeps me", "page", None, None, at(1))
        .unwrap()
        .0;
    let old = account_binding("10000000-0000-4000-8000-000000000001");
    let wrong = account_binding("20000000-0000-4000-8000-000000000002");
    let new = account_binding("30000000-0000-4000-8000-000000000003");

    assert_eq!(
        f.store.bind_profile_account(profile.id, None, old).unwrap(),
        ProfileAccountBindingOutcome::Bound
    );
    assert_eq!(
        f.store.bind_profile_account(profile.id, None, old).unwrap(),
        ProfileAccountBindingOutcome::AlreadyBound
    );
    assert_eq!(
        f.store
            .bind_profile_account(profile.id, Some(wrong), new)
            .unwrap_err()
            .code(),
        Code::ProfileAccountConflict
    );
    assert_eq!(
        f.store.profile(profile.id).unwrap().account.as_deref(),
        Some("koru:10000000-0000-4000-8000-000000000001")
    );

    // The account service approved an erased old identity. Rebinding is an
    // explicit CAS against that exact old identity; it is never an overwrite.
    assert_eq!(
        f.store
            .bind_profile_account(profile.id, Some(old), new)
            .unwrap(),
        ProfileAccountBindingOutcome::Bound
    );
    assert_eq!(
        f.store
            .bind_profile_account(profile.id, Some(old), new)
            .unwrap(),
        ProfileAccountBindingOutcome::AlreadyBound
    );

    f.restart();
    let persisted = f.store.profile(profile.id).unwrap();
    assert_eq!(persisted.id, profile.id);
    assert_eq!(persisted.name, profile.name);
    assert_eq!(
        persisted.account.as_deref(),
        Some("koru:30000000-0000-4000-8000-000000000003")
    );
    let listed = f
        .store
        .profiles()
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.id == profile.id)
        .unwrap();
    assert_eq!(listed.account, persisted.account);
    assert_eq!(
        f.store.workspace(workspace).unwrap().id,
        before_workspace.id
    );
    assert_eq!(titles(&mut f.store, workspace), ["Binding keeps me"]);
    assert!(
        f.store
            .documents(workspace)
            .unwrap()
            .iter()
            .any(|entry| entry.id == document)
    );
}

#[test]
fn profile_account_binding_distinguishes_missing_and_locally_removed() {
    let mut f = Fixture::new();
    let identity = account_binding("10000000-0000-4000-8000-000000000001");
    assert_eq!(
        f.store
            .bind_profile_account(ProfileId::new(), None, identity)
            .unwrap_err()
            .code(),
        Code::ProfileNotFound
    );

    let removed = f
        .store
        .create_profile("Removed", None, None, Locale::default(), at(1))
        .unwrap();
    let mark = f
        .store
        .begin_profile_removal(
            removed.id,
            koru_store_core::profile_removal::RemovalKind::Local,
            at(2),
        )
        .unwrap();
    f.store
        .remove_profile_local(removed.id, Some(mark.attempt), at(2))
        .unwrap();
    assert_eq!(
        f.store
            .bind_profile_account(removed.id, None, identity)
            .unwrap_err()
            .code(),
        Code::ProfileRemoved
    );
}

#[test]
fn clearing_an_exact_profile_account_is_idempotent_durable_and_keeps_content() {
    let mut f = Fixture::new();
    let workspace = f.a_workspace();
    let profile = f.store.workspace(workspace).unwrap().owner;
    let document = f
        .store
        .create_document(workspace, "Continue offline", "page", None, None, at(1))
        .unwrap()
        .0;
    let expected = account_binding("10000000-0000-4000-8000-000000000001");
    let wrong = account_binding("20000000-0000-4000-8000-000000000002");
    f.store
        .bind_profile_account(profile, None, expected)
        .unwrap();

    assert_eq!(
        f.store
            .clear_profile_account(profile, wrong)
            .unwrap_err()
            .code(),
        Code::ProfileAccountConflict
    );
    assert_eq!(
        f.store.clear_profile_account(profile, expected).unwrap(),
        ProfileAccountClearOutcome::Cleared
    );
    assert_eq!(
        f.store.clear_profile_account(profile, expected).unwrap(),
        ProfileAccountClearOutcome::AlreadyOffline
    );

    f.restart();
    assert_eq!(f.store.profile(profile).unwrap().account, None);
    assert_eq!(f.store.workspace(workspace).unwrap().owner, profile);
    assert!(
        f.store
            .documents(workspace)
            .unwrap()
            .iter()
            .any(|entry| entry.id == document),
        "continuing offline must retain every local document"
    );
}

#[test]
fn local_removal_refuses_a_legacy_holder_row() {
    let dir = tempfile::tempdir().unwrap();
    let mut catalog = SqliteCatalog::open(&dir.path().join("catalog.db")).unwrap();
    let holder = Profile::holder(Locale::parse("en").unwrap(), at(1)).unwrap();
    catalog.put_profile(&holder).unwrap();
    let mut store = Store::open(catalog, SqliteBackend::new(dir.path())).unwrap();

    assert_eq!(
        store
            .remove_profile_local(holder.id, None, at(2))
            .unwrap_err()
            .code(),
        Code::ProfileReserved
    );
    assert_eq!(store.profile(holder.id).unwrap(), holder);
}
#[test]
fn removing_an_empty_profile_uses_soft_deletion_and_keeps_its_owned_content() {
    let mut f = Fixture::new();
    let workspace = f.a_workspace();
    let owner = f.store.workspace(workspace).unwrap().owner;
    let document = f
        .store
        .create_document(workspace, "Still here", "page", None, None, at(1))
        .unwrap()
        .0;

    let mark = f
        .store
        .begin_profile_removal(
            owner,
            koru_store_core::profile_removal::RemovalKind::Local,
            at(2),
        )
        .unwrap();
    assert_eq!(
        f.store
            .remove_profile_local(owner, Some(mark.attempt), at(2))
            .unwrap_err()
            .code(),
        Code::ProfileNotEmpty
    );
    f.store
        .delete_workspace_for_removal(workspace, mark.attempt, at(3))
        .unwrap();
    assert_eq!(
        f.store
            .remove_profile_local(owner, Some(mark.attempt), at(4))
            .unwrap(),
        LocalProfileRemoval::Removed
    );
    assert!(f.store.profiles().unwrap().is_empty());

    let retained = f.store.workspace_including_deleted(workspace).unwrap();
    assert_eq!(retained.owner, owner);
    assert_eq!(retained.deleted_at, Some(at(3)));
    assert_eq!(
        f.store.restore_workspace(workspace).unwrap_err().code(),
        Code::ProfileNotFound
    );
    assert_eq!(
        f.store.workspace_manifest_deletion(workspace).unwrap(),
        Some(Some(at(3))),
        "a refused restore must not edit the retained tombstone"
    );
    let repo = SqliteBackend::new(f.dir.path())
        .open_workspace(workspace)
        .unwrap();
    assert!(
        repo.document(document).unwrap().is_some(),
        "local Profile removal must not delete Workspace content"
    );
    assert_eq!(
        f.store
            .remove_profile_local(owner, Some(mark.attempt), at(5))
            .unwrap_err()
            .code(),
        Code::RemovalAttemptStale
    );

    assert_eq!(
        f.store.restore_profile_local(owner).unwrap(),
        LocalProfileRestoration::Restored
    );
    assert_eq!(f.store.profile(owner).unwrap().id, owner);
    assert_eq!(
        f.store.restore_profile_local(owner).unwrap(),
        LocalProfileRestoration::AlreadyActive
    );
    assert!(
        f.store
            .workspace_including_deleted(workspace)
            .unwrap()
            .deleted_at
            .is_some(),
        "restoring a Profile must not silently restore its trashed Workspaces"
    );
    f.store.restore_workspace(workspace).unwrap();
    assert!(
        f.store
            .documents(workspace)
            .unwrap()
            .iter()
            .any(|entry| entry.id == document),
        "the explicitly restored Workspace keeps its retained documents"
    );
}

#[test]
fn purge_removes_only_deleted_workspaces_at_or_before_the_cutoff() {
    let mut f = Fixture::new();
    let expired = f.a_workspace();
    let recent = f.a_workspace();
    f.store.delete_workspace(expired, at(2)).unwrap();
    f.store.delete_workspace(recent, at(20)).unwrap();
    let expired_dir = f.dir.path().join("workspaces").join(expired.to_storage());
    let recent_dir = f.dir.path().join("workspaces").join(recent.to_storage());

    // Retention is per deletion fact: a cutoff of `at(10)` plus the default
    // retention expires the deletion at `at(2)` and not the one at `at(20)`.
    let cutoff = Timestamp::from_millis(at(10).millis() + koru_store_core::TRASH_RETENTION_MS);
    let outcome = f.store.purge_deleted(cutoff).unwrap();
    assert_eq!(outcome.purged, vec![expired]);
    assert!(outcome.held.is_empty());
    assert!(outcome.pending.is_empty());
    assert!(!expired_dir.exists());
    assert!(recent_dir.exists());
    assert_eq!(
        f.store.workspace(expired).unwrap_err().code(),
        Code::WorkspaceNotFound
    );
    assert_eq!(
        f.store.workspaces_including_deleted().unwrap()[0].id,
        recent
    );
}

// ------------------------------------------------------------------ documents

#[test]
fn a_document_appears_in_the_listing_and_survives_a_restart() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    f.store
        .create_document(ws, "四半期計画", "page", None, None, at(0))
        .unwrap();

    assert_eq!(titles(&mut f.store, ws), ["四半期計画"]);
    f.restart();
    assert_eq!(titles(&mut f.store, ws), ["四半期計画"]);
}

#[test]
fn titles_in_every_supported_language_round_trip_through_a_restart() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    for s in SAMPLES {
        f.store
            .create_document(ws, s.title, "page", None, None, at(0))
            .unwrap();
    }
    f.store
        .create_document(ws, BIDI_LINE, "page", None, None, at(0))
        .unwrap();

    f.restart();
    let mut expected: Vec<String> = SAMPLES.iter().map(|s| s.title.to_string()).collect();
    expected.push(BIDI_LINE.to_string());
    assert_eq!(titles(&mut f.store, ws), expected);
}

#[test]
fn documents_can_nest() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let (parent, _) = f
        .store
        .create_document(ws, "Parent", "page", None, None, at(0))
        .unwrap();
    let (child, _) = f
        .store
        .create_document(ws, "Child", "page", Some(parent), None, at(0))
        .unwrap();

    f.restart();
    let entries = f.store.documents(ws).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].id, child);
    assert_eq!(entries[1].parent, Some(parent));
}

// ------------------------------------------------------------------ durability

#[test]
fn nothing_is_durable_until_it_is_flushed() {
    // The heart of the contract. `apply_update` validates and queues; only
    // `flush` puts bytes on the disk, and only then may anything be
    // acknowledged.
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let manifest = f.store.manifest_id(ws).unwrap();

    let mut client = Client::joining(&mut f.store, ws, manifest);
    let update = client.add("Unflushed");
    f.store
        .apply_update(ws, manifest, update, at(0), None)
        .unwrap();

    // The live document has it — that is what validation means — but a restart
    // does not, because nothing was committed.
    assert_eq!(titles(&mut f.store, ws), Vec::<String>::new());
    f.restart();
    assert_eq!(titles(&mut f.store, ws), Vec::<String>::new());
}

#[test]
fn a_flushed_update_survives_a_restart() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let manifest = f.store.manifest_id(ws).unwrap();

    let mut client = Client::joining(&mut f.store, ws, manifest);
    let update = client.add("Acknowledged");
    f.store
        .apply_update(ws, manifest, update, at(0), None)
        .unwrap();
    let head = f.store.flush(ws, manifest, at(0)).unwrap();
    assert!(
        head.is_some(),
        "a flush with queued work must report a sequence"
    );

    f.restart();
    assert_eq!(titles(&mut f.store, ws), ["Acknowledged"]);
}

#[test]
fn a_replicated_manifest_rename_updates_the_catalog_and_is_not_overwritten_by_inventory() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let manifest = f.store.manifest_id(ws).unwrap();
    let mut peer = Client::joining(&mut f.store, ws, manifest);

    let update = peer.rename("Renamed on mobile");
    f.store
        .apply_update(ws, manifest, update, at(1), None)
        .unwrap();
    f.store.flush(ws, manifest, at(1)).unwrap();
    assert_eq!(f.store.workspace(ws).unwrap().name, "Renamed on mobile");

    // The peer's bootstrap descriptor may still carry the old name. Once this
    // workspace exists, that descriptor is not a competing metadata register.
    let mut stale = f.store.workspace(ws).unwrap();
    stale.name = "Workspace".into();
    let profile = f.store.profile(stale.owner).unwrap();
    f.store
        .provision_replica(&profile, &stale, manifest, at(2))
        .unwrap();
    assert_eq!(f.store.workspace(ws).unwrap().name, "Renamed on mobile");

    f.restart();
    assert_eq!(f.store.workspace(ws).unwrap().name, "Renamed on mobile");
}

#[test]
fn document_entry_rename_and_trash_are_durable_manifest_commits() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let document = f
        .store
        .create_document(ws, "Original", "document", None, None, at(1))
        .unwrap()
        .0;

    f.store
        .rename_document_entry(ws, document, "Renamed", at(2))
        .unwrap();
    f.store
        .trash_document_entry(ws, document, at(3), Some("Mobile"))
        .unwrap();

    f.restart();
    let entry = f
        .store
        .documents(ws)
        .unwrap()
        .into_iter()
        .find(|entry| entry.id == document)
        .unwrap();
    assert_eq!(entry.title, "Renamed");
    assert_eq!(entry.trashed_at, Some(at(3)));
    assert_eq!(entry.trashed_by.as_deref(), Some("Mobile"));
}

#[test]
fn remote_cross_workspace_move_releases_the_old_derived_location() {
    let mut f = Fixture::new();
    let source = f.a_workspace();
    let target = f.a_workspace();
    let document = f
        .store
        .create_document(source, "Moved", "document", None, None, at(1))
        .unwrap()
        .0;
    let source_manifest = f.store.manifest_id(source).unwrap();
    let target_manifest = f.store.manifest_id(target).unwrap();
    let mut remote_source =
        Manifest::from_snapshot(&f.store.snapshot(source, source_manifest).unwrap()).unwrap();
    let mut remote_target =
        Manifest::from_snapshot(&f.store.snapshot(target, target_manifest).unwrap()).unwrap();

    // Target addition arrives first. Refusing to create a second owner is
    // correct until the source removal becomes durable.
    let before_target = remote_target.version();
    remote_target
        .add_document_at(document, "Moved", "document", None, None)
        .unwrap();
    f.store
        .apply_update(
            target,
            target_manifest,
            remote_target.export_updates_since(&before_target).unwrap(),
            at(2),
            None,
        )
        .unwrap();
    f.store.flush(target, target_manifest, at(2)).unwrap();
    assert_eq!(
        f.store
            .provision_replica_document(target, document, at(2))
            .unwrap_err()
            .code(),
        Code::DocumentExists
    );

    // Once the old manifest removes membership, retained source bytes no
    // longer own the globally unique ID. The next bounded retry provisions the
    // target and can receive its missing CRDT operations.
    let before_source = remote_source.version();
    remote_source.remove_document(document).unwrap();
    f.store
        .apply_update(
            source,
            source_manifest,
            remote_source.export_updates_since(&before_source).unwrap(),
            at(3),
            None,
        )
        .unwrap();
    f.store.flush(source, source_manifest, at(3)).unwrap();
    assert_eq!(f.store.locate(document).unwrap(), None);
    f.store
        .provision_replica_document(target, document, at(4))
        .unwrap();
    assert_eq!(f.store.locate(document).unwrap(), Some(target));
}

#[test]
fn many_updates_share_one_commit() {
    // What group commit is for: a burst of typing costs one fsync, not one per
    // keystroke — and every update in the window is durable when it returns.
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let manifest = f.store.manifest_id(ws).unwrap();

    let mut client = Client::joining(&mut f.store, ws, manifest);
    for title in ["One", "Two", "Three"] {
        let update = client.add(title);
        f.store
            .apply_update(ws, manifest, update, at(0), None)
            .unwrap();
    }
    let head = f
        .store
        .flush(ws, manifest, at(1))
        .unwrap()
        .expect("a sequence");
    assert_eq!(
        head, 4,
        "one for the manifest's creation, three for the edits"
    );

    f.restart();
    assert_eq!(titles(&mut f.store, ws), ["One", "Two", "Three"]);
}

#[test]
fn flushing_nothing_is_not_an_error() {
    // A commit timer firing on a quiet window is normal.
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let manifest = f.store.manifest_id(ws).unwrap();
    assert_eq!(f.store.flush(ws, manifest, at(0)).unwrap(), None);
}

#[test]
fn a_malformed_update_is_refused_before_anything_is_written() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let manifest = f.store.manifest_id(ws).unwrap();

    assert_eq!(
        f.store
            .apply_update(ws, manifest, b"not an update".to_vec(), at(0), None)
            .unwrap_err()
            .code(),
        Code::CrdtRejected
    );
    // And there is nothing queued, so a later flush cannot carry it.
    assert_eq!(f.store.flush(ws, manifest, at(0)).unwrap(), None);
}

#[test]
fn a_redelivered_update_costs_bytes_rather_than_correctness() {
    // Transports retry. Applying the same update twice must change nothing.
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let manifest = f.store.manifest_id(ws).unwrap();

    let mut client = Client::joining(&mut f.store, ws, manifest);
    let update = client.add("Once");
    f.store
        .apply_update(ws, manifest, update.clone(), at(0), None)
        .unwrap();
    f.store.flush(ws, manifest, at(0)).unwrap();

    f.store
        .apply_update(ws, manifest, update.clone(), at(1), None)
        .unwrap();
    assert_eq!(
        f.store.flush(ws, manifest, at(1)).unwrap(),
        None,
        "a known update was queued again"
    );
    f.restart();
    assert_eq!(titles(&mut f.store, ws), ["Once"]);
}

#[test]
fn a_window_reports_when_it_is_due() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let manifest = f.store.manifest_id(ws).unwrap();
    assert!(f.store.due(at(1000)).is_empty());
    assert_eq!(f.store.next_due(), None);

    let mut client = Client::joining(&mut f.store, ws, manifest);
    let update = client.add("Queued");
    f.store
        .apply_update(ws, manifest, update, at(0), None)
        .unwrap();

    assert!(f.store.due(at(0)).is_empty(), "not yet");
    assert_eq!(f.store.due(at(1000)), vec![(ws, manifest)]);
    assert!(f.store.next_due().is_some());
}

// ------------------------------------------------------------------ two clients

#[test]
fn two_clients_converge_through_the_store() {
    // The shape of the §17 slice, without a network in the way.
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let manifest = f.store.manifest_id(ws).unwrap();

    let mut a = Client::joining(&mut f.store, ws, manifest);
    let update = a.add("From A");
    f.store
        .apply_update(ws, manifest, update, at(0), None)
        .unwrap();
    f.store.flush(ws, manifest, at(0)).unwrap();

    // B joins afterwards and gets a snapshot, so it needs no history.
    let b = Client::joining(&mut f.store, ws, manifest);
    assert_eq!(
        b.manifest
            .entries()
            .into_iter()
            .map(|e| e.title)
            .collect::<Vec<_>>(),
        ["From A"]
    );
}

#[test]
fn a_client_catches_up_from_its_own_version_vector() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let manifest = f.store.manifest_id(ws).unwrap();

    let mut a = Client::joining(&mut f.store, ws, manifest);
    let mut b = Client::joining(&mut f.store, ws, manifest);
    let behind = b.manifest.version();

    for title in ["One", "Two"] {
        let update = a.add(title);
        f.store
            .apply_update(ws, manifest, update, at(0), None)
            .unwrap();
    }
    f.store.flush(ws, manifest, at(0)).unwrap();

    // One delta for everything B is missing, rather than a fresh snapshot.
    let catch_up = f.store.updates_since(ws, manifest, &behind).unwrap();
    b.manifest.import(&catch_up).unwrap();
    assert_eq!(
        b.manifest
            .entries()
            .into_iter()
            .map(|e| e.title)
            .collect::<Vec<_>>(),
        ["One", "Two"]
    );
}

// ------------------------------------------------------- the committed queue

/// Everything committed since the last drain, keyed so a test can ask about one
/// document without caring what else moved.
fn drained(store: &mut TestStore) -> Vec<koru_store_core::Committed> {
    let mut out = store.take_committed();
    out.sort_by_key(|c| c.document.to_storage());
    out
}

/// **The signal derived state is built from.** Without it every consumer would
/// have to poll, and polling a store to find out whether anything happened is
/// the shape this whole layer exists to avoid.
#[test]
fn a_commit_is_reported_once_it_is_on_the_disk() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let (doc, _) = f
        .store
        .create_document(ws, "Roadmap", "page", None, None, at(0))
        .unwrap();
    let _ = drained(&mut f.store);

    let manifest = f.store.manifest_id(ws).unwrap();
    let mut client = Client::joining(&mut f.store, ws, manifest);
    let update = client.add("Another");
    f.store
        .apply_update(ws, manifest, update, at(1), None)
        .unwrap();

    assert!(
        drained(&mut f.store).is_empty(),
        "an update that has not been flushed is not on the disk, and must not be reported"
    );

    f.store.flush(ws, manifest, at(1)).unwrap();
    let committed = drained(&mut f.store);
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0].document, manifest);
    assert!(committed[0].is_manifest, "the manifest says so");
    assert!(!committed[0].version_vector.is_empty());
    let _ = doc;
}

/// **A trigger, not a log.** Twenty commits to one document are one entry
/// naming the newest revision — which is what a consumer wants, since it
/// re-reads the state rather than replaying what it was told, and it is what
/// keeps the queue bounded when nothing is draining it.
#[test]
fn many_commits_to_one_document_coalesce_to_the_newest() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let manifest = f.store.manifest_id(ws).unwrap();
    let mut client = Client::joining(&mut f.store, ws, manifest);
    let _ = drained(&mut f.store);

    let mut last = Vec::new();
    for i in 0..20 {
        let update = client.add(&format!("Doc {i}"));
        f.store
            .apply_update(ws, manifest, update, at(i), None)
            .unwrap();
        if let Ok(Some(_)) = f.store.flush(ws, manifest, at(i)) {
            last = f.store.version(ws, manifest).expect("a version").encode();
        }
    }

    let committed = drained(&mut f.store);
    assert_eq!(committed.len(), 1, "one entry per document, not per commit");
    assert_eq!(
        committed[0].version_vector, last,
        "the entry names the newest revision, not the first"
    );
}

/// Draining is draining: what has been taken is not handed out again, or a
/// worker would re-project the same document forever.
#[test]
fn draining_the_queue_empties_it() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    f.store
        .create_document(ws, "Roadmap", "page", None, None, at(0))
        .unwrap();

    assert!(!drained(&mut f.store).is_empty());
    assert!(drained(&mut f.store).is_empty());
}

/// Two documents are two entries. The queue is keyed by document precisely so
/// that a busy one cannot hide a quiet one.
#[test]
fn two_documents_are_two_entries() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let (one, _) = f
        .store
        .create_document(ws, "One", "page", None, None, at(0))
        .unwrap();
    let (two, _) = f
        .store
        .create_document(ws, "Two", "page", None, None, at(0))
        .unwrap();
    let _ = drained(&mut f.store);

    for (doc, text) in [(one, "first"), (two, "second")] {
        let live = loro::LoroDoc::new();
        live.import(&f.store.snapshot(ws, doc).unwrap()).unwrap();
        live.get_text("t").insert(0, text).unwrap();
        live.commit();
        let update = live.export(loro::ExportMode::all_updates()).unwrap();
        f.store.apply_update(ws, doc, update, at(1), None).unwrap();
        f.store.flush(ws, doc, at(1)).unwrap();
    }

    let committed = drained(&mut f.store);
    assert_eq!(committed.len(), 2);
    assert!(committed.iter().all(|c| !c.is_manifest));
}

// --------------------------------------------------------------- forking

/// **The seam derived state reads through.** A fork is a structural share, so a
/// caller can take it under the store's lock and walk it outside — which is the
/// whole reason a projection does not block every other workspace's commit.
#[test]
fn a_forked_document_holds_what_the_store_holds_and_does_not_follow_it() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let (doc, _) = f
        .store
        .create_document(ws, "Roadmap", "page", None, None, at(0))
        .unwrap();

    let live = loro::LoroDoc::new();
    live.import(&f.store.snapshot(ws, doc).unwrap()).unwrap();
    live.get_text("t").insert(0, "before").unwrap();
    live.commit();
    let update = live.export(loro::ExportMode::all_updates()).unwrap();
    f.store.apply_update(ws, doc, update, at(1), None).unwrap();
    f.store.flush(ws, doc, at(1)).unwrap();

    let forked = f.store.fork_document(ws, doc).unwrap();
    assert_eq!(forked.get_text("t").to_string(), "before");

    // The store moves on. The fork must not, or a projection would describe a
    // revision it never claimed to have read.
    live.get_text("t").insert(6, " and after").unwrap();
    live.commit();
    let update = live.export(loro::ExportMode::all_updates()).unwrap();
    f.store.apply_update(ws, doc, update, at(2), None).unwrap();
    f.store.flush(ws, doc, at(2)).unwrap();

    assert_eq!(
        forked.get_text("t").to_string(),
        "before",
        "the fork followed the document it was taken from"
    );
    assert_eq!(
        f.store
            .fork_document(ws, doc)
            .unwrap()
            .get_text("t")
            .to_string(),
        "before and after"
    );
}

/// The manifest is a document to the store and is not a Koru document. Handing
/// it to something that projects content would have it walk the store's own
/// shape and call the result somebody's page.
#[test]
fn the_manifest_cannot_be_forked_for_projection() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let manifest = f.store.manifest_id(ws).unwrap();
    assert_eq!(
        f.store.fork_document(ws, manifest).unwrap_err().code,
        Code::InvalidRequest
    );
}

/// **Every durable path reports, not only `flush`.** Creating a workspace,
/// renaming one and creating a document all commit through `commit_one` and
/// never go near the group-commit window — so derived state hooked only to
/// `flush` would miss a newly created document until somebody edited it, and
/// miss a rename entirely.
///
/// One test per path, because the failure is silent and the paths are only
/// discoverable by reading `service.rs`.
#[test]
fn creating_a_workspace_reports_a_commit() {
    let mut f = Fixture::new();
    let profile = f
        .store
        .create_profile("Owner", None, None, Locale::parse("en").unwrap(), at(0))
        .unwrap();
    let _ = drained(&mut f.store);

    let ws = f
        .store
        .create_workspace(profile.id, "Workspace", at(0))
        .unwrap();

    let committed = drained(&mut f.store);
    assert_eq!(committed.len(), 1, "the manifest's own creation was silent");
    assert_eq!(committed[0].workspace, ws.id);
    assert!(committed[0].is_manifest);
}

#[test]
fn creating_a_document_reports_a_commit() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let _ = drained(&mut f.store);

    f.store
        .create_document(ws, "Roadmap", "page", None, None, at(0))
        .unwrap();

    let committed = drained(&mut f.store);
    assert_eq!(
        committed.len(),
        1,
        "a document created over HTTP would never be projected"
    );
    assert!(
        committed[0].is_manifest,
        "it is the manifest that changed; the document itself is still empty"
    );
}

#[test]
fn renaming_a_workspace_reports_a_commit() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let _ = drained(&mut f.store);

    f.store.rename_workspace(ws, "Renamed", at(1)).unwrap();

    let committed = drained(&mut f.store);
    assert_eq!(
        committed.len(),
        1,
        "the rename reached the journal silently"
    );
    assert!(committed[0].is_manifest);
}

#[test]
fn renaming_a_cold_workspace_keeps_catalog_and_manifest_aligned() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let manifest = f.store.manifest_id(ws).unwrap();
    f.restart();

    f.store.rename_workspace(ws, "Cold rename", at(1)).unwrap();
    assert_eq!(f.store.workspace(ws).unwrap().name, "Cold rename");
    let snapshot = f.store.snapshot(ws, manifest).unwrap();
    assert_eq!(
        koru_store_core::Manifest::from_snapshot(&snapshot)
            .unwrap()
            .name()
            .as_deref(),
        Some("Cold rename")
    );

    f.restart();
    assert_eq!(f.store.workspace(ws).unwrap().name, "Cold rename");
}

// ------------------------------------------------------- history: rename

fn manual_marker(f: &mut Fixture, ws: WorkspaceId, doc: DocumentId, at_ms: i64) -> HistoryEntry {
    f.store
        .capture_history(
            ws,
            doc,
            None,
            HistoryReason::Manual,
            at(at_ms),
            None,
            None,
            None,
        )
        .unwrap()
}

/// A marker's label is the one thing about it that can change, and the change
/// is what a later listing reads — not something the caller was handed back.
#[test]
fn a_history_marker_can_be_renamed_and_the_name_survives_a_restart() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let (doc, live) = a_document_with_a_replica(&mut f, ws);
    let mut sent = live.oplog_vv();
    drip(&mut f, ws, doc, &live, &mut sent, "a draft", 2);
    let marker = manual_marker(&mut f, ws, doc, 3);
    assert_eq!(marker.label, None);

    let renamed = f
        .store
        .relabel_history(ws, marker.id, Some("Before the rewrite"))
        .unwrap();
    assert_eq!(renamed.label.as_deref(), Some("Before the rewrite"));
    assert_eq!(
        renamed.recorded_at, marker.recorded_at,
        "nothing else moved"
    );
    assert_eq!(renamed.version, marker.version);
    assert_eq!(renamed.reason, marker.reason);

    f.restart();
    let listed = f.store.history_entries(ws, doc, None, 50).unwrap();
    assert_eq!(listed.len(), 1, "a rename appends no marker");
    assert_eq!(listed[0].label.as_deref(), Some("Before the rewrite"));

    // And `None` puts it back to unnamed rather than leaving the old name.
    f.store.relabel_history(ws, marker.id, None).unwrap();
    assert_eq!(
        f.store.history_entries(ws, doc, None, 1).unwrap()[0].label,
        None
    );
}

/// The store's own not-found code, not a storage failure — so the route above
/// answers with `history_entry_not_found` like every other missing marker.
#[test]
fn renaming_a_marker_that_is_not_there_is_the_stores_not_found() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    assert_eq!(
        f.store
            .relabel_history(ws, HistoryEntryId::new(), Some("nowhere"))
            .unwrap_err()
            .code(),
        Code::HistoryEntryNotFound
    );
}

/// A marker from one workspace is not renameable through another: the lookup is
/// scoped to the repository that workspace owns, the same property
/// `history_version` has.
#[test]
fn a_marker_is_not_renameable_through_another_workspace() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let other = f.a_workspace();
    let (doc, live) = a_document_with_a_replica(&mut f, ws);
    let mut sent = live.oplog_vv();
    drip(&mut f, ws, doc, &live, &mut sent, "mine", 2);
    let marker = manual_marker(&mut f, ws, doc, 3);
    assert_eq!(
        f.store
            .relabel_history(other, marker.id, Some("theirs"))
            .unwrap_err()
            .code(),
        Code::HistoryEntryNotFound
    );
}

// ---------------------------------------------------- history: staleness

/// **The whole reason this is asked of the store rather than guessed in a UI.**
///
/// A fresh marker means nothing has changed; one edit means something has; and
/// the answer is the same after a restart — which is exactly what the
/// projection worker's in-memory `AutoHistory` schedule cannot promise.
#[test]
fn staleness_follows_the_document_and_survives_a_restart() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let (doc, live) = a_document_with_a_replica(&mut f, ws);
    let mut sent = live.oplog_vv();

    // No markers at all: there is nothing to be unchanged relative to.
    assert_eq!(f.store.history_staleness(ws, doc).unwrap(), Some(true));

    drip(&mut f, ws, doc, &live, &mut sent, "first draft", 2);
    manual_marker(&mut f, ws, doc, 3);
    assert_eq!(
        f.store.history_staleness(ws, doc).unwrap(),
        Some(false),
        "a marker taken at the current state is the current state"
    );

    drip(&mut f, ws, doc, &live, &mut sent, " and more", 4);
    assert_eq!(
        f.store.history_staleness(ws, doc).unwrap(),
        Some(true),
        "one edit is a change"
    );

    // **The restart case.** The comparison reads the version vector stored
    // beside the marker and the version of the replica this store reopened, so
    // no part of the answer lived in memory.
    f.restart();
    assert_eq!(f.store.history_staleness(ws, doc).unwrap(), Some(true));
    manual_marker(&mut f, ws, doc, 5);
    assert_eq!(f.store.history_staleness(ws, doc).unwrap(), Some(false));
    f.restart();
    assert_eq!(
        f.store.history_staleness(ws, doc).unwrap(),
        Some(false),
        "the answer is a durable comparison, not a remembered one"
    );
}

/// An edit that arrived from somewhere else is still an edit. A schedule kept
/// beside one editing session would call this unchanged.
#[test]
fn a_peers_edit_makes_the_document_stale() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let (doc, live) = a_document_with_a_replica(&mut f, ws);
    let mut sent = live.oplog_vv();
    drip(&mut f, ws, doc, &live, &mut sent, "shared", 2);
    manual_marker(&mut f, ws, doc, 3);
    assert_eq!(f.store.history_staleness(ws, doc).unwrap(), Some(false));

    // A second replica with its own peer id, sending an update the way a linked
    // device does.
    let peer = loro::LoroDoc::new();
    peer.import(&f.store.snapshot(ws, doc).unwrap()).unwrap();
    peer.set_peer_id(77).unwrap();
    let before = peer.oplog_vv();
    peer.get_text("t").insert(0, "from the phone").unwrap();
    peer.commit();
    let update = peer.export(loro::ExportMode::updates(&before)).unwrap();
    f.store.apply_update(ws, doc, update, at(4), None).unwrap();
    f.store.flush(ws, doc, at(4)).unwrap();

    assert_eq!(f.store.history_staleness(ws, doc).unwrap(), Some(true));
}

/// `None` is *the store does not say*, and the manifest is the one document it
/// says nothing about — it has no user history to be stale against.
#[test]
fn the_manifest_has_no_user_history_and_therefore_no_staleness() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let manifest = f.store.manifest_id(ws).unwrap();
    assert_eq!(f.store.history_staleness(ws, manifest).unwrap(), None);
}

// ------------------------------------------- moving a workspace (SP-15a)

/// Two profiles to move a workspace between.
fn two_profiles(f: &mut Fixture) -> (ProfileId, ProfileId) {
    let one = f
        .store
        .create_profile("Work", None, None, Locale::parse("en").unwrap(), at(0))
        .unwrap()
        .id;
    let two = f
        .store
        .create_profile("Home", None, None, Locale::parse("en").unwrap(), at(1))
        .unwrap()
        .id;
    (one, two)
}

/// The workspaces one profile owns, in their saved order.
fn owned(f: &Fixture, owner: ProfileId) -> Vec<WorkspaceId> {
    f.store
        .workspaces()
        .unwrap()
        .into_iter()
        .filter(|workspace| workspace.owner == owner)
        .map(|workspace| workspace.id)
        .collect()
}

/// **The whole of what a move is**: the owner changes, and nothing else about
/// the workspace does. The id is the same id, so every document and every room
/// name is still reachable under it.
#[test]
fn moving_a_workspace_changes_its_owner_and_nothing_else() {
    let mut f = Fixture::new();
    let (from, to) = two_profiles(&mut f);
    let before = f.store.create_workspace(from, "Notes", at(2)).unwrap();
    let (document, _) = f
        .store
        .create_document(before.id, "Roadmap", "page", None, None, at(2))
        .unwrap();

    let after = f.store.move_workspace(before.id, to).unwrap();

    assert_eq!(after.id, before.id, "a move mints no new id");
    assert_eq!(after.owner, to);
    assert_eq!(after.name, before.name);
    assert_eq!(after.created_at, before.created_at);
    assert_eq!(after.icon, before.icon);
    assert_eq!(after.color, before.color);
    assert_eq!(after.pinned, before.pinned);
    assert_eq!(after.archived, before.archived);
    assert_eq!(after.deleted_at, before.deleted_at);
    assert_eq!(
        f.store.locate(document).unwrap(),
        Some(before.id),
        "a document still lives where it lived"
    );
}

/// It survives a restart, because it is a committed row and not a decision this
/// process is holding.
#[test]
fn a_move_is_durable() {
    let mut f = Fixture::new();
    let (from, to) = two_profiles(&mut f);
    let id = f.store.create_workspace(from, "Notes", at(2)).unwrap().id;
    f.store.move_workspace(id, to).unwrap();
    f.restart();
    assert_eq!(f.store.workspace(id).unwrap().owner, to);
}

/// **It arrives last in its new home, and the old home keeps its order.** The
/// destination's existing workspaces do not shuffle, and the source's remaining
/// ones stay in the sequence they were in — a gap in the numbers is not a gap
/// in the order.
#[test]
fn a_moved_workspace_lands_at_the_end_of_its_new_owners_order() {
    let mut f = Fixture::new();
    let (from, to) = two_profiles(&mut f);
    let first = f.store.create_workspace(from, "First", at(2)).unwrap().id;
    let travelling = f.store.create_workspace(from, "Second", at(3)).unwrap().id;
    let third = f.store.create_workspace(from, "Third", at(4)).unwrap().id;
    let settled = f.store.create_workspace(to, "Already", at(5)).unwrap().id;

    f.store.move_workspace(travelling, to).unwrap();

    assert_eq!(owned(&f, to), vec![settled, travelling]);
    assert_eq!(owned(&f, from), vec![first, third]);
}

/// And the source's order is still replaceable afterwards, which is the
/// invariant a gap could have broken: `reorder_workspaces` takes the complete
/// set of what the profile owns, and the moved one is no longer in it.
#[test]
fn the_old_owners_order_can_still_be_replaced_after_a_move() {
    let mut f = Fixture::new();
    let (from, to) = two_profiles(&mut f);
    let first = f.store.create_workspace(from, "First", at(2)).unwrap().id;
    let travelling = f.store.create_workspace(from, "Second", at(3)).unwrap().id;
    let third = f.store.create_workspace(from, "Third", at(4)).unwrap().id;
    f.store.move_workspace(travelling, to).unwrap();

    f.store.reorder_workspaces(from, &[third, first]).unwrap();
    assert_eq!(owned(&f, from), vec![third, first]);

    // Naming the departed one is now a lie about what this profile owns.
    assert_eq!(
        f.store
            .reorder_workspaces(from, &[third, first, travelling])
            .unwrap_err()
            .code,
        Code::InvalidRequest
    );
}

/// **The same move twice is one move.** The second is not a second trip to the
/// end of the list, which is what makes a retry over current ownership safe: a
/// replay after the first attempt committed changes nothing at all.
#[test]
fn moving_a_workspace_to_the_profile_that_already_owns_it_changes_nothing() {
    let mut f = Fixture::new();
    let (from, to) = two_profiles(&mut f);
    let settled = f.store.create_workspace(to, "Already", at(2)).unwrap().id;
    let travelling = f.store.create_workspace(from, "Notes", at(3)).unwrap().id;

    let once = f.store.move_workspace(travelling, to).unwrap();
    let twice = f.store.move_workspace(travelling, to).unwrap();

    assert_eq!(
        once.position, twice.position,
        "a replay does not re-place it"
    );
    assert_eq!(owned(&f, to), vec![settled, travelling]);
}

/// A target that does not exist is refused by name, before the row is touched.
#[test]
fn moving_a_workspace_to_a_profile_that_does_not_exist_is_refused() {
    let mut f = Fixture::new();
    let (from, _) = two_profiles(&mut f);
    let id = f.store.create_workspace(from, "Notes", at(2)).unwrap().id;

    assert_eq!(
        f.store
            .move_workspace(id, ProfileId::new())
            .unwrap_err()
            .code,
        Code::ProfileNotFound
    );
    assert_eq!(
        f.store.workspace(id).unwrap().owner,
        from,
        "a refused move moves nothing"
    );
}

/// A workspace that was never here, and one that is in the trash, answer the
/// same code — and the trashed one says which it is.
#[test]
fn moving_a_workspace_that_is_absent_or_trashed_is_refused() {
    let mut f = Fixture::new();
    let (from, to) = two_profiles(&mut f);

    let absent = f.store.move_workspace(WorkspaceId::new(), to).unwrap_err();
    assert_eq!(absent.code, Code::WorkspaceNotFound);

    let id = f.store.create_workspace(from, "Notes", at(2)).unwrap().id;
    f.store.delete_workspace(id, at(3)).unwrap();
    let trashed = f.store.move_workspace(id, to).unwrap_err();
    assert_eq!(trashed.code, Code::WorkspaceNotFound);
    assert!(
        trashed.message().contains("trash"),
        "the message says which of the two it is: {}",
        trashed.message()
    );
    assert_eq!(
        f.store.workspace_including_deleted(id).unwrap().owner,
        from,
        "a workspace in the trash keeps the owner it went in with"
    );
}

/// **Open documents do not stop a move**, which is the one behaviour that
/// separates it from a soft delete. Nothing is disconnected, so there is
/// nothing for an open document to be wrong about afterwards.
#[test]
fn a_move_does_not_care_whether_documents_are_open() {
    let mut f = Fixture::new();
    let (from, to) = two_profiles(&mut f);
    let id = f.store.create_workspace(from, "Notes", at(2)).unwrap().id;
    let (document, _) = f
        .store
        .create_document(id, "Roadmap", "page", None, None, at(2))
        .unwrap();
    f.store.snapshot(id, document).unwrap();

    f.store.move_workspace(id, to).unwrap();

    assert!(
        f.store.snapshot(id, document).is_ok(),
        "the document is still readable through the same ids"
    );
}

// ------------------------------------------------------------- file notes

/// A file in the store, as the import path registers one.
fn import_file(store: &mut TestStore, ws: WorkspaceId, title: &str) -> DocumentId {
    let doc = loro::LoroDoc::new();
    doc.commit();
    let snapshot = doc.export(loro::ExportMode::Snapshot).unwrap();
    store
        .import_document_at_with_mime(
            ws,
            DocumentId::new(),
            title,
            "file",
            Some("application/pdf"),
            None,
            None,
            snapshot,
            at(1),
        )
        .unwrap()
        .0
}

/// **One note per file, however many times it is asked for.** The second call
/// returns the first call's note and registers nothing new.
#[test]
fn ensure_file_note_creates_once_and_is_idempotent() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let file = import_file(&mut f.store, ws, "Report.pdf");

    let ensured = f.store.ensure_file_note(ws, file, at(2)).unwrap();
    assert!(ensured.created, "the first ensure created the note");
    assert!(!ensured.delta.is_empty(), "a creation changed the manifest");
    let note = ensured.id;
    let again = f.store.ensure_file_note(ws, file, at(3)).unwrap();
    assert_eq!(again.id, note, "a second ensure minted a second note");
    assert!(!again.created, "the idempotent path created nothing");
    assert!(
        again.delta.is_empty(),
        "the idempotent path changed nothing"
    );

    // `documents` reads the SQL mirror, not the manifest — so this is also
    // the proof that the reindex wrote note_doc.
    let docs = f.store.documents(ws).unwrap();
    assert_eq!(docs.len(), 2, "file plus its note, and no third row");
    let file_entry = docs.iter().find(|e| e.id == file).unwrap();
    assert_eq!(file_entry.note, Some(note));
    let note_entry = docs.iter().find(|e| e.id == note).unwrap();
    assert_eq!(note_entry.kind, "note");
    assert_eq!(note_entry.parent, Some(file));
    assert_eq!(note_entry.title, "Report.pdf");

    // The link survives a restart: the mirror column is durable, not derived
    // on open.
    f.restart();
    let docs = f.store.documents(ws).unwrap();
    assert_eq!(
        docs.iter().find(|e| e.id == file).and_then(|e| e.note),
        Some(note)
    );
}

#[test]
fn ensure_file_note_refuses_anything_but_a_file() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let (page, _) = f
        .store
        .create_document(ws, "Roadmap", "page", None, None, at(1))
        .unwrap();

    let error = f.store.ensure_file_note(ws, page, at(2)).unwrap_err();
    assert_eq!(error.code(), Code::InvalidRequest);

    let error = f
        .store
        .ensure_file_note(ws, DocumentId::new(), at(2))
        .unwrap_err();
    assert_eq!(error.code(), Code::DocumentNotFound);
}

/// A note's manifest title is the file's title, and stays it: renaming the
/// file renames the note in the same operation, so a tab showing the note
/// keeps naming the file it belongs to.
#[test]
fn renaming_a_file_renames_its_note() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let file = import_file(&mut f.store, ws, "Report.pdf");
    let note = f.store.ensure_file_note(ws, file, at(2)).unwrap().id;

    f.store
        .rename_document_entry(ws, file, "Final report.pdf", at(3))
        .unwrap();

    let docs = f.store.documents(ws).unwrap();
    assert_eq!(
        docs.iter().find(|e| e.id == note).unwrap().title,
        "Final report.pdf"
    );
}
