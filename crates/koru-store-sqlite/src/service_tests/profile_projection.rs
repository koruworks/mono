// SPDX-License-Identifier: MPL-2.0

//! Real in-memory catalog coverage for the account-placeholder/P2P failure.
use super::*;
use koru_store_core::errors::{Code, Result};
use koru_store_core::ports::Catalog;
use koru_store_core::profile_units::{IdentityManifest, PrivateManifest, ProfileUnitRef};
use koru_store_core::service::Backend;
use koru_store_core::{DocumentId, Profile, ProfileAccountBinding, Workspace};

struct MemoryBackend;
impl Backend for MemoryBackend {
    type Workspace = crate::SqliteWorkspace;
    type Blobs = crate::blobs::SqliteBlobs;
    fn open_workspace(&self, _: WorkspaceId) -> Result<Self::Workspace> {
        crate::SqliteWorkspace::in_memory()
    }
    fn create_workspace(&self, id: WorkspaceId) -> Result<Self::Workspace> {
        self.open_workspace(id)
    }
    fn delete_workspace(&self, _: WorkspaceId) -> Result<()> {
        panic!("projection repair never deletes a Workspace")
    }
    fn objects(&self) -> Result<Self::Blobs> {
        panic!("projection repair never opens blobs")
    }
}

fn identity() -> (Profile, Vec<u8>) {
    let mut profile =
        Profile::new("Original", Locale::default(), Timestamp::from_millis(42)).unwrap();
    profile.account = Some(binding().to_storage());
    let bytes = IdentityManifest::create(&profile)
        .unwrap()
        .export_snapshot()
        .unwrap();
    (profile, bytes)
}

fn binding() -> ProfileAccountBinding {
    ProfileAccountBinding::parse_identity_id("10000000-0000-4000-8000-000000000001").unwrap()
}

#[test]
fn identity_import_replaces_placeholder_time_and_allows_peer_workspace_provisioning() {
    let (profile, bytes) = identity();
    let mut store = Store::open(SqliteCatalog::in_memory().unwrap(), MemoryBackend).unwrap();
    store
        .provision_account_profile(
            profile.id,
            binding(),
            "Placeholder",
            Locale::default(),
            at(),
        )
        .unwrap();
    store
        .apply_profile_unit_updates(
            ProfileUnitRef::Identity {
                profile: profile.id,
            },
            &[bytes],
        )
        .unwrap();
    assert_eq!(
        store.profile(profile.id).unwrap().created_at,
        profile.created_at
    );
    store
        .provision_replica(
            &profile,
            &Workspace::new(profile.id, "Peer Workspace", at()).unwrap(),
            DocumentId::new(),
            at(),
        )
        .unwrap();
    let mut conflicting = profile.clone();
    conflicting.created_at = at();
    assert_eq!(
        store
            .provision_replica(
                &conflicting,
                &Workspace::new(profile.id, "Conflict", at()).unwrap(),
                DocumentId::new(),
                at(),
            )
            .unwrap_err()
            .code,
        Code::ProfileExists
    );
}

#[test]
fn initialization_projects_the_original_time_over_a_placeholder() {
    let (profile, bytes) = identity();
    let mut catalog = SqliteCatalog::in_memory().unwrap();
    let mut placeholder = profile.clone();
    placeholder.created_at = at();
    catalog.put_profile(&placeholder).unwrap();
    let private = PrivateManifest::create(profile.id)
        .unwrap()
        .export_snapshot()
        .unwrap();
    catalog
        .initialize_profile_units(&profile, &bytes, &private)
        .unwrap();
    assert_eq!(
        catalog.profile(profile.id).unwrap().unwrap().created_at,
        profile.created_at
    );
}

#[test]
fn restart_repairs_equal_version_identity_without_rewriting_bytes_or_restoring_removed_profiles() {
    for removed in [false, true] {
        let db = format!("profile-projection-{}", DocumentId::new().to_storage());
        let mut keeper = SqliteCatalog::shared_in_memory(&db).unwrap();
        let (profile, bytes) = identity();
        let mut stale = profile.clone();
        stale.created_at = at();
        // Model the old writer: correct retained CRDT, stale listing timestamp.
        keeper.commit_profile_identity(&stale, &bytes).unwrap();
        if removed {
            let mark = keeper
                .update_profile_removal(
                    profile.id,
                    koru_store_core::profile_removal::RemovalUpdate::Begin {
                        kind: koru_store_core::profile_removal::RemovalKind::Local,
                        at: at(),
                    },
                )
                .unwrap()
                .unwrap();
            keeper
                .remove_profile_local(profile.id, Some(mark.attempt), at())
                .unwrap();
        }
        let before = keeper.profile_units(profile.id).unwrap();
        for _ in 0..2 {
            let store =
                Store::open(SqliteCatalog::shared_in_memory(&db).unwrap(), MemoryBackend).unwrap();
            assert_eq!(store.open_workspaces(), 0);
            assert_eq!(
                keeper
                    .profile_including_removed(profile.id)
                    .unwrap()
                    .unwrap(),
                profile
            );
            assert_eq!(keeper.profile(profile.id).unwrap().is_none(), removed);
            assert_eq!(keeper.profile_units(profile.id).unwrap(), before);
        }
    }
}

#[test]
fn restart_leaves_missing_corrupt_foreign_and_future_identities_untouched() {
    for case in ["missing", "corrupt", "foreign", "future"] {
        let db = format!(
            "profile-projection-refusal-{}",
            DocumentId::new().to_storage()
        );
        let mut keeper = SqliteCatalog::shared_in_memory(&db).unwrap();
        let (profile, bytes) = identity();
        let mut placeholder = profile.clone();
        placeholder.created_at = at();
        let snapshot = match case {
            "missing" => None,
            "corrupt" => Some(b"invalid identity".to_vec()),
            "foreign" => Some(identity().1),
            "future" => {
                let doc = loro::LoroDoc::new();
                doc.import(&bytes).unwrap();
                doc.get_map("meta").insert("schema_version", 99).unwrap();
                doc.commit();
                Some(doc.export(loro::ExportMode::Snapshot).unwrap())
            }
            _ => unreachable!(),
        };
        match snapshot {
            Some(snapshot) => keeper
                .commit_profile_identity(&placeholder, &snapshot)
                .unwrap(),
            None => keeper.put_profile(&placeholder).unwrap(),
        }
        let before = keeper.profile_units(profile.id).unwrap();
        let store =
            Store::open(SqliteCatalog::shared_in_memory(&db).unwrap(), MemoryBackend).unwrap();
        assert_eq!(store.profile(profile.id).unwrap(), placeholder, "{case}");
        assert_eq!(keeper.profile_units(profile.id).unwrap(), before, "{case}");
        assert_eq!(store.open_workspaces(), 0);
    }
}
