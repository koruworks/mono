// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_core::fixtures::{BIDI_LINE, COMPOSED, DECOMPOSED, SAMPLES};

mod profile_units;
mod root_catalog;
mod root_catalog_faults;
mod transport_modes;

fn at() -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000)
}

fn begin_removal(c: &mut SqliteCatalog, id: ProfileId) -> koru_store_core::ids::RemovalAttemptId {
    c.update_profile_removal(
        id,
        koru_store_core::profile_removal::RemovalUpdate::Begin {
            kind: koru_store_core::profile_removal::RemovalKind::Local,
            at: at(),
        },
    )
    .unwrap()
    .unwrap()
    .attempt
}

/// Write a Profile row the way a database stamped at an **older** schema holds
/// it: the projection columns and nothing else.
///
/// Production's `write_profile_projection` now also seeds the Profile's
/// transport mode row, which is right for a live catalog and wrong for a
/// migration fixture — a table that does not exist yet cannot be written to.
/// A fixture for an old shape has to write the old shape.
fn legacy_profile_row(connection: &Connection, profile: &Profile) {
    connection
        .execute(
            "INSERT INTO profiles
               (id, name, purpose, avatar, account, locale, created_at, icon, color, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                profile.id.to_storage(),
                profile.name,
                profile.purpose,
                profile.avatar,
                profile.account,
                profile.locale.as_str(),
                profile.created_at.millis(),
                profile.icon,
                profile.color,
                profile.kind.as_str(),
            ],
        )
        .unwrap();
}

fn catalog() -> SqliteCatalog {
    SqliteCatalog::in_memory().expect("open")
}

fn a_profile(c: &mut SqliteCatalog) -> Profile {
    let p = Profile::new("Owner", Locale::parse("en").unwrap(), at()).unwrap();
    c.put_profile(&p).unwrap();
    p
}

// ------------------------------------------------------------------ profiles

#[test]
fn a_profile_round_trips() {
    let mut c = catalog();
    let mut p = Profile::new("Trabajo", Locale::parse("es").unwrap(), at()).unwrap();
    p.purpose = Some("work".into());
    c.put_profile(&p).unwrap();
    assert_eq!(c.profile(p.id).unwrap().as_ref(), Some(&p));
}

#[test]
fn a_profile_appearance_round_trips() {
    let mut c = catalog();
    let p = Profile::new("Trabajo", Locale::parse("es").unwrap(), at())
        .unwrap()
        .with_appearance(Some("🧑‍💻".into()), Some("purple".into()));
    c.put_profile(&p).unwrap();
    assert_eq!(c.profile(p.id).unwrap().as_ref(), Some(&p));
}

#[test]
fn a_profile_in_every_supported_language_round_trips_byte_for_byte() {
    let mut c = catalog();
    for s in SAMPLES {
        let p = Profile::new(s.title, s.locale(), at()).unwrap();
        c.put_profile(&p).unwrap();
        let back = c.profile(p.id).unwrap().expect("stored");
        assert_eq!(back.name, s.title, "{}", s.tag);
        assert_eq!(back.locale.as_str(), s.tag, "{}", s.tag);
    }
    assert_eq!(c.profiles().unwrap().len(), SAMPLES.len());
}

#[test]
fn both_unicode_spellings_stay_two_distinct_rows() {
    // Content is not normalised. If SQLite or a writer folded these together,
    // one of the two names would silently become the other.
    let mut c = catalog();
    let a = Profile::new(COMPOSED, Locale::default(), at()).unwrap();
    let b = Profile::new(DECOMPOSED, Locale::default(), at()).unwrap();
    c.put_profile(&a).unwrap();
    c.put_profile(&b).unwrap();
    assert_eq!(c.profile(a.id).unwrap().unwrap().name, COMPOSED);
    assert_eq!(c.profile(b.id).unwrap().unwrap().name, DECOMPOSED);
}

#[test]
fn a_bidi_name_survives_with_its_controls() {
    let mut c = catalog();
    let p = Profile::new(BIDI_LINE, Locale::parse("ar").unwrap(), at()).unwrap();
    c.put_profile(&p).unwrap();
    assert_eq!(c.profile(p.id).unwrap().unwrap().name, BIDI_LINE);
}

#[test]
fn putting_a_profile_twice_updates_rather_than_duplicates() {
    let mut c = catalog();
    let mut p = a_profile(&mut c);
    p.name = "Renamed".into();
    c.put_profile(&p).unwrap();
    assert_eq!(c.profiles().unwrap().len(), 1);
    assert_eq!(c.profile(p.id).unwrap().unwrap().name, "Renamed");
}

#[test]
fn local_profile_removal_requires_no_active_workspaces_and_preserves_tombstones() {
    let mut c = catalog();
    let owner = Profile::new("Owner", Locale::default(), at()).unwrap();
    let other = Profile::new("Other", Locale::default(), at()).unwrap();
    c.commit_profile_crdt(&owner, b"profile-snapshot").unwrap();
    c.put_profile(&other).unwrap();
    let workspace = Workspace::new(owner.id, "Deleted but recoverable", at()).unwrap();
    c.put_workspace(&workspace).unwrap();

    let attempt = begin_removal(&mut c, owner.id);
    assert_eq!(
        c.remove_profile_local(
            owner.id,
            Some(attempt),
            Timestamp::from_millis(at().millis() + 1)
        )
        .unwrap_err()
        .code(),
        Code::ProfileNotEmpty
    );
    c.set_workspace_deleted(
        workspace.id,
        Some(Timestamp::from_millis(at().millis() + 2)),
        &[],
    )
    .unwrap();
    assert_eq!(
        c.remove_profile_local(
            owner.id,
            Some(attempt),
            Timestamp::from_millis(at().millis() + 3)
        )
        .unwrap(),
        LocalProfileRemoval::Removed
    );

    assert_eq!(c.profile(owner.id).unwrap(), None);
    assert_eq!(c.profiles().unwrap(), vec![other.clone()]);
    assert_eq!(
        c.profile_crdt(owner.id).unwrap().unwrap().snapshot,
        b"profile-snapshot"
    );
    let retained = c
        .workspace(workspace.id)
        .unwrap()
        .expect("tombstone retained");
    assert_eq!(retained.owner, owner.id);
    assert!(retained.deleted_at.is_some());
    assert_eq!(
        c.remove_profile_local(
            owner.id,
            Some(attempt),
            Timestamp::from_millis(at().millis() + 4)
        )
        .unwrap_err()
        .code(),
        Code::RemovalAttemptStale
    );
    assert_eq!(
        c.set_workspace_deleted(workspace.id, None, &[])
            .unwrap_err()
            .code(),
        Code::ProfileNotFound
    );
    assert!(
        c.workspace(workspace.id)
            .unwrap()
            .expect("tombstone still retained")
            .deleted_at
            .is_some()
    );
    assert_eq!(
        c.restore_profile_local(owner.id).unwrap(),
        LocalProfileRestoration::Restored
    );
    assert_eq!(c.profile(owner.id).unwrap(), Some(owner.clone()));
    assert_eq!(
        c.restore_profile_local(owner.id).unwrap(),
        LocalProfileRestoration::AlreadyActive
    );
    assert!(
        c.workspace(workspace.id)
            .unwrap()
            .expect("restoring the Profile keeps its Workspace in Trash")
            .deleted_at
            .is_some()
    );
    assert_eq!(c.profile(other.id).unwrap(), Some(other));
}

#[test]
fn removing_the_final_local_profile_leaves_the_visible_catalog_empty() {
    let mut c = catalog();
    let profile = a_profile(&mut c);
    let attempt = begin_removal(&mut c, profile.id);
    assert_eq!(
        c.remove_profile_local(
            profile.id,
            Some(attempt),
            Timestamp::from_millis(at().millis() + 1)
        )
        .unwrap(),
        LocalProfileRemoval::Removed
    );
    // Desktop startup maps this exact empty listing to StoreLaunch::NeedsProfile.
    assert!(c.profiles().unwrap().is_empty());
}

#[test]
fn lifecycle_inventory_is_content_free_and_includes_removed_bound_profiles() {
    let mut c = catalog();
    let identity =
        ProfileAccountBinding::parse_identity_id("10000000-0000-4000-8000-000000000001").unwrap();
    let mut active = Profile::new("Private active name", Locale::default(), at()).unwrap();
    active.account = Some(identity.to_storage());
    let mut removed = Profile::new("Private removed name", Locale::default(), at()).unwrap();
    removed.account = Some(identity.to_storage());
    let unbound = Profile::new("Offline only", Locale::default(), at()).unwrap();
    c.put_profile(&active).unwrap();
    c.put_profile(&removed).unwrap();
    c.put_profile(&unbound).unwrap();
    let removed_at = Timestamp::from_millis(at().millis() + 1);
    let attempt = begin_removal(&mut c, removed.id);
    c.remove_profile_local(removed.id, Some(attempt), removed_at)
        .unwrap();

    let inventory = c.profile_lifecycle_inventory().unwrap();
    assert_eq!(inventory.len(), 2);
    assert_eq!(inventory[0].account_binding, identity);
    assert_eq!(inventory[1].account_binding, identity);
    let active_row = inventory
        .iter()
        .find(|row| row.profile == active.id)
        .unwrap();
    let removed_row = inventory
        .iter()
        .find(|row| row.profile == removed.id)
        .unwrap();
    assert_eq!(active_row.removed_at, None);
    assert_eq!(removed_row.removed_at, Some(removed_at));
    assert!(inventory.iter().all(|row| row.profile != unbound.id));
    let visible = c.profiles().unwrap();
    assert_eq!(visible.len(), 2);
    assert!(visible.iter().any(|profile| profile.id == active.id));
    assert!(visible.iter().any(|profile| profile.id == unbound.id));
    assert!(visible.iter().all(|profile| profile.id != removed.id));
}

#[test]
fn lifecycle_inventory_rejects_a_corrupt_stored_account_binding() {
    let mut c = catalog();
    let profile = a_profile(&mut c);
    c.connection
        .execute(
            "UPDATE profiles SET account = 'google:subject' WHERE id = ?1",
            params![profile.id.to_storage()],
        )
        .unwrap();

    assert_eq!(
        c.profile_lifecycle_inventory().unwrap_err().code(),
        Code::CorruptState
    );
}

#[test]
fn a_removed_profile_cannot_be_used_as_an_implicit_workspace_owner() {
    let mut c = catalog();
    let profile = a_profile(&mut c);
    let attempt = begin_removal(&mut c, profile.id);
    c.remove_profile_local(
        profile.id,
        Some(attempt),
        Timestamp::from_millis(at().millis() + 1),
    )
    .unwrap();
    let workspace = Workspace::new(profile.id, "Must not silently restore", at()).unwrap();
    assert_eq!(
        c.put_workspace(&workspace).unwrap_err().code(),
        Code::ProfileNotFound
    );
}

#[test]
fn an_absent_profile_is_none_not_an_error() {
    assert_eq!(catalog().profile(ProfileId::new()).unwrap(), None);
}

#[test]
fn an_unparseable_locale_falls_back_rather_than_failing_the_read() {
    // A row from a future build must not make a profile unreadable.
    let mut c = catalog();
    let p = a_profile(&mut c);
    c.connection
        .execute(
            "UPDATE profiles SET locale = 'not a tag' WHERE id = ?1",
            params![p.id.to_storage()],
        )
        .unwrap();
    assert_eq!(c.profile(p.id).unwrap().unwrap().locale.as_str(), "und");
}

#[test]
fn a_corrupt_id_is_reported_rather_than_panicking() {
    let mut c = catalog();
    let p = a_profile(&mut c);
    // The Profile's transport mode row references this id, so the fixture takes
    // it out of the way first. Deferring the constraint instead would let both
    // rows carry the bad id, and the read under test is about `profiles`.
    c.connection
        .execute(
            "DELETE FROM local_transport_modes WHERE owner_profile_id = ?1",
            params![p.id.to_storage()],
        )
        .unwrap();
    c.connection
        .execute(
            "UPDATE profiles SET id = 'not-a-uuid' WHERE id = ?1",
            params![p.id.to_storage()],
        )
        .unwrap();
    assert_eq!(c.profiles().unwrap_err().code(), Code::CorruptState);
}

// ------------------------------------------------------------------ workspaces

#[test]
fn a_workspace_round_trips_and_belongs_to_its_owner() {
    let mut c = catalog();
    let p = a_profile(&mut c);
    let w = Workspace::new(p.id, "季度计划", at()).unwrap();
    c.put_workspace(&w).unwrap();
    assert_eq!(c.workspace(w.id).unwrap().as_ref(), Some(&w));
    assert_eq!(c.workspaces().unwrap(), vec![w]);
}

#[test]
fn a_workspace_without_an_owner_is_refused() {
    // A foreign key, reported as a code a client can branch on rather than as
    // a SQLite string.
    let mut c = catalog();
    let w = Workspace::new(ProfileId::new(), "Orphan", at()).unwrap();
    assert_eq!(
        c.put_workspace(&w).unwrap_err().code(),
        Code::ProfileNotFound
    );
}

#[test]
fn touching_a_workspace_records_when_it_was_opened() {
    let mut c = catalog();
    let p = a_profile(&mut c);
    let w = Workspace::new(p.id, "W", at()).unwrap();
    c.put_workspace(&w).unwrap();
    assert_eq!(c.workspace(w.id).unwrap().unwrap().last_opened_at, None);

    let later = Timestamp::from_millis(at().millis() + 5_000);
    c.touch_workspace(w.id, later).unwrap();
    assert_eq!(
        c.workspace(w.id).unwrap().unwrap().last_opened_at,
        Some(later)
    );
}

#[test]
fn touching_an_unknown_workspace_says_so() {
    let mut c = catalog();
    assert_eq!(
        c.touch_workspace(WorkspaceId::new(), at())
            .unwrap_err()
            .code(),
        Code::WorkspaceNotFound
    );
}

#[test]
fn workspace_order_is_replaced_atomically_and_scoped_to_its_owner() {
    let mut c = catalog();
    let owner = a_profile(&mut c);
    let other = Profile::new("Other", Locale::default(), at()).unwrap();
    c.put_profile(&other).unwrap();
    let mut a = Workspace::new(owner.id, "A", at()).unwrap();
    let mut b = Workspace::new(owner.id, "B", at()).unwrap();
    let mut foreign = Workspace::new(other.id, "Foreign", at()).unwrap();
    a.position = 0;
    b.position = 1;
    foreign.position = 0;
    for workspace in [&a, &b, &foreign] {
        c.put_workspace(workspace).unwrap();
    }

    c.reorder_workspaces(owner.id, &[b.id, a.id]).unwrap();
    let mine: Vec<_> = c
        .workspaces_including_deleted()
        .unwrap()
        .into_iter()
        .filter(|workspace| workspace.owner == owner.id)
        .map(|workspace| workspace.id)
        .collect();
    assert_eq!(mine, vec![b.id, a.id]);
    assert_eq!(c.workspace(foreign.id).unwrap().unwrap().position, 0);

    assert_eq!(
        c.reorder_workspaces(owner.id, &[a.id, foreign.id])
            .unwrap_err()
            .code(),
        Code::InvalidRequest
    );
    let mine_after_failure: Vec<_> = c
        .workspaces_including_deleted()
        .unwrap()
        .into_iter()
        .filter(|workspace| workspace.owner == owner.id)
        .map(|workspace| workspace.id)
        .collect();
    assert_eq!(mine_after_failure, vec![b.id, a.id]);
}

#[test]
fn forgetting_a_workspace_removes_only_its_row() {
    let mut c = catalog();
    let p = a_profile(&mut c);
    let w = Workspace::new(p.id, "W", at()).unwrap();
    c.put_workspace(&w).unwrap();
    c.forget_workspace(w.id).unwrap();
    assert_eq!(c.workspace(w.id).unwrap(), None);
    // The profile is untouched: forgetting is an index operation.
    assert!(c.profile(p.id).unwrap().is_some());
}

// ------------------------------------------------------------------ lineage

#[test]
fn lineage_records_how_a_workspace_arrived() {
    let mut c = catalog();
    let p = a_profile(&mut c);
    let origin = Workspace::new(p.id, "Origin", at()).unwrap();
    let copy = Workspace::new(p.id, "Copy", at()).unwrap();
    c.put_workspace(&origin).unwrap();
    c.put_workspace(&copy).unwrap();

    let l = Lineage {
        workspace: copy.id,
        derived_from: origin.id,
        kind: LineageKind::Duplicate,
        at: at(),
    };
    c.record_lineage(&l).unwrap();
    assert_eq!(c.lineage(copy.id).unwrap(), vec![l]);
    assert!(c.lineage(origin.id).unwrap().is_empty());
}

// ------------------------------------------------------------------ clients

#[test]
fn a_client_is_found_by_its_token_hash() {
    let mut c = catalog();
    let client = ApiClient {
        id: ClientId::new(),
        name: "bootstrap".into(),
        scopes: Scope::ALL.to_vec(),
        created_at: at(),
        revoked_at: None,
    };
    c.put_client(&client, b"hashed").unwrap();
    assert_eq!(
        c.client_by_token(b"hashed").unwrap().as_ref(),
        Some(&client)
    );
    assert_eq!(c.client_by_token(b"other").unwrap(), None);
}

#[test]
fn the_token_itself_is_never_stored() {
    // A catalog that leaked must not be a set of working keys.
    let mut c = catalog();
    let client = ApiClient {
        id: ClientId::new(),
        name: "bootstrap".into(),
        scopes: vec![Scope::ReadDocuments],
        created_at: at(),
        revoked_at: None,
    };
    c.put_client(&client, b"the-hash-not-the-token").unwrap();

    let columns: Vec<String> = c
        .connection
        .prepare("SELECT name FROM pragma_table_info('api_clients')")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(columns.contains(&"token_hash".to_string()));
    assert!(!columns.contains(&"token".to_string()), "{columns:?}");
}

#[test]
fn revoking_a_client_stops_it_allowing_anything() {
    let mut c = catalog();
    let client = ApiClient {
        id: ClientId::new(),
        name: "temp".into(),
        scopes: Scope::ALL.to_vec(),
        created_at: at(),
        revoked_at: None,
    };
    c.put_client(&client, b"h").unwrap();
    c.revoke_client(client.id, at()).unwrap();

    let back = c.client_by_token(b"h").unwrap().expect("still a row");
    assert!(!back.active());
    for s in Scope::ALL {
        assert!(!back.allows(*s), "{s} survived revocation");
    }
}

#[test]
fn revoking_twice_is_refused_without_saying_which_case_it_was() {
    let mut c = catalog();
    let client = ApiClient {
        id: ClientId::new(),
        name: "temp".into(),
        scopes: vec![],
        created_at: at(),
        revoked_at: None,
    };
    c.put_client(&client, b"h").unwrap();
    c.revoke_client(client.id, at()).unwrap();

    // Already revoked and never existed must be indistinguishable, or the
    // error is an id-enumeration oracle.
    assert_eq!(
        c.revoke_client(client.id, at()).unwrap_err().code(),
        Code::Forbidden
    );
    assert_eq!(
        c.revoke_client(ClientId::new(), at()).unwrap_err().code(),
        Code::Forbidden
    );
}

#[test]
fn scopes_round_trip_and_unknown_ones_grant_nothing() {
    let mut c = catalog();
    let client = ApiClient {
        id: ClientId::new(),
        name: "reader".into(),
        scopes: vec![Scope::ReadDocuments, Scope::ReadTasks],
        created_at: at(),
        revoked_at: None,
    };
    c.put_client(&client, b"h").unwrap();
    assert_eq!(
        c.client_by_token(b"h").unwrap().unwrap().scopes,
        client.scopes
    );

    // A scope from a future build must grant nothing here, not everything.
    c.connection
        .execute(
            "UPDATE api_clients SET scopes = 'read_documents administer_everything'",
            [],
        )
        .unwrap();
    let back = c.client_by_token(b"h").unwrap().unwrap();
    assert_eq!(back.scopes, vec![Scope::ReadDocuments]);
}

// ------------------------------------------------------------------ state

#[test]
fn store_state_is_a_key_value_that_overwrites() {
    let mut c = catalog();
    // `peer_id` is no longer an arbitrary key: catalog 13's migration mints
    // the Store peer and the local user scope when the file is created.
    assert!(c.state("peer_id").unwrap().is_some());
    let key = "tests.a_key_the_catalog_does_not_own";
    assert_eq!(c.state(key).unwrap(), None);
    c.set_state(key, "12345").unwrap();
    c.set_state(key, "67890").unwrap();
    assert_eq!(c.state(key).unwrap().as_deref(), Some("67890"));
}

// ------------------------------------------------------------------ schema

#[test]
fn a_fresh_catalog_is_at_the_current_schema_and_passes_its_check() {
    let c = catalog();
    assert_eq!(c.schema_version().unwrap(), CATALOG_SCHEMA_VERSION);
    assert!(c.quick_check().unwrap());
}

#[test]
fn opening_an_existing_catalog_twice_is_safe() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog.db");
    let p = {
        let mut c = SqliteCatalog::open(&path).unwrap();
        a_profile(&mut c)
    };
    let c = SqliteCatalog::open(&path).unwrap();
    assert_eq!(c.profile(p.id).unwrap().unwrap().id, p.id);
    assert_eq!(c.schema_version().unwrap(), CATALOG_SCHEMA_VERSION);
}

#[test]
fn schema_six_migrates_profile_appearance_and_preserves_legacy_colour() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog.db");
    let id = ProfileId::new();
    {
        let mut connection = crate::db::open(&path).unwrap();
        crate::db::migrate(&mut connection, "catalog.db", &MIGRATIONS[..6], 6).unwrap();
        connection
            .execute(
                "INSERT INTO profiles
                    (id, name, purpose, avatar, account, locale, created_at)
                 VALUES (?1, 'Owner', NULL, 'color:purple', NULL, 'en', ?2)",
                params![id.to_storage(), at().millis()],
            )
            .unwrap();
    }

    let c = SqliteCatalog::open(&path).unwrap();
    let profile = c.profile(id).unwrap().expect("profile survives migration");
    assert_eq!(c.schema_version().unwrap(), CATALOG_SCHEMA_VERSION);
    assert_eq!(profile.avatar.as_deref(), Some("color:purple"));
    assert_eq!(profile.color.as_deref(), Some("purple"));
    assert_eq!(profile.icon, None);
}

#[test]
fn schema_eight_adds_the_sync_event_log_without_touching_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog.db");
    let profile = {
        let mut connection = crate::db::open(&path).unwrap();
        crate::db::migrate(&mut connection, "catalog.db", &MIGRATIONS[..7], 7).unwrap();
        let profile = Profile::new("Owner", Locale::parse("en").unwrap(), at()).unwrap();
        connection
            .execute(
                "INSERT INTO profiles
                    (id, name, purpose, avatar, account, locale, created_at, icon, color)
                 VALUES (?1, ?2, NULL, NULL, NULL, 'en', ?3, NULL, NULL)",
                params![
                    profile.id.to_storage(),
                    profile.name.as_str(),
                    profile.created_at.millis()
                ],
            )
            .unwrap();
        profile
    };
    let catalog = SqliteCatalog::open(&path).unwrap();
    assert_eq!(catalog.schema_version().unwrap(), CATALOG_SCHEMA_VERSION);
    assert_eq!(catalog.profile(profile.id).unwrap().unwrap().name, "Owner");
    let indexes: i64 = catalog
        .connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name IN
               ('sync_state_events_peer_id', 'sync_state_events_received_at')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(indexes, 2);
}

#[test]
fn schema_nine_adds_local_profile_removal_without_hiding_existing_profiles() {
    let mut connection = crate::db::open_in_memory().unwrap();
    crate::db::migrate(&mut connection, "catalog.db", &MIGRATIONS[..8], 8).unwrap();
    let profile = Profile::new("Owner", Locale::default(), at()).unwrap();
    connection
        .execute(
            "INSERT INTO profiles
                (id, name, purpose, avatar, account, locale, created_at, icon, color)
             VALUES (?1, ?2, NULL, NULL, NULL, 'und', ?3, NULL, NULL)",
            params![
                profile.id.to_storage(),
                profile.name.as_str(),
                profile.created_at.millis()
            ],
        )
        .unwrap();
    assert!(!crate::db::has_column(&connection, "profiles", "removed_at").unwrap());

    let catalog = SqliteCatalog::from_connection(connection).unwrap();
    assert_eq!(catalog.schema_version().unwrap(), CATALOG_SCHEMA_VERSION);
    assert!(crate::db::has_column(&catalog.connection, "profiles", "removed_at").unwrap());
    assert_eq!(catalog.profile(profile.id).unwrap(), Some(profile));
}

#[test]
fn schema_ten_reconciles_an_oauth_stamped_v9_without_kind() {
    let mut connection = crate::db::open_in_memory().unwrap();
    crate::db::migrate(&mut connection, "catalog.db", &MIGRATIONS[..8], 8).unwrap();
    connection
        .execute_batch(
            "ALTER TABLE profiles ADD COLUMN removed_at INTEGER;
             PRAGMA user_version = 9;",
        )
        .unwrap();

    let catalog = SqliteCatalog::from_connection(connection).unwrap();

    assert_eq!(catalog.schema_version().unwrap(), CATALOG_SCHEMA_VERSION);
    assert!(crate::db::has_column(&catalog.connection, "profiles", "removed_at").unwrap());
    assert!(crate::db::has_column(&catalog.connection, "profiles", "kind").unwrap());
}

#[test]
fn schema_ten_reconciles_a_dev_stamped_v9_without_removed_at() {
    let mut connection = crate::db::open_in_memory().unwrap();
    crate::db::migrate(&mut connection, "catalog.db", &MIGRATIONS[..8], 8).unwrap();
    connection
        .execute_batch(
            "ALTER TABLE profiles ADD COLUMN kind TEXT;
             PRAGMA user_version = 9;",
        )
        .unwrap();

    let catalog = SqliteCatalog::from_connection(connection).unwrap();

    assert_eq!(catalog.schema_version().unwrap(), CATALOG_SCHEMA_VERSION);
    assert!(crate::db::has_column(&catalog.connection, "profiles", "removed_at").unwrap());
    assert!(crate::db::has_column(&catalog.connection, "profiles", "kind").unwrap());
}

fn sync_event(peer: &str, millis: i64, items: usize) -> SyncStateEvent {
    SyncStateEvent {
        id: None,
        received_at: Timestamp::from_millis(millis),
        peer_device_id: peer.into(),
        kind: "document_updated".into(),
        profile: Some(ProfileId::new()),
        workspace: Some(WorkspaceId::new()),
        document: Some(DocumentId::new()),
        document_name: Some("Roadmap".into()),
        blob_hash: None,
        detail: Some("body changed".into()),
        items: Some(items),
    }
}

#[test]
fn sync_events_round_trip_page_stably_and_prune_at_the_seven_day_cutoff() {
    let mut catalog = catalog();
    for index in 0..60 {
        catalog
            .record_sync_state_event(
                &sync_event("dev-peer", 1_000 + index, index as usize),
                Timestamp::from_millis(0),
            )
            .unwrap();
    }
    let first = catalog
        .sync_state_events("dev-peer", Timestamp::from_millis(0), None, 50)
        .unwrap();
    assert_eq!(first.len(), 50);
    assert_eq!(first[0].items, Some(59));
    assert_eq!(first[49].items, Some(10));
    let cursor = first.last().unwrap().id;

    // A new row between requests does not shift an id-cursor page.
    catalog
        .record_sync_state_event(
            &sync_event("dev-peer", 2_000, 60),
            Timestamp::from_millis(0),
        )
        .unwrap();
    let second = catalog
        .sync_state_events("dev-peer", Timestamp::from_millis(0), cursor, 50)
        .unwrap();
    assert_eq!(second.len(), 10);
    assert_eq!(second[0].items, Some(9));
    assert_eq!(second[9].items, Some(0));

    catalog
        .record_sync_state_event(
            &sync_event("dev-other", 3_000, 1),
            Timestamp::from_millis(1_030),
        )
        .unwrap();
    assert!(
        catalog
            .sync_state_events("dev-peer", Timestamp::from_millis(0), None, 100)
            .unwrap()
            .iter()
            .all(|event| event.received_at.millis() >= 1_030)
    );
    assert_eq!(
        catalog
            .latest_sync_state_event_times(Timestamp::from_millis(0))
            .unwrap(),
        vec![
            ("dev-other".into(), Timestamp::from_millis(3_000)),
            ("dev-peer".into(), Timestamp::from_millis(2_000)),
        ]
    );
}

// ---------------------------------------------------- the No-profile holder

/// **Schema 9 adds the marker without disturbing the personas already there.**
///
/// The fixture builds the exact schema-8 shape by hand — nine columns, no
/// `kind` — and opens it through production migration code. A row written
/// before the column existed must come back as a persona, because absent and
/// `normal` are the same fact and a build that guessed differently would put
/// somebody's profile in a bucket nobody can switch out of.
#[test]
fn schema_nine_adds_the_profile_kind_and_reads_an_older_row_as_a_persona() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog.db");
    let profile = {
        let mut connection = crate::db::open(&path).unwrap();
        crate::db::migrate(&mut connection, "catalog.db", &MIGRATIONS[..8], 8).unwrap();
        let profile = Profile::new("Owner", Locale::parse("en").unwrap(), at()).unwrap();
        connection
            .execute(
                "INSERT INTO profiles
                    (id, name, purpose, avatar, account, locale, created_at, icon, color)
                 VALUES (?1, ?2, NULL, NULL, NULL, 'en', ?3, NULL, NULL)",
                params![
                    profile.id.to_storage(),
                    profile.name.as_str(),
                    profile.created_at.millis()
                ],
            )
            .unwrap();
        profile
    };
    let catalog = SqliteCatalog::open(&path).unwrap();
    assert_eq!(catalog.schema_version().unwrap(), CATALOG_SCHEMA_VERSION);
    let migrated = catalog.profile(profile.id).unwrap().expect("survives");
    assert_eq!(migrated.kind, ProfileKind::Normal);
    assert!(!migrated.is_holder());
    let columns: i64 = catalog
        .connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('profiles') WHERE name = 'kind'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(columns, 1);
}

/// The marker survives a write and a read, and listing carries it too — the
/// listing is what every exclusion filters on.
#[test]
fn a_holder_row_round_trips_as_a_holder_through_the_listing() {
    let mut catalog = catalog();
    let persona = a_profile(&mut catalog);
    let holder = Profile::holder(Locale::parse("en").unwrap(), at()).unwrap();
    catalog.put_profile(&holder).unwrap();

    assert!(catalog.profile(holder.id).unwrap().unwrap().is_holder());
    assert!(!catalog.profile(persona.id).unwrap().unwrap().is_holder());
    let listed = catalog.profiles().unwrap();
    assert_eq!(listed.iter().filter(|p| p.is_holder()).count(), 1);
    assert_eq!(listed.len(), 2);
}

/// **A stored kind this build has never heard of reads as a persona.**
///
/// The same rule `locale` follows: a row a future build wrote must still list.
/// Reading a stranger as a persona is the conservative half — it is the kind
/// that is safe to show and safe to leave alone.
#[test]
fn an_unknown_profile_kind_reads_as_a_persona_rather_than_failing_the_row() {
    let mut catalog = catalog();
    let profile = a_profile(&mut catalog);
    catalog
        .connection
        .execute(
            "UPDATE profiles SET kind = 'archivist' WHERE id = ?1",
            params![profile.id.to_storage()],
        )
        .unwrap();
    let read = catalog
        .profile(profile.id)
        .unwrap()
        .expect("still readable");
    assert_eq!(read.kind, ProfileKind::Normal);
}
