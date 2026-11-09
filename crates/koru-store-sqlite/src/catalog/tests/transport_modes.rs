// SPDX-License-Identifier: MPL-2.0

//! `local_transport_modes` on disk: the 11 → 12 migration, the constraints the
//! DDL carries, and the compare-and-set as one serialized transaction.
//!
//! The decision *rules* are pinned in `koru_store_core::transport` with no
//! database in sight; what is proved here is that the storage holds what those
//! rules assume — the seeding, the ownership validation, and the refusals the
//! CHECKs and foreign keys are there to make.

use super::*;
use koru_store_core::transport::{InitializeTransportMode, SetTransportMode};

fn ws_of(c: &mut SqliteCatalog, owner: ProfileId, name: &str) -> Workspace {
    let workspace = Workspace::new(owner, name, at()).unwrap();
    c.put_workspace(&workspace).unwrap();
    workspace
}

fn set(subject: TransportSubject, expected: i64, mode: TransportMode) -> TransportModeRequest {
    TransportModeRequest::Set(SetTransportMode {
        subject,
        expected_revision: expected,
        mode,
    })
}

fn init(subject: TransportSubject, expected: i64, mode: TransportMode) -> TransportModeRequest {
    TransportModeRequest::Initialize(InitializeTransportMode {
        subject,
        expected_revision: expected,
        mode,
    })
}

fn has_table(c: &SqliteCatalog, name: &str) -> bool {
    c.connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![name],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
        == 1
}

// --------------------------------------------------------------- the bump

#[test]
fn a_fresh_catalog_carries_the_table_and_its_index() {
    // Upgrade fixtures do not catch invalid current DDL — this is the clean
    // install's shape, opened through production code.
    let c = catalog();

    assert_eq!(c.schema_version().unwrap(), CATALOG_SCHEMA_VERSION);
    assert_eq!(CATALOG_SCHEMA_VERSION, 13);
    assert!(has_table(&c, "local_transport_modes"));
    let index: String = c
        .connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'local_transport_modes_owner_idx'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        index.contains("owner_profile_id, subject_kind, subject_id"),
        "{index}"
    );
}

#[test]
fn an_eleven_upgrade_seeds_every_retained_profile_and_workspace_local_only() {
    let mut connection = db::open_in_memory().unwrap();
    db::migrate(&mut connection, "catalog.db", &MIGRATIONS[..11], 11).unwrap();

    let owner = Profile::new("Existing", Locale::default(), at()).unwrap();
    legacy_profile_row(&connection, &owner);
    let removed = Profile::new("Locally removed", Locale::default(), at()).unwrap();
    legacy_profile_row(&connection, &removed);
    connection
        .execute(
            "UPDATE profiles SET removed_at = ?2 WHERE id = ?1",
            params![removed.id.to_storage(), at().millis()],
        )
        .unwrap();
    let live = Workspace::new(owner.id, "Live", at()).unwrap();
    let tombstone = Workspace::new(owner.id, "In the bin", at()).unwrap();
    for (workspace, deleted) in [(&live, None), (&tombstone, Some(at().millis()))] {
        connection
            .execute(
                "INSERT INTO workspaces (id, owner, name, created_at, position, deleted_at)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5)",
                params![
                    workspace.id.to_storage(),
                    workspace.owner.to_storage(),
                    workspace.name,
                    workspace.created_at.millis(),
                    deleted,
                ],
            )
            .unwrap();
    }

    let c = SqliteCatalog::from_connection(connection).unwrap();

    assert_eq!(c.schema_version().unwrap(), CATALOG_SCHEMA_VERSION);
    // Every retained row, including the Workspace tombstone and the locally
    // removed Profile: contract §2 keeps their modes, and a missing row is a
    // storage error rather than a default.
    for subject in [
        TransportSubject::Profile(owner.id),
        TransportSubject::Profile(removed.id),
        TransportSubject::Workspace(live.id),
        TransportSubject::Workspace(tombstone.id),
    ] {
        let state = c.transport_mode(subject).unwrap();
        assert_eq!(state.mode, TransportMode::LocalOnly, "{subject:?}");
        assert!(!state.explicit_choice, "{subject:?}");
        assert_eq!(state.revision, 1, "{subject:?}");
    }
    assert_eq!(
        c.transport_mode(TransportSubject::Workspace(live.id))
            .unwrap()
            .owner,
        owner.id
    );
    // And an older build meeting the upgraded file changes nothing about it.
    let mut connection = c.connection;
    assert_eq!(
        db::migrate(&mut connection, "catalog.db", &MIGRATIONS[..11], 11)
            .unwrap_err()
            .code,
        Code::SchemaTooNew
    );
}

// -------------------------------------------------- creation and ownership

#[test]
fn a_new_profile_and_workspace_get_their_rows_with_the_catalog_row() {
    let mut c = catalog();
    let owner = a_profile(&mut c);
    let workspace = ws_of(&mut c, owner.id, "Work");

    let group = c.transport_mode_group(owner.id).unwrap();
    assert_eq!(group.profile.mode, TransportMode::LocalOnly);
    assert_eq!(group.profile.revision, 1);
    assert_eq!(group.children.len(), 1);
    assert_eq!(
        group.children[0].subject,
        TransportSubject::Workspace(workspace.id)
    );
    assert_eq!(group.children[0].owner, owner.id);
}

#[test]
fn re_registering_a_profile_or_workspace_never_resets_a_choice() {
    let mut c = catalog();
    let owner = a_profile(&mut c);
    let workspace = ws_of(&mut c, owner.id, "Work");
    c.commit_transport_modes(
        owner.id,
        &[
            set(TransportSubject::Profile(owner.id), 1, TransportMode::Cloud),
            set(
                TransportSubject::Workspace(workspace.id),
                1,
                TransportMode::Cloud,
            ),
        ],
    )
    .unwrap();

    // The projection is rewritten by a rename, by provisioning, by a replica
    // arriving. None of those is a mode decision.
    let mut renamed = owner.clone();
    renamed.name = "Renamed".into();
    c.put_profile(&renamed).unwrap();
    c.put_workspace(&workspace).unwrap();

    let group = c.transport_mode_group(owner.id).unwrap();
    assert_eq!(group.profile.mode, TransportMode::Cloud);
    assert!(group.profile.explicit_choice);
    assert_eq!(group.children[0].mode, TransportMode::Cloud);
}

#[test]
fn moving_a_workspace_carries_its_owner_projection_in_the_same_transaction() {
    let mut c = catalog();
    let from = a_profile(&mut c);
    let to = Profile::new("Elsewhere", Locale::default(), at()).unwrap();
    c.put_profile(&to).unwrap();
    let mut workspace = ws_of(&mut c, from.id, "Work");
    c.commit_transport_modes(
        from.id,
        &[
            set(TransportSubject::Profile(from.id), 1, TransportMode::Cloud),
            set(
                TransportSubject::Workspace(workspace.id),
                1,
                TransportMode::Cloud,
            ),
        ],
    )
    .unwrap();

    workspace.owner = to.id;
    c.put_workspace(&workspace).unwrap();

    // The row followed the Workspace, and its choice was **not** repaired to
    // fit the new Local-only Profile: contract §3 reports the mismatch instead.
    let state = c
        .transport_mode(TransportSubject::Workspace(workspace.id))
        .unwrap();
    assert_eq!(state.owner, to.id);
    assert_eq!(state.mode, TransportMode::Cloud);
    let group = c.transport_mode_group(to.id).unwrap();
    assert_eq!(group.profile.mode, TransportMode::LocalOnly);
    assert_eq!(group.children.len(), 1);
    assert!(c.transport_mode_group(from.id).unwrap().children.is_empty());
}

#[test]
fn physical_removal_drops_the_dependent_row_first() {
    let mut c = catalog();
    let owner = a_profile(&mut c);
    let forgotten = ws_of(&mut c, owner.id, "Forgotten");
    let purged = ws_of(&mut c, owner.id, "Purged");

    // The foreign key is ON DELETE RESTRICT, so neither of these can succeed
    // unless the mode row was removed deliberately first.
    c.forget_workspace(forgotten.id).unwrap();
    c.purge_workspace(purged.id).unwrap();

    assert!(
        c.transport_mode_group(owner.id)
            .unwrap()
            .children
            .is_empty()
    );
    assert_eq!(
        c.transport_mode(TransportSubject::Workspace(purged.id))
            .unwrap_err()
            .code(),
        Code::CorruptState
    );
}

// -------------------------------------------------- the DDL's own refusals

#[test]
fn the_table_refuses_a_row_whose_kind_and_ids_disagree() {
    let mut c = catalog();
    let owner = a_profile(&mut c);
    let workspace = ws_of(&mut c, owner.id, "Work");

    // A profile row naming a workspace, and a workspace row naming no
    // workspace. Both are the ownership CHECK, not the primary key.
    for (kind, subject, ws_id) in [
        (
            "profile",
            workspace.id.to_storage(),
            Some(workspace.id.to_storage()),
        ),
        ("workspace", workspace.id.to_storage(), None),
    ] {
        let error = c
            .connection
            .execute(
                "INSERT INTO local_transport_modes
                   (subject_kind, subject_id, owner_profile_id, workspace_id,
                    mode, explicit_choice, revision)
                 VALUES (?1, ?2, ?3, ?4, 'local_only', 0, 1)",
                params![kind, subject, owner.id.to_storage(), ws_id],
            )
            .unwrap_err();
        assert!(format!("{error}").contains("CHECK"), "{kind}: {error}");
    }
}

#[test]
fn the_table_refuses_an_unknown_mode_a_non_boolean_choice_and_a_zero_revision() {
    let mut c = catalog();
    let owner = a_profile(&mut c);

    for (mode, explicit, revision) in [
        ("satellite", 0, 1),
        ("local_only", 2, 1),
        ("local_only", 0, 0),
        ("local_only", 0, -1),
    ] {
        let error = c
            .connection
            .execute(
                "UPDATE local_transport_modes
                    SET mode = ?2, explicit_choice = ?3, revision = ?4
                  WHERE subject_kind = 'profile' AND subject_id = ?1",
                params![owner.id.to_storage(), mode, explicit, revision],
            )
            .unwrap_err();
        assert!(
            format!("{error}").contains("CHECK"),
            "{mode}/{explicit}/{revision}: {error}"
        );
    }
}

#[test]
fn the_table_refuses_a_row_for_a_profile_that_does_not_exist() {
    let c = catalog();
    let absent = ProfileId::new();

    let error = c
        .connection
        .execute(
            "INSERT INTO local_transport_modes
               (subject_kind, subject_id, owner_profile_id, workspace_id,
                mode, explicit_choice, revision)
             VALUES ('profile', ?1, ?1, NULL, 'local_only', 0, 1)",
            params![absent.to_storage()],
        )
        .unwrap_err();
    assert!(format!("{error}").contains("FOREIGN KEY"), "{error}");
}

// -------------------------------------------------- reads report, not repair

#[test]
fn an_undecodable_stored_mode_is_a_storage_error() {
    let mut c = catalog();
    let owner = a_profile(&mut c);
    // Reach past the CHECK the way a damaged file would.
    c.connection
        .execute_batch("PRAGMA writable_schema = ON;")
        .unwrap();
    c.connection
        .execute(
            "UPDATE local_transport_modes SET mode = 'satellite'
              WHERE subject_kind = 'profile' AND subject_id = ?1",
            params![owner.id.to_storage()],
        )
        .ok();
    c.connection
        .execute_batch("PRAGMA writable_schema = OFF;")
        .unwrap();

    if c.connection
        .query_row(
            "SELECT mode FROM local_transport_modes WHERE subject_id = ?1",
            params![owner.id.to_storage()],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
        == "satellite"
    {
        assert_eq!(
            c.transport_mode(TransportSubject::Profile(owner.id))
                .unwrap_err()
                .code(),
            Code::CorruptState
        );
    }
}

#[test]
fn a_workspace_with_no_mode_row_makes_its_group_a_storage_error() {
    let mut c = catalog();
    let owner = a_profile(&mut c);
    let workspace = ws_of(&mut c, owner.id, "Work");
    c.connection
        .execute(
            "DELETE FROM local_transport_modes WHERE subject_id = ?1",
            params![workspace.id.to_storage()],
        )
        .unwrap();

    // Not "assume Local-only and carry on": the catalog and the mode table
    // disagree, and a compare-and-set against a guess would overwrite whatever
    // the row actually held.
    assert_eq!(
        c.transport_mode_group(owner.id).unwrap_err().code(),
        Code::CorruptState
    );
}

#[test]
fn an_owner_projection_that_drifted_is_reported_rather_than_used() {
    let mut c = catalog();
    let owner = a_profile(&mut c);
    let other = Profile::new("Other", Locale::default(), at()).unwrap();
    c.put_profile(&other).unwrap();
    let workspace = ws_of(&mut c, owner.id, "Work");
    // The two foreign keys are both still satisfied; only the relationship
    // between them is wrong, which is exactly what they cannot express.
    c.connection
        .execute(
            "UPDATE local_transport_modes SET owner_profile_id = ?2 WHERE subject_id = ?1",
            params![workspace.id.to_storage(), other.id.to_storage()],
        )
        .unwrap();

    assert_eq!(
        c.transport_mode_group(owner.id).unwrap_err().code(),
        Code::CorruptState
    );
}

// ------------------------------------------------- compare-and-set on disk

#[test]
fn a_committed_choice_is_durable_and_advances_the_revision_once() {
    let mut c = catalog();
    let owner = a_profile(&mut c);

    let outcomes = c
        .commit_transport_modes(
            owner.id,
            &[set(
                TransportSubject::Profile(owner.id),
                1,
                TransportMode::P2p,
            )],
        )
        .unwrap();
    assert!(outcomes[0].wrote());

    let stored = c
        .transport_mode(TransportSubject::Profile(owner.id))
        .unwrap();
    assert_eq!(stored.mode, TransportMode::P2p);
    assert!(stored.explicit_choice);
    assert_eq!(stored.revision, 2);

    // The exact duplicate writes nothing and does not move the revision.
    let again = c
        .commit_transport_modes(
            owner.id,
            &[set(
                TransportSubject::Profile(owner.id),
                2,
                TransportMode::P2p,
            )],
        )
        .unwrap();
    assert!(!again[0].wrote());
    assert_eq!(
        c.transport_mode(TransportSubject::Profile(owner.id))
            .unwrap()
            .revision,
        2
    );
}

#[test]
fn a_refused_group_writes_none_of_its_members() {
    let mut c = catalog();
    let owner = a_profile(&mut c);
    let one = ws_of(&mut c, owner.id, "One");
    let two = ws_of(&mut c, owner.id, "Two");
    c.commit_transport_modes(
        owner.id,
        &[
            set(TransportSubject::Profile(owner.id), 1, TransportMode::Cloud),
            set(TransportSubject::Workspace(one.id), 1, TransportMode::Cloud),
            set(TransportSubject::Workspace(two.id), 1, TransportMode::Cloud),
        ],
    )
    .unwrap();

    // Drop the Profile to P2P, resolve one child, and leave the other above the
    // new ceiling. The whole group is refused.
    let error = c
        .commit_transport_modes(
            owner.id,
            &[
                set(TransportSubject::Profile(owner.id), 2, TransportMode::P2p),
                set(TransportSubject::Workspace(one.id), 2, TransportMode::P2p),
            ],
        )
        .unwrap_err();
    assert_eq!(error.code(), Code::TransportModeChildren);

    let group = c.transport_mode_group(owner.id).unwrap();
    assert_eq!(group.profile.mode, TransportMode::Cloud);
    for child in &group.children {
        assert_eq!(child.mode, TransportMode::Cloud, "{:?}", child.subject);
        assert_eq!(child.revision, 2);
    }
}

#[test]
fn a_resolved_downgrade_lands_every_row_together() {
    let mut c = catalog();
    let owner = a_profile(&mut c);
    let one = ws_of(&mut c, owner.id, "One");
    let two = ws_of(&mut c, owner.id, "Two");
    c.commit_transport_modes(
        owner.id,
        &[
            set(TransportSubject::Profile(owner.id), 1, TransportMode::Cloud),
            set(TransportSubject::Workspace(one.id), 1, TransportMode::Cloud),
            set(TransportSubject::Workspace(two.id), 1, TransportMode::Cloud),
        ],
    )
    .unwrap();

    c.commit_transport_modes(
        owner.id,
        &[
            set(
                TransportSubject::Profile(owner.id),
                2,
                TransportMode::LocalOnly,
            ),
            set(
                TransportSubject::Workspace(one.id),
                2,
                TransportMode::LocalOnly,
            ),
            set(
                TransportSubject::Workspace(two.id),
                2,
                TransportMode::LocalOnly,
            ),
        ],
    )
    .unwrap();

    let group = c.transport_mode_group(owner.id).unwrap();
    assert_eq!(group.profile.mode, TransportMode::LocalOnly);
    assert!(
        group
            .children
            .iter()
            .all(|c| c.mode == TransportMode::LocalOnly)
    );
}

#[test]
fn a_verified_default_is_capped_at_the_profile_it_may_not_raise() {
    let mut c = catalog();
    let owner = a_profile(&mut c);
    let workspace = ws_of(&mut c, owner.id, "Work");
    c.commit_transport_modes(
        owner.id,
        &[set(
            TransportSubject::Profile(owner.id),
            1,
            TransportMode::P2p,
        )],
    )
    .unwrap();

    c.commit_transport_modes(
        owner.id,
        &[init(
            TransportSubject::Workspace(workspace.id),
            1,
            TransportMode::Cloud,
        )],
    )
    .unwrap();

    let group = c.transport_mode_group(owner.id).unwrap();
    assert_eq!(group.children[0].mode, TransportMode::P2p);
    assert!(!group.children[0].explicit_choice);
    assert_eq!(
        group.profile.mode,
        TransportMode::P2p,
        "the Profile was raised"
    );
}

#[test]
fn a_request_for_another_profiles_subject_is_refused() {
    let mut c = catalog();
    let owner = a_profile(&mut c);
    let other = Profile::new("Other", Locale::default(), at()).unwrap();
    c.put_profile(&other).unwrap();
    let elsewhere = ws_of(&mut c, other.id, "Theirs");

    let error = c
        .commit_transport_modes(
            owner.id,
            &[set(
                TransportSubject::Workspace(elsewhere.id),
                1,
                TransportMode::Cloud,
            )],
        )
        .unwrap_err();

    assert_eq!(error.code(), Code::InvalidRequest);
    assert_eq!(
        c.transport_mode(TransportSubject::Workspace(elsewhere.id))
            .unwrap()
            .mode,
        TransportMode::LocalOnly
    );
}
