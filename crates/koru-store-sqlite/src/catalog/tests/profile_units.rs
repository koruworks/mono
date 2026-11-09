// SPDX-License-Identifier: MPL-2.0

use super::*;

/// A v1 personal address. No row is stored under it since catalog 13; the
/// tests below pin what the legacy methods answer about it, and
/// `tests/root_catalog.rs` covers the v2 rows that replaced it.
fn personal(c: &mut SqliteCatalog, owner: ProfileId) -> ProfileUnitRef {
    let workspace = Workspace::new(owner, "Workspace", at()).unwrap();
    c.put_workspace(&workspace).unwrap();
    ProfileUnitRef::Personal {
        owner,
        workspace: workspace.id,
    }
}

#[test]
fn schema_ten_upgrade_creates_units_without_seeding_or_touching_legacy() {
    let mut connection = db::open_in_memory().unwrap();
    db::migrate(&mut connection, "catalog.db", &MIGRATIONS[..10], 10).unwrap();
    let profile = Profile::new("Existing", Locale::default(), at()).unwrap();
    legacy_profile_row(&connection, &profile);
    connection
        .execute(
            "INSERT INTO profile_crdts VALUES (?1, ?2)",
            params![profile.id.to_storage(), b"legacy-history".as_slice()],
        )
        .unwrap();
    let mut c = SqliteCatalog::from_connection(connection).unwrap();
    assert_eq!(c.schema_version().unwrap(), CATALOG_SCHEMA_VERSION);
    assert_eq!(
        c.profile_crdt(profile.id).unwrap().unwrap().snapshot,
        b"legacy-history"
    );
    assert_eq!(c.profile_units(profile.id).unwrap().len(), 1);
    assert!(c.profile_reminder_due(profile.id).unwrap().is_empty());
    // Schema 11 created four tables; catalog 13 then replaced the two
    // Profile-keyed personal ones with the Workspace-keyed v2 rows.
    let tables: i64 = c
        .connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN
         ('profile_identity_crdts', 'profile_private_crdts')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 2);
    let legacy: i64 = c
        .connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name IN
         ('profile_personal_crdts', 'profile_reminder_due', 'profile_reminder_due_by_time')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(legacy, 0);
    let index: String = c
        .connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'workspace_reminder_due_by_time'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(index.contains("user_scope_id, due_at_ms, workspace_id, reminder_id"));
    assert_eq!(
        db::migrate(&mut c.connection, "catalog.db", &MIGRATIONS[..10], 10)
            .unwrap_err()
            .code,
        Code::SchemaTooNew
    );
    assert_eq!(c.profile(profile.id).unwrap(), Some(profile));
}

#[test]
fn initialization_is_atomic_and_retries_never_replace_retained_authority() {
    let mut c = catalog();
    let mut profile = Profile::new("Fresh", Locale::default(), at()).unwrap();
    assert!(
        c.initialize_profile_units(&profile, b"identity", b"")
            .is_err()
    );
    assert!(c.profile(profile.id).unwrap().is_none());
    assert!(c.profile_units(profile.id).unwrap().is_empty());
    c.initialize_profile_units(&profile, b"identity", b"private")
        .unwrap();
    let original = c.profile_units(profile.id).unwrap();
    profile.name = "Stale candidate".into();
    c.initialize_profile_units(&profile, b"other-identity", b"other-private")
        .unwrap();
    assert_eq!(c.profile_units(profile.id).unwrap(), original);
    assert_eq!(c.profile(profile.id).unwrap().unwrap().name, "Fresh");
    assert!(c.profile_crdt(profile.id).unwrap().is_none());
}

#[test]
fn initialization_preserves_private_and_refuses_missing_private_after_identity() {
    let mut c = catalog();
    let profile = a_profile(&mut c);
    let private = ProfileUnitRef::Private {
        profile: profile.id,
    };
    c.commit_profile_unit(private, b"retained-private").unwrap();
    c.initialize_profile_units(&profile, b"identity", b"candidate-private")
        .unwrap();
    assert_eq!(
        c.profile_unit(private).unwrap().unwrap().snapshot,
        b"retained-private"
    );
    // Model a provisioning gap; never invent a new lineage once identity exists.
    c.connection
        .execute(
            "DELETE FROM profile_private_crdts WHERE profile_id = ?1",
            params![profile.id.to_storage()],
        )
        .unwrap();
    assert_eq!(
        c.initialize_profile_units(&profile, b"candidate", b"candidate")
            .unwrap_err()
            .code,
        Code::CorruptState
    );
    assert!(c.profile_unit(private).unwrap().is_none());
}

#[test]
fn identity_projection_preserves_lifecycle_and_rolls_back_with_snapshot() {
    let mut c = catalog();
    let mut profile = Profile::holder(Locale::default(), at()).unwrap();
    c.commit_profile_crdt(&profile, b"legacy").unwrap();
    let attempt = super::begin_removal(&mut c, profile.id);
    c.remove_profile_local(profile.id, Some(attempt), at())
        .unwrap();
    c.initialize_profile_units(&profile, b"identity", b"private")
        .unwrap();
    let created_at = profile.created_at;
    profile.name = "Updated".into();
    profile.kind = ProfileKind::Normal;
    profile.created_at = Timestamp::from_millis(42);
    assert!(c.commit_profile_identity(&profile, b"").is_err());
    assert_ne!(
        c.profile_including_removed(profile.id)
            .unwrap()
            .unwrap()
            .name,
        "Updated"
    );
    assert_eq!(
        c.profile_including_removed(profile.id)
            .unwrap()
            .unwrap()
            .created_at,
        created_at,
        "a failed identity commit must roll back the timestamp too"
    );
    c.commit_profile_identity(&profile, b"updated-identity")
        .unwrap();
    let stored = c.profile_including_removed(profile.id).unwrap().unwrap();
    assert_eq!(stored.name, "Updated");
    assert!(stored.is_holder());
    assert_eq!(stored.created_at, profile.created_at);
    assert!(c.profile(profile.id).unwrap().is_none());
    assert_eq!(
        c.profile_crdt(profile.id).unwrap().unwrap().snapshot,
        b"legacy"
    );
}

#[test]
fn timestamp_repair_compares_source_bytes_and_changes_no_other_projection_fields() {
    let mut c = catalog();
    let profile = a_profile(&mut c);
    c.commit_profile_identity(&profile, b"retained-identity")
        .unwrap();
    let attempt = super::begin_removal(&mut c, profile.id);
    c.remove_profile_local(profile.id, Some(attempt), at())
        .unwrap();
    let original_units = c.profile_units(profile.id).unwrap();
    let original_mode = c
        .transport_mode(TransportSubject::Profile(profile.id))
        .unwrap();
    let corrected = Timestamp::from_millis(42);
    assert!(
        !c.reconcile_profile_creation_time(profile.id, corrected, b"stale")
            .unwrap()
    );
    assert_eq!(
        c.profile_including_removed(profile.id).unwrap().unwrap(),
        profile
    );
    assert!(
        c.reconcile_profile_creation_time(profile.id, corrected, b"retained-identity")
            .unwrap()
    );
    assert!(
        !c.reconcile_profile_creation_time(profile.id, corrected, b"retained-identity")
            .unwrap()
    );
    let mut expected = profile.clone();
    expected.created_at = corrected;
    assert_eq!(
        c.profile_including_removed(profile.id).unwrap().unwrap(),
        expected
    );
    assert!(c.profile(profile.id).unwrap().is_none());
    assert_eq!(c.profile_units(profile.id).unwrap(), original_units);
    assert_eq!(
        c.transport_mode(TransportSubject::Profile(profile.id))
            .unwrap(),
        original_mode
    );
    // A bootstrap/legacy writer cannot undo an identity-authorized correction.
    c.put_profile(&profile).unwrap();
    c.commit_profile_crdt(&profile, b"legacy").unwrap();
    assert_eq!(
        c.profile_including_removed(profile.id)
            .unwrap()
            .unwrap()
            .created_at,
        corrected
    );
}

#[test]
fn profile_addressed_units_stay_distinct_and_the_v1_personal_address_is_honest() {
    let mut c = catalog();
    let profile = a_profile(&mut c);
    let other = a_profile(&mut c);
    let unit = personal(&mut c, profile.id);
    c.initialize_profile_units(&profile, b"identity", b"private")
        .unwrap();
    c.commit_profile_unit(
        ProfileUnitRef::LegacyPrivate {
            profile: profile.id,
        },
        b"legacy",
    )
    .unwrap();
    // Legacy, identity and private are listed under the Profile; no
    // personal unit is, because none is keyed by a Profile any more.
    assert_eq!(c.profile_units(profile.id).unwrap().len(), 3);
    assert_eq!(c.profile_units(other.id).unwrap().len(), 1);
    assert_eq!(c.profile_unit(unit).unwrap(), None);
    // Every v1 personal write names the v2 replacement and writes nothing.
    assert_eq!(
        c.commit_profile_unit(unit, b"personal").unwrap_err().code,
        Code::ProfileUnitScopeMismatch
    );
    assert_eq!(
        c.commit_profile_units(&[(unit, b"personal".to_vec())])
            .unwrap_err()
            .code,
        Code::ProfileUnitScopeMismatch
    );
    assert_eq!(
        c.replace_profile_due_index(
            profile.id,
            unit.workspace().unwrap(),
            b"snapshot",
            b"vector",
            &[]
        )
        .unwrap_err()
        .code,
        Code::ProfileUnitScopeMismatch
    );
    assert!(c.profile_reminder_due(profile.id).unwrap().is_empty());
    assert!(
        c.workspace_personal_unit(c.local_user_scope().unwrap(), unit.workspace().unwrap())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        c.commit_profile_unit(
            ProfileUnitRef::Identity {
                profile: profile.id
            },
            b"bad"
        )
        .unwrap_err()
        .code,
        Code::InvalidRequest
    );
    assert_eq!(c.profile(profile.id).unwrap().unwrap(), profile);
}

#[test]
fn failed_private_commit_keeps_live_cache_and_restart_on_the_committed_lineage() {
    use koru_store_core::profile_units::{IdentityManifest, PrivateManifest};
    use koru_store_core::service::{Backend, Store};
    struct NoWorkspaceIo;
    impl Backend for NoWorkspaceIo {
        type Workspace = crate::SqliteWorkspace;
        type Blobs = crate::blobs::SqliteBlobs;
        fn open_workspace(&self, _: WorkspaceId) -> Result<Self::Workspace> {
            panic!("closed workspace");
        }
        fn delete_workspace(&self, _: WorkspaceId) -> Result<()> {
            panic!("must preflight before deleting");
        }
        fn objects(&self) -> Result<Self::Blobs> {
            panic!("closed blobs");
        }
    }
    // Shared named memory provides a second connection for fault injection and
    // reopening the same committed database; no test opens a filesystem path.
    let profile = Profile::new("Owner", Locale::default(), at()).unwrap();
    let uri = format!("file:profile-unit-{}?mode=memory&cache=shared", profile.id);
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
        | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
        | rusqlite::OpenFlags::SQLITE_OPEN_URI;
    let control = Connection::open_with_flags(&uri, flags).unwrap();
    let mut c =
        SqliteCatalog::from_connection(Connection::open_with_flags(&uri, flags).unwrap()).unwrap();
    c.initialize_profile_units(
        &profile,
        &IdentityManifest::create(&profile)
            .unwrap()
            .export_snapshot()
            .unwrap(),
        &PrivateManifest::create(profile.id)
            .unwrap()
            .export_snapshot()
            .unwrap(),
    )
    .unwrap();
    let workspace = Workspace::new(profile.id, "Retained", at()).unwrap();
    c.put_workspace(&workspace).unwrap();
    let scope = c.local_user_scope().unwrap();
    c.commit_workspace_personal_unit(
        scope,
        workspace.id,
        &koru_store_core::profile_units::PersonalManifest::create_v2(workspace.id, 7)
            .unwrap()
            .export_snapshot()
            .unwrap(),
        7,
    )
    .unwrap();
    c.set_workspace_deleted(workspace.id, Some(Timestamp::from_millis(1)), &[])
        .unwrap();
    let mut store = Store::open(c, NoWorkspaceIo).unwrap();
    assert_eq!(
        store.take_dictionary_committed(),
        [koru_store_core::DictionaryCommitted {
            profile: profile.id
        }]
    );
    let private = ProfileUnitRef::Private {
        profile: profile.id,
    };
    let initial = store.profile_unit_version(private).unwrap();
    control.execute_batch("CREATE TRIGGER reject_private BEFORE UPDATE ON profile_private_crdts BEGIN SELECT RAISE(ABORT, 'injected'); END;").unwrap();
    assert_eq!(
        store
            .update_reminder_settings(
                profile.id,
                Some(Some(koru_store_core::LocalTime::parse("19:00:00").unwrap())),
                None,
                None
            )
            .unwrap_err()
            .code,
        Code::StorageFailed
    );
    assert_eq!(
        store.reminder_settings(profile.id).unwrap(),
        koru_store_core::ReminderSettings::default()
    );
    assert_eq!(
        store
            .set_dictionary_word(profile.id, "en", "retained", true)
            .unwrap_err()
            .code,
        Code::StorageFailed
    );
    assert!(store.personal_dictionary(profile.id).unwrap().is_empty());
    assert_eq!(store.profile_unit_version(private).unwrap(), initial);
    assert!(store.take_reminder_committed().is_empty());
    assert!(store.take_dictionary_committed().is_empty());
    control
        .execute_batch("DROP TRIGGER reject_private;")
        .unwrap();
    store
        .set_dictionary_word(profile.id, "en", "retained", true)
        .unwrap();
    assert_eq!(
        store.take_dictionary_committed(),
        [koru_store_core::DictionaryCommitted {
            profile: profile.id
        }]
    );
    let committed = store.profile_unit_version(private).unwrap();
    assert_ne!(committed, initial);
    let remote = PrivateManifest::from_snapshot(
        &store
            .profile_unit_states(profile.id)
            .unwrap()
            .into_iter()
            .find(|state| state.unit == private)
            .unwrap()
            .snapshot,
    )
    .unwrap();
    remote.set_peer_id(31).unwrap();
    remote.add_dictionary_word("en", "remote").unwrap();
    let remote_updates = remote.export_updates_since(&committed).unwrap();
    store
        .apply_profile_unit_updates(private, std::slice::from_ref(&remote_updates))
        .unwrap();
    assert_eq!(
        store.take_dictionary_committed(),
        [koru_store_core::DictionaryCommitted {
            profile: profile.id
        }]
    );
    assert_eq!(
        store.personal_dictionary(profile.id).unwrap()["en"],
        vec!["remote", "retained"]
    );
    store
        .apply_profile_unit_updates(private, &[remote_updates])
        .unwrap();
    assert!(store.take_dictionary_committed().is_empty());
    let remote_committed = store.profile_unit_version(private).unwrap();
    let identity_before = store.profile_version(profile.id).unwrap();
    control.execute_batch("CREATE TRIGGER reject_identity BEFORE UPDATE ON profile_identity_crdts BEGIN SELECT RAISE(ABORT, 'injected'); END;").unwrap();
    let binding = koru_store_core::ProfileAccountBinding::parse_identity_id(
        "10000000-0000-4000-8000-000000000031",
    )
    .unwrap();
    assert_eq!(
        store
            .bind_profile_account(profile.id, None, binding)
            .unwrap_err()
            .code,
        Code::StorageFailed
    );
    assert_eq!(store.profile(profile.id).unwrap().account, None);
    assert_eq!(
        store
            .rename_profile(profile.id, "Uncommitted")
            .unwrap_err()
            .code,
        Code::StorageFailed
    );
    assert_eq!(store.profile(profile.id).unwrap().name, "Owner");
    assert_eq!(store.profile_version(profile.id).unwrap(), identity_before);
    control
        .execute_batch("DROP TRIGGER reject_identity;")
        .unwrap();
    // The trashed row was written by the legacy projection alone and has no
    // root unit: the purge is driven by unit facts (Plan 1.3), so it finds
    // no candidate, touches no backend bytes and retires no personal row.
    assert_eq!(
        store.purge_deleted(at()).unwrap(),
        koru_store_core::service::PurgeOutcome::default()
    );
    drop(store);
    let mut reopened = Store::open(
        SqliteCatalog::from_connection(control).unwrap(),
        NoWorkspaceIo,
    )
    .unwrap();
    assert_eq!(
        reopened.profile_unit_version(private).unwrap(),
        remote_committed
    );
    assert_eq!(
        reopened.personal_dictionary(profile.id).unwrap()["en"],
        vec!["remote", "retained"]
    );
    assert_eq!(
        reopened.profile_version(profile.id).unwrap(),
        identity_before
    );
}

/// **A Profile identity that arrives from somewhere else announces itself.**
///
/// The desktop's Cloud pass and its peer pass both commit an imported identity
/// unit through `apply_profile_unit_updates` and then told nothing: a rename
/// made on a phone reached the disk and never reached the window, for as long
/// as the window stayed open (D242, for Profiles rather than documents). The
/// diff is made here because here is the only place holding both the committed
/// snapshot and the one it replaced — so the wake can say *which* of the two
/// identity facts moved instead of firing both published events on every
/// import.
#[test]
fn an_imported_identity_wakes_only_the_facts_that_changed() {
    use koru_store_core::profile_units::{IdentityManifest, PrivateManifest};
    use koru_store_core::service::{Backend, Store};
    struct NoWorkspaceIo;
    impl Backend for NoWorkspaceIo {
        type Workspace = crate::SqliteWorkspace;
        type Blobs = crate::blobs::SqliteBlobs;
        fn open_workspace(&self, _: WorkspaceId) -> Result<Self::Workspace> {
            panic!("closed workspace");
        }
        fn delete_workspace(&self, _: WorkspaceId) -> Result<()> {
            panic!("must preflight before deleting");
        }
        fn objects(&self) -> Result<Self::Blobs> {
            panic!("closed blobs");
        }
    }
    let profile = Profile::new("Owner", Locale::default(), at()).unwrap();
    let mut c = catalog();
    c.initialize_profile_units(
        &profile,
        &IdentityManifest::create(&profile)
            .unwrap()
            .export_snapshot()
            .unwrap(),
        &PrivateManifest::create(profile.id)
            .unwrap()
            .export_snapshot()
            .unwrap(),
    )
    .unwrap();
    let mut store = Store::open(c, NoWorkspaceIo).unwrap();
    let unit = ProfileUnitRef::Identity {
        profile: profile.id,
    };
    // Nothing is seeded at open: a rename nobody performed must not be
    // announced merely because a Store restarted.
    assert!(store.take_identity_committed().is_empty());

    let renamed = remote_identity(&store.profile_unit_snapshot(unit).unwrap(), |remote| {
        remote.set_name("Renamed on the phone").unwrap();
    });
    store
        .apply_profile_unit_updates(unit, std::slice::from_ref(&renamed))
        .unwrap();
    assert_eq!(
        store.take_identity_committed(),
        [koru_store_core::IdentityCommitted {
            profile: profile.id,
            change: koru_store_core::IdentityChange::Name,
        }]
    );
    assert_eq!(
        store.profile(profile.id).unwrap().name,
        "Renamed on the phone"
    );

    // Replaying bytes already held changes nothing, so it wakes nothing: a
    // pass that settles a unit without carrying anything new is the ordinary
    // case once two devices agree.
    store.apply_profile_unit_updates(unit, &[renamed]).unwrap();
    assert!(store.take_identity_committed().is_empty());

    let recoloured = remote_identity(&store.profile_unit_snapshot(unit).unwrap(), |remote| {
        remote.set_appearance(Some("🦊"), Some("amber")).unwrap();
    });
    store
        .apply_profile_unit_updates(unit, &[recoloured])
        .unwrap();
    assert_eq!(
        store.take_identity_committed(),
        [koru_store_core::IdentityCommitted {
            profile: profile.id,
            change: koru_store_core::IdentityChange::Appearance,
        }]
    );

    // Both at once is two wakes, not one ambiguous one: `profile.renamed` and
    // `profile.appearance_changed` are two published events.
    let both = remote_identity(&store.profile_unit_snapshot(unit).unwrap(), |remote| {
        remote.set_name("Renamed again").unwrap();
        remote.set_appearance(Some("🐢"), Some("teal")).unwrap();
    });
    store.apply_profile_unit_updates(unit, &[both]).unwrap();
    let mut woke = store.take_identity_committed();
    woke.sort_by_key(|event| event.change);
    assert_eq!(
        woke,
        [
            koru_store_core::IdentityCommitted {
                profile: profile.id,
                change: koru_store_core::IdentityChange::Name,
            },
            koru_store_core::IdentityCommitted {
                profile: profile.id,
                change: koru_store_core::IdentityChange::Appearance,
            },
        ]
    );
}

/// Another device's edit to the same identity unit, as the bytes a peer frame
/// or a Cloud download would hand to `apply_profile_unit_updates`.
fn remote_identity(
    committed: &[u8],
    edit: impl FnOnce(&koru_store_core::profile_units::IdentityManifest),
) -> Vec<u8> {
    let remote =
        koru_store_core::profile_units::IdentityManifest::from_snapshot(committed).unwrap();
    let from = remote.version();
    remote.set_peer_id(31).unwrap();
    edit(&remote);
    remote.export_updates_since(&from).unwrap()
}
