// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_core::errors::{Code, Result};
use koru_store_core::service::Backend;
use koru_store_core::{ProfileAccountBinding, ProfileId};

struct NoWorkspaceIo;
impl Backend for NoWorkspaceIo {
    type Workspace = crate::SqliteWorkspace;
    type Blobs = crate::blobs::SqliteBlobs;
    fn open_workspace(&self, _: WorkspaceId) -> Result<Self::Workspace> {
        panic!("Profile provisioning never opens a Workspace")
    }
    fn delete_workspace(&self, _: WorkspaceId) -> Result<()> {
        panic!("Profile provisioning never deletes a Workspace")
    }
    fn objects(&self) -> Result<Self::Blobs> {
        panic!("Profile provisioning never opens blobs")
    }
}
fn binding(n: u8) -> ProfileAccountBinding {
    ProfileAccountBinding::parse_identity_id(&format!("10000000-0000-4000-8000-{n:012}")).unwrap()
}

#[test]
fn legacy_upgrade_starts_private_empty_and_late_legacy_import_cannot_reseed_identity() {
    use koru_store_core::ports::Catalog;
    use koru_store_core::profile_manifest::ProfileManifest;
    use koru_store_core::profile_units::ProfileUnitRef;
    let mut catalog = SqliteCatalog::in_memory().unwrap();
    let profile = koru_store_core::Profile::new("Old", Locale::default(), at()).unwrap();
    let legacy = ProfileManifest::create(&profile).unwrap();
    legacy.add_dictionary_word("en", "legacy-secret").unwrap();
    catalog
        .commit_profile_crdt(&profile, &legacy.export_snapshot().unwrap())
        .unwrap();
    let mut store = Store::open(catalog, NoWorkspaceIo).unwrap();
    assert!(store.personal_dictionary(profile.id).unwrap().is_empty());
    store.rename_profile(profile.id, "Fresh name").unwrap();
    store
        .set_dictionary_word(profile.id, "en", "fresh", true)
        .unwrap();
    let version = store.profile_version(profile.id).unwrap();
    legacy.set_name("Obsolete remote name").unwrap();
    store
        .apply_profile_unit_updates(
            ProfileUnitRef::LegacyPrivate {
                profile: profile.id,
            },
            &[legacy.export_snapshot().unwrap()],
        )
        .unwrap();
    assert_eq!(store.profile(profile.id).unwrap().name, "Fresh name");
    assert_eq!(store.profile_version(profile.id).unwrap(), version);
    assert_eq!(
        store.personal_dictionary(profile.id).unwrap()["en"],
        vec!["fresh"]
    );
    assert_eq!(store.open_workspaces(), 0);
}

#[test]
fn reopened_catalog_only_profile_never_synthesizes_lineage() {
    use koru_store_core::ports::Catalog;
    let profile = koru_store_core::Profile::new("Discovered", Locale::default(), at()).unwrap();
    for _restart in 0..2 {
        let mut catalog = SqliteCatalog::in_memory().unwrap();
        catalog.put_profile(&profile).unwrap();
        let mut store = Store::open(catalog, NoWorkspaceIo).unwrap();
        assert_eq!(
            store.profile_version(profile.id).unwrap_err().code,
            Code::VersionUnavailable
        );
        assert_eq!(
            store.reminder_settings(profile.id).unwrap_err().code,
            Code::ProfilePrivateUnavailable
        );
        assert!(store.profile_unit_states(profile.id).unwrap().is_empty());
    }
}

#[test]
fn closed_workspace_index_repairs_by_source_version_and_never_reports_missing_as_empty() {
    use koru_store_core::ports::Catalog;
    use koru_store_core::profile_units::{IdentityManifest, PrivateManifest, ProfileUnitRef};
    use koru_store_core::{
        DocumentId, EntityRef, FloatingDateTime, OneTimeSchedule, ReminderId, ReminderRule,
        RuleRevisionId, RuleStatus,
    };
    let mut catalog = SqliteCatalog::in_memory().unwrap();
    let profile = koru_store_core::Profile::new("Owner", Locale::default(), at()).unwrap();
    let other = koru_store_core::Profile::new("Other account", Locale::default(), at()).unwrap();
    for row in [&profile, &other] {
        catalog
            .initialize_profile_units(
                row,
                &IdentityManifest::create(row)
                    .unwrap()
                    .export_snapshot()
                    .unwrap(),
                &PrivateManifest::create(row.id)
                    .unwrap()
                    .export_snapshot()
                    .unwrap(),
            )
            .unwrap();
    }
    // Personal data is v2 and follows the Workspace's current placement: the
    // Profile the Workspace is placed under sees its reminders, any other
    // Profile of the same user sees none of them.
    let workspace = koru_store_core::Workspace::new(profile.id, "Closed", at()).unwrap();
    catalog.put_workspace(&workspace).unwrap();
    let mut store = Store::open(catalog, NoWorkspaceIo).unwrap();
    let rule = ReminderRule {
        id: ReminderId::new(),
        target: EntityRef::new(
            "task",
            workspace.id,
            DocumentId::new(),
            DocumentId::new().as_uuid(),
        )
        .unwrap(),
        revision: RuleRevisionId::new(),
        status: RuleStatus::Enabled,
        schedule: OneTimeSchedule::Floating {
            wall: FloatingDateTime::parse("2026-09-12T10:00:00").unwrap(),
        },
        tone_override: None,
    };
    store.create_reminder(profile.id, &rule).unwrap();
    assert_eq!(
        store
            .reminder_reduction_inputs(profile.id)
            .unwrap_err()
            .code,
        Code::ReminderIndexUpdating
    );
    assert!(store.reminders(other.id).unwrap().is_empty());
    assert!(
        store
            .reminder_index_candidates(other.id)
            .unwrap()
            .is_empty()
    );
    let source = store
        .reminder_index_candidates(profile.id)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        source.unit,
        ProfileUnitRef::Personal {
            owner: profile.id,
            workspace: workspace.id
        }
    );
    let (version, rows) = koru_store_core::service::build_reminder_index(&source).unwrap();
    assert!(
        store
            .publish_reminder_index(&source, &version.encode(), &rows)
            .unwrap()
    );
    assert_eq!(
        store.reminder_reduction_inputs(profile.id).unwrap().len(),
        1
    );
    store
        .set_reminder_status_in(
            profile.id,
            workspace.id,
            rule.id,
            RuleRevisionId::new(),
            RuleStatus::Paused,
        )
        .unwrap();
    assert!(
        !store
            .publish_reminder_index(&source, &version.encode(), &rows)
            .unwrap()
    );
    assert_eq!(
        store
            .reminder_reduction_inputs(profile.id)
            .unwrap_err()
            .code,
        Code::ReminderIndexUpdating
    );
    assert_eq!(store.open_workspaces(), 0);
}

#[test]
fn initial_profile_waits_for_received_unit_lineages_before_editing() {
    let mut store = Store::open(SqliteCatalog::in_memory().unwrap(), NoWorkspaceIo).unwrap();
    let id = ProfileId::new();
    let profile = store
        .provision_account_profile(id, binding(1), "Work", Locale::default(), at())
        .unwrap();
    assert_eq!(profile.id, id);
    assert_eq!(profile.account, Some(binding(1).to_storage()));
    assert_eq!(
        store.profile_version(id).unwrap_err().code,
        Code::VersionUnavailable
    );
    assert_eq!(
        store.rename_profile(id, "too early").unwrap_err().code,
        Code::VersionUnavailable
    );
    assert_eq!(
        store.reminder_settings(id).unwrap_err().code,
        Code::ProfilePrivateUnavailable
    );
    assert_eq!(store.profiles().unwrap().len(), 1);
    assert_eq!(store.open_workspaces(), 0);
    use koru_store_core::profile_units::{IdentityManifest, PrivateManifest, ProfileUnitRef};
    let identity = IdentityManifest::create(&profile).unwrap();
    let private = PrivateManifest::create(id).unwrap();
    store
        .apply_profile_unit_updates(
            ProfileUnitRef::Identity { profile: id },
            &[identity.export_snapshot().unwrap()],
        )
        .unwrap();
    store
        .apply_profile_unit_updates(
            ProfileUnitRef::Private { profile: id },
            &[private.export_snapshot().unwrap()],
        )
        .unwrap();
    assert_eq!(store.profile_version(id).unwrap(), identity.version());
    store.rename_profile(id, "My local name").unwrap();
    let replay = store
        .provision_account_profile(
            id,
            binding(1),
            "Old discovery name",
            Locale::default(),
            at(),
        )
        .unwrap();
    assert_eq!(replay.name, "My local name");
}

#[test]
fn retries_never_rebind_existing_profiles_or_restore_removed_ones() {
    let mut store = Store::open(SqliteCatalog::in_memory().unwrap(), NoWorkspaceIo).unwrap();
    let offline = store
        .create_profile("Offline", None, None, Locale::default(), at())
        .unwrap();
    assert_eq!(
        store
            .provision_account_profile(offline.id, binding(1), "Work", Locale::default(), at())
            .unwrap_err()
            .code,
        Code::ProfileAccountConflict
    );
    let id = ProfileId::new();
    store
        .provision_account_profile(id, binding(1), "Work", Locale::default(), at())
        .unwrap();
    assert_eq!(
        store
            .provision_account_profile(id, binding(2), "Other", Locale::default(), at())
            .unwrap_err()
            .code,
        Code::ProfileAccountConflict
    );
    let mark = store
        .begin_profile_removal(
            id,
            koru_store_core::profile_removal::RemovalKind::Local,
            at(),
        )
        .unwrap();
    store
        .remove_profile_local(id, Some(mark.attempt), at())
        .unwrap();
    assert_eq!(
        store
            .provision_account_profile(id, binding(1), "Work", Locale::default(), at())
            .unwrap_err()
            .code,
        Code::ProfileRemoved
    );
    assert_eq!(store.profile(id).unwrap_err().code, Code::ProfileNotFound);
}
