// SPDX-License-Identifier: MPL-2.0

//! Catalog 12 → 13 and the rows behind the root-catalog and personal v2
//! ports. Every database here is in memory; the upgrade fixtures stamp a
//! catalog at 12, write the legacy shape by hand, and let the ordinary open
//! migrate it — the only honest way to test what an installation meets.

use super::*;
use koru_store_core::ids::{DocumentId, ReminderId, RuleRevisionId, UserScopeId};
use koru_store_core::personal_attributes::{EntityRef, PersonalAttributeValue};
use koru_store_core::ports::{RootCatalogOverlay, RootCatalogUnitCommit, WorkspaceReminderDue};
use koru_store_core::profile_units::PersonalManifest;
use koru_store_core::reminders::{OneTimeSchedule, ReminderRule, RuleStatus};
use koru_store_core::root_catalog::seed::{seeded_deletion_id, seeded_move_id};
use koru_store_core::root_catalog::{
    CatalogUnitKind, CatalogUnitRef, K_LOCAL_USER_SCOPE, ProfileCatalogUnit, WorkspaceCatalogUnit,
};
use koru_store_core::service::{K_PEER_ID, indexed_version_is_current};
use koru_store_core::temporal::ZonedDateTime;

/// The one peer every legacy personal unit of an installation was written by.
const STORE_PEER: u64 = 4_242;

fn table_exists(connection: &Connection, name: &str) -> bool {
    connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![name],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
        == 1
}

fn user_version(connection: &Connection) -> i64 {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap()
}

fn count(connection: &Connection, sql: &str) -> i64 {
    connection.query_row(sql, [], |row| row.get(0)).unwrap()
}

/// A Workspace row the way a 12 holds it: the projection columns only.
fn legacy_workspace_row(connection: &Connection, workspace: &Workspace, deleted_at: Option<i64>) {
    connection
        .execute(
            "INSERT INTO workspaces (id, owner, name, created_at, position, deleted_at)
             VALUES (?1, ?2, ?3, ?4, 0, ?5)",
            params![
                workspace.id.to_storage(),
                workspace.owner.to_storage(),
                workspace.name,
                workspace.created_at.millis(),
                deleted_at,
            ],
        )
        .unwrap();
}

fn entity(workspace: WorkspaceId) -> EntityRef {
    EntityRef::new(
        "task",
        workspace,
        DocumentId::new(),
        DocumentId::new().as_uuid(),
    )
    .unwrap()
}

fn text(value: &str) -> PersonalAttributeValue {
    PersonalAttributeValue::Text {
        text: value.to_string(),
    }
}

fn zoned_rule(target: &EntityRef, instant_ms: i64) -> ReminderRule {
    ReminderRule {
        id: ReminderId::new(),
        target: target.clone(),
        revision: RuleRevisionId::new(),
        status: RuleStatus::Enabled,
        schedule: OneTimeSchedule::Zoned {
            value: ZonedDateTime::new(instant_ms, "UTC").unwrap(),
        },
        tone_override: None,
    }
}

/// A legacy `(owner, workspace)` personal unit written by the Store's peer.
fn legacy_personal(owner: ProfileId, workspace: WorkspaceId) -> PersonalManifest {
    let unit = PersonalManifest::create(owner, workspace).unwrap();
    unit.set_peer_id(STORE_PEER).unwrap();
    unit
}

fn legacy_personal_row(connection: &Connection, unit: &PersonalManifest) {
    connection
        .execute(
            "INSERT INTO profile_personal_crdts (owner_profile_id, workspace_id, snapshot)
             VALUES (?1, ?2, ?3)",
            params![
                unit.owner().unwrap().to_storage(),
                unit.workspace().to_storage(),
                unit.export_snapshot().unwrap(),
            ],
        )
        .unwrap();
}

fn v2_snapshot(workspace: WorkspaceId, writer: u64) -> Vec<u8> {
    PersonalManifest::create_v2(workspace, writer)
        .unwrap()
        .export_snapshot()
        .unwrap()
}

fn due(workspace: WorkspaceId, due_at_ms: Option<i64>) -> WorkspaceReminderDue {
    WorkspaceReminderDue {
        workspace,
        reminder: ReminderId::new(),
        due_at_ms,
    }
}

fn a_workspace(c: &mut SqliteCatalog, owner: ProfileId) -> WorkspaceId {
    let workspace = Workspace::new(owner, "Workspace", at()).unwrap();
    c.put_workspace(&workspace).unwrap();
    workspace.id
}

// ------------------------------------------------------------- the bump

#[test]
fn a_fresh_catalog_has_the_root_tables_one_scope_and_no_legacy_personal_tables() {
    let c = catalog();
    assert_eq!(c.schema_version().unwrap(), CATALOG_SCHEMA_VERSION);
    assert_eq!(CATALOG_SCHEMA_VERSION, 13);
    for table in [
        "root_catalog_units",
        "root_catalog_overlays",
        "workspace_personal_crdts",
        "workspace_reminder_due",
        "root_catalog_migration_receipts",
    ] {
        assert!(table_exists(&c.connection, table), "{table}");
    }
    for table in ["profile_personal_crdts", "profile_reminder_due"] {
        assert!(!table_exists(&c.connection, table), "{table}");
    }
    let scope = c.local_user_scope().unwrap();
    assert_eq!(
        c.state(K_LOCAL_USER_SCOPE).unwrap(),
        Some(scope.to_storage())
    );
    // The Store's peer id was minted for it, as `Store::open` would.
    let peer: u64 = c.state(K_PEER_ID).unwrap().unwrap().parse().unwrap();
    assert!(peer < u64::MAX);
    assert!(
        c.root_catalog_units(scope, CatalogUnitKind::Profile)
            .unwrap()
            .is_empty()
    );
    assert!(c.workspace_personal_units(scope).unwrap().is_empty());
    assert_eq!(
        count(
            &c.connection,
            "SELECT COUNT(*) FROM root_catalog_migration_receipts"
        ),
        0
    );
    assert!(c.quick_check().unwrap());
}

#[test]
fn a_twelve_upgrade_seeds_units_merges_personal_histories_and_drops_the_legacy_tables() {
    let name = format!("root-catalog-upgrade-{}", ProfileId::new());
    let keeper = db::open_shared_in_memory(&name).unwrap();
    let mut connection = db::open_shared_in_memory(&name).unwrap();
    db::migrate(&mut connection, "catalog.db", &MIGRATIONS[..12], 12).unwrap();

    // The legacy state: an active Profile, a locally removed one, a Holder;
    // a live Workspace, a trashed one, one owned by the removed Profile; two
    // personal units for the live Workspace written by the one Store peer.
    let owner = Profile::new("Owner", Locale::default(), at()).unwrap();
    let removed = Profile::new("Removed", Locale::default(), at()).unwrap();
    let holder = Profile::holder(Locale::default(), at()).unwrap();
    for profile in [&owner, &removed, &holder] {
        legacy_profile_row(&connection, profile);
    }
    let removed_at = at().millis() + 5;
    connection
        .execute(
            "UPDATE profiles SET removed_at = ?2 WHERE id = ?1",
            params![removed.id.to_storage(), removed_at],
        )
        .unwrap();
    let live = Workspace::new(owner.id, "Live", at()).unwrap();
    let trashed = Workspace::new(owner.id, "Trashed", at()).unwrap();
    let orphaned = Workspace::new(removed.id, "Of the removed Profile", at()).unwrap();
    let deleted_at = at().millis() + 9;
    legacy_workspace_row(&connection, &live, None);
    legacy_workspace_row(&connection, &trashed, Some(deleted_at));
    legacy_workspace_row(&connection, &orphaned, None);
    connection
        .execute(
            "INSERT INTO store_state (key, value) VALUES (?1, ?2)",
            params![K_PEER_ID, STORE_PEER.to_string()],
        )
        .unwrap();
    let target = entity(live.id);
    let by_owner = legacy_personal(owner.id, live.id);
    by_owner
        .set_personal_attribute(&target, "from", &text("owner"))
        .unwrap();
    let rule = zoned_rule(&target, 1_800_000_000_000);
    by_owner.put_reminder_rule(&rule).unwrap();
    let by_removed = legacy_personal(removed.id, live.id);
    by_removed
        .set_personal_attribute(&target, "also", &text("removed profile"))
        .unwrap();
    let deleted_rule = zoned_rule(&target, 1_900_000_000_000);
    by_removed.put_reminder_rule(&deleted_rule).unwrap();
    by_removed.delete_reminder(deleted_rule.id).unwrap();
    // Both start at counter zero of the same peer: the hazard §5.2 names.
    assert!(by_owner.version().get(&STORE_PEER).is_some());
    assert!(by_removed.version().get(&STORE_PEER).is_some());
    legacy_personal_row(&connection, &by_owner);
    legacy_personal_row(&connection, &by_removed);
    let orphan_target = entity(orphaned.id);
    let orphan_unit = legacy_personal(removed.id, orphaned.id);
    orphan_unit
        .set_personal_attribute(&orphan_target, "note", &text("kept"))
        .unwrap();
    legacy_personal_row(&connection, &orphan_unit);

    let c = SqliteCatalog::from_connection(connection).unwrap();

    assert_eq!(c.schema_version().unwrap(), 13);
    assert!(!table_exists(&c.connection, "profile_personal_crdts"));
    assert!(!table_exists(&c.connection, "profile_reminder_due"));
    assert_eq!(c.state(K_PEER_ID).unwrap(), Some(STORE_PEER.to_string()));
    let scope = c.local_user_scope().unwrap();

    // Root seeds: one live Profile unit per non-Holder row, removed or not.
    let profiles = c
        .root_catalog_units(scope, CatalogUnitKind::Profile)
        .unwrap();
    assert_eq!(profiles.len(), 2);
    for (unit, row) in &profiles {
        let profile = unit.profile().unwrap();
        assert!(profile == owner.id || profile == removed.id);
        assert_eq!(row.revision, 1);
        let unit = ProfileCatalogUnit::from_snapshot(&row.snapshot, profile, STORE_PEER).unwrap();
        assert!(unit.is_live());
        assert!(unit.deletions().is_empty());
    }
    assert!(
        c.root_catalog_unit(scope, CatalogUnitRef::Profile { profile: holder.id })
            .unwrap()
            .is_none()
    );
    // One Workspace unit per row, placed under the projection's owner; the
    // trashed one carries the one deterministic deletion fact.
    let workspaces = c
        .root_catalog_units(scope, CatalogUnitKind::Workspace)
        .unwrap();
    assert_eq!(workspaces.len(), 3);
    for (address, row) in &workspaces {
        let workspace = address.workspace().unwrap();
        let unit =
            WorkspaceCatalogUnit::from_snapshot(&row.snapshot, workspace, STORE_PEER).unwrap();
        let placement = unit.placement().unwrap();
        assert_eq!(placement.move_id, seeded_move_id(workspace));
        if workspace == trashed.id {
            assert_eq!(placement.profile, Some(owner.id));
            assert!(unit.is_deleted());
            let deletions = unit.deletions();
            assert_eq!(deletions.len(), 1);
            let (id, record) = deletions.iter().next().unwrap();
            assert_eq!(*id, seeded_deletion_id(trashed.id, deleted_at));
            assert_eq!(record.at_ms, deleted_at);
            assert_eq!(record.batch_id, None);
        } else {
            assert!(unit.is_live());
            let expected = if workspace == live.id {
                owner.id
            } else {
                removed.id
            };
            assert_eq!(placement.profile, Some(expected));
        }
    }
    // Receipts: one per Profile row (the Holder's says excluded), one per
    // Workspace, one per migrated Workspace personal unit.
    assert_eq!(
        count(
            &c.connection,
            "SELECT COUNT(*) FROM root_catalog_migration_receipts WHERE kind = 'root_seed'"
        ),
        6
    );
    let holder_receipt: String = c
        .connection
        .query_row(
            "SELECT receipt FROM root_catalog_migration_receipts
             WHERE kind = 'root_seed' AND subject_id = ?1",
            params![holder.id.to_storage()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(holder_receipt.contains("holder_excluded"));
    let removed_receipt: String = c
        .connection
        .query_row(
            "SELECT receipt FROM root_catalog_migration_receipts
             WHERE kind = 'root_seed' AND subject_id = ?1",
            params![removed.id.to_storage()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(removed_receipt.contains(&format!("\"legacy_removed_at_ms\":{removed_at}")));
    assert_eq!(
        count(
            &c.connection,
            "SELECT COUNT(*) FROM root_catalog_migration_receipts WHERE kind = 'personal_v2'"
        ),
        2
    );
    let personal_receipt: String = c
        .connection
        .query_row(
            "SELECT receipt FROM root_catalog_migration_receipts
             WHERE kind = 'personal_v2' AND subject_id = ?1",
            params![live.id.to_storage()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(personal_receipt.contains("koru-personal-v2-peer"));
    assert!(personal_receipt.contains(&format!("\"original\":{STORE_PEER}")));

    // The live Workspace's unit holds both histories, including the one of
    // the removed Profile, and no legacy address is needed to find it.
    let state = c.workspace_personal_unit(scope, live.id).unwrap().unwrap();
    let merged = PersonalManifest::from_snapshot_v2(&state.snapshot, live.id).unwrap();
    assert!(!merged.is_read_only());
    assert_eq!(merged.owner(), None);
    let attributes = merged.personal_attributes(&target);
    assert_eq!(attributes["from"], text("owner"));
    assert_eq!(attributes["also"], text("removed profile"));
    assert!(!merged.reminder(rule.id).unwrap().deleted);
    assert!(merged.reminder(deleted_rule.id).unwrap().deleted);
    assert!(merged.version().get(&STORE_PEER).is_none());
    assert!(state.writer_peer_id != STORE_PEER);
    assert!(merged.version().get(&state.writer_peer_id).is_some());
    // The stamp names the merged version. Asserted decoded, never as bytes:
    // the stamp was encoded off the assembled candidate and `merged` built its
    // own map by importing the stored snapshot, and `VersionVector::encode`
    // emits peer entries in its `HashMap`'s iteration order — so the same
    // version came out of the two with entries transposed often enough to fail
    // CI and never often enough to fail here.
    assert!(indexed_version_is_current(
        state.indexed_version.as_deref(),
        &merged.version()
    ));
    let mut due_rows = c.workspace_reminder_due(scope).unwrap();
    due_rows.retain(|row| row.workspace == live.id);
    due_rows.sort_by_key(|row| row.reminder);
    let mut expected = vec![
        WorkspaceReminderDue {
            workspace: live.id,
            reminder: rule.id,
            due_at_ms: Some(1_800_000_000_000),
        },
        WorkspaceReminderDue {
            workspace: live.id,
            reminder: deleted_rule.id,
            due_at_ms: None,
        },
    ];
    expected.sort_by_key(|row| row.reminder);
    assert_eq!(due_rows, expected);
    // The removed Profile's own Workspace kept its personal data too.
    let orphan_state = c
        .workspace_personal_unit(scope, orphaned.id)
        .unwrap()
        .unwrap();
    let orphan = PersonalManifest::from_snapshot_v2(&orphan_state.snapshot, orphaned.id).unwrap();
    assert_eq!(
        orphan.personal_attributes(&orphan_target)["note"],
        text("kept")
    );
    assert_eq!(c.workspace_personal_units(scope).unwrap().len(), 2);
    assert!(c.workspace_personal_unit(scope, live.id).unwrap().is_some());
    assert!(
        c.workspace_personal_unit(scope, orphaned.id)
            .unwrap()
            .is_some()
    );
    assert!(
        c.workspace_personal_unit(scope, trashed.id)
            .unwrap()
            .is_none()
    );
    // This scope's own rows are no purge hold: the retirement job is theirs.
    assert_eq!(c.personal_unit_hold(scope, live.id).unwrap(), None);
    // The v1 address answers absence, not an error.
    assert_eq!(
        c.profile_unit(ProfileUnitRef::Personal {
            owner: owner.id,
            workspace: live.id
        })
        .unwrap(),
        None
    );
    // Reopening is a no-op: same stamp, same rows, same scope.
    let again = SqliteCatalog::from_connection(db::open_shared_in_memory(&name).unwrap()).unwrap();
    assert_eq!(again.schema_version().unwrap(), 13);
    assert_eq!(again.local_user_scope().unwrap(), scope);
    assert_eq!(
        again
            .root_catalog_units(scope, CatalogUnitKind::Workspace)
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        count(
            &again.connection,
            "SELECT COUNT(*) FROM root_catalog_migration_receipts"
        ),
        8
    );
    // And an older build meeting the upgraded file changes nothing about it.
    let mut older = db::open_shared_in_memory(&name).unwrap();
    assert_eq!(
        db::migrate(&mut older, "catalog.db", &MIGRATIONS[..12], 12)
            .unwrap_err()
            .code,
        Code::SchemaTooNew
    );
    drop(keeper);
}

#[test]
fn a_corrupt_personal_source_rolls_the_whole_step_back_and_the_next_open_retries() {
    let name = format!("root-catalog-rollback-{}", ProfileId::new());
    let keeper = db::open_shared_in_memory(&name).unwrap();
    let mut connection = db::open_shared_in_memory(&name).unwrap();
    db::migrate(&mut connection, "catalog.db", &MIGRATIONS[..12], 12).unwrap();
    let owner = Profile::new("Owner", Locale::default(), at()).unwrap();
    legacy_profile_row(&connection, &owner);
    let workspace = Workspace::new(owner.id, "Live", at()).unwrap();
    legacy_workspace_row(&connection, &workspace, None);
    connection
        .execute(
            "INSERT INTO profile_personal_crdts (owner_profile_id, workspace_id, snapshot)
             VALUES (?1, ?2, ?3)",
            params![
                owner.id.to_storage(),
                workspace.id.to_storage(),
                b"not a loro snapshot".as_slice()
            ],
        )
        .unwrap();

    let error = match SqliteCatalog::from_connection(connection) {
        Ok(_) => panic!("a corrupt personal source must refuse the migration"),
        Err(error) => error,
    };
    assert_eq!(error.code(), Code::CorruptState);

    // Nothing of the step survived: the stamp, the tables, the seeds, the
    // scope. The legacy rows are exactly where they were.
    assert_eq!(user_version(&keeper), 12);
    for table in [
        "root_catalog_units",
        "root_catalog_overlays",
        "workspace_personal_crdts",
        "workspace_reminder_due",
        "root_catalog_migration_receipts",
    ] {
        assert!(!table_exists(&keeper, table), "{table}");
    }
    assert!(table_exists(&keeper, "profile_personal_crdts"));
    assert!(table_exists(&keeper, "profile_reminder_due"));
    assert_eq!(
        count(&keeper, "SELECT COUNT(*) FROM profile_personal_crdts"),
        1
    );
    assert_eq!(
        count(
            &keeper,
            "SELECT COUNT(*) FROM store_state WHERE key = 'local_user_scope'"
        ),
        0
    );
    // Once the source is sound, the same open migrates.
    keeper
        .execute(
            "UPDATE profile_personal_crdts SET snapshot = ?1",
            params![
                legacy_personal(owner.id, workspace.id)
                    .export_snapshot()
                    .unwrap()
            ],
        )
        .unwrap();
    let c = SqliteCatalog::from_connection(db::open_shared_in_memory(&name).unwrap()).unwrap();
    assert_eq!(c.schema_version().unwrap(), 13);
    let scope = c.local_user_scope().unwrap();
    assert!(
        c.workspace_personal_unit(scope, workspace.id)
            .unwrap()
            .is_some()
    );
    assert!(!table_exists(&c.connection, "profile_personal_crdts"));
    drop(keeper);
}

// ----------------------------------------------------------- the ports

#[test]
fn two_scopes_never_see_each_others_rows() {
    let mut c = catalog();
    let profile = a_profile(&mut c);
    let workspace = a_workspace(&mut c, profile.id);
    let mine = c.local_user_scope().unwrap();
    let theirs = UserScopeId::new();
    let unit = CatalogUnitRef::Workspace { workspace };
    for (scope, writer) in [(mine, 1), (theirs, 2)] {
        c.commit_root_catalog_units(
            scope,
            &[RootCatalogUnitCommit {
                unit,
                snapshot: format!("unit-{writer}").into_bytes(),
                expected_revision: None,
            }],
        )
        .unwrap();
        c.commit_root_catalog_overlay(
            scope,
            unit,
            &RootCatalogOverlay {
                base_vv: vec![writer as u8],
                private_snapshot: format!("overlay-{writer}").into_bytes(),
                writer_peer_id: writer,
            },
        )
        .unwrap();
        c.commit_workspace_personal_unit(scope, workspace, &v2_snapshot(workspace, writer), writer)
            .unwrap();
    }
    for (scope, writer) in [(mine, 1u64), (theirs, 2)] {
        let rows = c
            .root_catalog_units(scope, CatalogUnitKind::Workspace)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1.snapshot, format!("unit-{writer}").into_bytes());
        assert_eq!(
            c.root_catalog_overlay(scope, unit)
                .unwrap()
                .unwrap()
                .writer_peer_id,
            writer
        );
        let personal = c.workspace_personal_units(scope).unwrap();
        assert_eq!(personal.len(), 1);
        assert_eq!(personal[0].1.writer_peer_id, writer);
        assert!(c.workspace_reminder_due(scope).unwrap().is_empty());
    }
    assert!(
        c.root_catalog_unit(UserScopeId::new(), unit)
            .unwrap()
            .is_none()
    );
    assert!(
        c.workspace_personal_unit(UserScopeId::new(), workspace)
            .unwrap()
            .is_none()
    );
    // One scope's rows are a different user's rows: each scope's purge may
    // retire only its own row, and the other's is a hold it reports rather
    // than deletes around. With both rows present, each side sees the other.
    assert_eq!(
        c.personal_unit_hold(mine, workspace).unwrap(),
        Some(PurgeHold::ForeignScope)
    );
    assert_eq!(
        c.personal_unit_hold(theirs, workspace).unwrap(),
        Some(PurgeHold::ForeignScope)
    );
    // Retiring mine touches exactly mine; theirs still holds.
    assert!(c.retire_workspace_personal_unit(mine, workspace).unwrap());
    assert!(
        c.workspace_personal_unit(mine, workspace)
            .unwrap()
            .is_none()
    );
    assert!(
        c.workspace_personal_unit(theirs, workspace)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        c.personal_unit_hold(mine, workspace).unwrap(),
        Some(PurgeHold::ForeignScope)
    );
    assert_eq!(c.personal_unit_hold(theirs, workspace).unwrap(), None);
    assert_eq!(
        c.purge_workspace(workspace).unwrap_err().code,
        Code::WorkspaceInUse,
        "the RESTRICT guard stands while any scope's row remains"
    );
}

#[test]
fn root_unit_commits_are_compare_and_set_and_all_or_nothing() {
    let mut c = catalog();
    let scope = c.local_user_scope().unwrap();
    let a = CatalogUnitRef::Profile {
        profile: ProfileId::new(),
    };
    let b = CatalogUnitRef::Workspace {
        workspace: WorkspaceId::new(),
    };
    c.commit_root_catalog_units(
        scope,
        &[
            RootCatalogUnitCommit {
                unit: a,
                snapshot: b"a1".to_vec(),
                expected_revision: None,
            },
            RootCatalogUnitCommit {
                unit: b,
                snapshot: b"b1".to_vec(),
                expected_revision: None,
            },
        ],
    )
    .unwrap();
    let read = |c: &SqliteCatalog, unit| c.root_catalog_unit(scope, unit).unwrap().unwrap();
    assert_eq!(read(&c, a).revision, 1);
    assert_eq!(read(&c, b).revision, 1);
    // A must-not-exist insert of an existing row conflicts.
    assert_eq!(
        c.commit_root_catalog_units(
            scope,
            &[RootCatalogUnitCommit {
                unit: a,
                snapshot: b"a-again".to_vec(),
                expected_revision: None,
            }]
        )
        .unwrap_err()
        .code,
        Code::IdempotencyConflict
    );
    assert_eq!(read(&c, a).snapshot, b"a1");
    // The revision read moves to the revision read plus one.
    c.commit_root_catalog_units(
        scope,
        &[RootCatalogUnitCommit {
            unit: a,
            snapshot: b"a2".to_vec(),
            expected_revision: Some(1),
        }],
    )
    .unwrap();
    assert_eq!(read(&c, a).revision, 2);
    assert_eq!(read(&c, a).snapshot, b"a2");
    // A stale revision conflicts, and takes the rest of its batch with it.
    assert_eq!(
        c.commit_root_catalog_units(
            scope,
            &[
                RootCatalogUnitCommit {
                    unit: b,
                    snapshot: b"b2".to_vec(),
                    expected_revision: Some(1),
                },
                RootCatalogUnitCommit {
                    unit: a,
                    snapshot: b"a3".to_vec(),
                    expected_revision: Some(1),
                },
            ]
        )
        .unwrap_err()
        .code,
        Code::IdempotencyConflict
    );
    assert_eq!(read(&c, b).revision, 1);
    assert_eq!(read(&c, b).snapshot, b"b1");
    assert_eq!(read(&c, a).revision, 2);
    // An empty snapshot never reaches a row.
    assert_eq!(
        c.commit_root_catalog_units(
            scope,
            &[RootCatalogUnitCommit {
                unit: b,
                snapshot: Vec::new(),
                expected_revision: Some(1),
            }]
        )
        .unwrap_err()
        .code,
        Code::InvalidRequest
    );
    assert_eq!(read(&c, b).revision, 1);
    // Listing is by kind, and a missing row is just missing.
    assert_eq!(
        c.root_catalog_units(scope, CatalogUnitKind::Profile)
            .unwrap()
            .len(),
        1
    );
    assert!(
        c.root_catalog_unit(
            scope,
            CatalogUnitRef::Workspace {
                workspace: WorkspaceId::new()
            }
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn overlays_are_kept_per_unit_replaced_whole_and_cleared() {
    let mut c = catalog();
    let scope = c.local_user_scope().unwrap();
    let unit = CatalogUnitRef::Profile {
        profile: ProfileId::new(),
    };
    assert!(c.root_catalog_overlay(scope, unit).unwrap().is_none());
    // Clearing an absent overlay is not an error.
    c.clear_root_catalog_overlay(scope, unit).unwrap();
    let first = RootCatalogOverlay {
        base_vv: b"vv1".to_vec(),
        private_snapshot: b"fork1".to_vec(),
        writer_peer_id: u64::MAX >> 1,
    };
    c.commit_root_catalog_overlay(scope, unit, &first).unwrap();
    assert_eq!(c.root_catalog_overlay(scope, unit).unwrap(), Some(first));
    let second = RootCatalogOverlay {
        base_vv: b"vv2".to_vec(),
        private_snapshot: b"fork2".to_vec(),
        writer_peer_id: 3,
    };
    c.commit_root_catalog_overlay(scope, unit, &second).unwrap();
    assert_eq!(c.root_catalog_overlay(scope, unit).unwrap(), Some(second));
    c.clear_root_catalog_overlay(scope, unit).unwrap();
    assert!(c.root_catalog_overlay(scope, unit).unwrap().is_none());
}

#[test]
fn changed_personal_bytes_invalidate_the_stamp_and_a_stale_rebuild_cannot_replace_hints() {
    let mut c = catalog();
    let profile = a_profile(&mut c);
    let workspace = a_workspace(&mut c, profile.id);
    let scope = c.local_user_scope().unwrap();
    let hints = [due(workspace, None), due(workspace, Some(10))];
    c.commit_workspace_personal_unit(scope, workspace, b"snapshot-1", 9)
        .unwrap();
    assert!(
        c.replace_workspace_due_index(scope, workspace, b"snapshot-1", b"vector-1", &hints)
            .unwrap()
    );
    // Same bytes: nothing changes, the stamp stays.
    c.commit_workspace_personal_unit(scope, workspace, b"snapshot-1", 9)
        .unwrap();
    let state = c
        .workspace_personal_unit(scope, workspace)
        .unwrap()
        .unwrap();
    assert_eq!(state.indexed_version, Some(b"vector-1".to_vec()));
    assert_eq!(state.writer_peer_id, 9);
    let mut indexed = c.workspace_reminder_due(scope).unwrap();
    indexed.sort_by_key(|row| row.reminder);
    let mut expected = hints.to_vec();
    expected.sort_by_key(|row| row.reminder);
    assert_eq!(indexed, expected);
    // Changed bytes clear the stamp; a rebuild from the old bytes is refused
    // without touching the hints.
    c.commit_workspace_personal_unit(scope, workspace, b"snapshot-2", 9)
        .unwrap();
    assert_eq!(
        c.workspace_personal_unit(scope, workspace)
            .unwrap()
            .unwrap()
            .indexed_version,
        None
    );
    assert!(
        !c.replace_workspace_due_index(scope, workspace, b"snapshot-1", b"old-vector", &[])
            .unwrap()
    );
    assert_eq!(c.workspace_reminder_due(scope).unwrap().len(), 2);
    assert_eq!(
        c.workspace_personal_unit(scope, workspace)
            .unwrap()
            .unwrap()
            .indexed_version,
        None
    );
    // A rebuild from the current bytes replaces everything at once.
    assert!(
        c.replace_workspace_due_index(scope, workspace, b"snapshot-2", b"vector-2", &hints[..1])
            .unwrap()
    );
    assert_eq!(c.workspace_reminder_due(scope).unwrap(), hints[..1]);
}

#[test]
fn due_index_failure_rolls_back_stamp_and_rows_and_rejects_wrong_scope() {
    let mut c = catalog();
    let profile = a_profile(&mut c);
    let workspace = a_workspace(&mut c, profile.id);
    let scope = c.local_user_scope().unwrap();
    c.commit_workspace_personal_unit(scope, workspace, b"snapshot", 9)
        .unwrap();
    let hint = due(workspace, Some(10));
    c.replace_workspace_due_index(scope, workspace, b"snapshot", b"vector", &[hint.clone()])
        .unwrap();
    let duplicate = due(workspace, None);
    assert!(
        c.replace_workspace_due_index(
            scope,
            workspace,
            b"snapshot",
            b"new-vector",
            &[duplicate.clone(), duplicate]
        )
        .is_err()
    );
    assert_eq!(c.workspace_reminder_due(scope).unwrap(), [hint.clone()]);
    assert_eq!(
        c.workspace_personal_unit(scope, workspace)
            .unwrap()
            .unwrap()
            .indexed_version,
        Some(b"vector".to_vec())
    );
    let mut foreign = hint.clone();
    foreign.workspace = WorkspaceId::new();
    assert_eq!(
        c.replace_workspace_due_index(scope, workspace, b"snapshot", b"new-vector", &[foreign])
            .unwrap_err()
            .code,
        Code::InvalidRequest
    );
    assert_eq!(c.workspace_reminder_due(scope).unwrap(), [hint]);
    // Another scope's rebuild of the same Workspace finds no row of its own.
    assert!(
        !c.replace_workspace_due_index(UserScopeId::new(), workspace, b"snapshot", b"vector", &[])
            .unwrap()
    );
}

#[test]
fn a_retained_personal_v2_row_still_blocks_workspace_purge_and_forget() {
    let mut c = catalog();
    let profile = a_profile(&mut c);
    c.initialize_profile_units(&profile, b"identity", b"private")
        .unwrap();
    let workspace = a_workspace(&mut c, profile.id);
    let scope = c.local_user_scope().unwrap();
    c.commit_workspace_personal_unit(scope, workspace, &v2_snapshot(workspace, 9), 9)
        .unwrap();
    c.replace_workspace_due_index(
        scope,
        workspace,
        &v2_snapshot(workspace, 9),
        b"vector",
        &[due(workspace, None)],
    )
    .unwrap();
    let location = DocumentId::new();
    c.set_workspace_deleted(workspace, Some(at()), &[]).unwrap();
    c.record_location(location, workspace).unwrap();
    // Removing the Profile locally hides nothing of the Workspace's data.
    let attempt = super::begin_removal(&mut c, profile.id);
    c.remove_profile_local(profile.id, Some(attempt), at())
        .unwrap();
    assert!(
        c.workspace_personal_unit(scope, workspace)
            .unwrap()
            .is_some()
    );
    assert!(
        c.workspace_personal_unit(scope, WorkspaceId::new())
            .unwrap()
            .is_none()
    );
    let retained = c.workspace_personal_unit(scope, workspace).unwrap();
    // The §5.3 guard: RESTRICT until Plan 1.3's retirement job.
    assert_eq!(
        c.forget_workspace(workspace).unwrap_err().code,
        Code::WorkspaceInUse
    );
    assert_eq!(
        c.purge_workspace(workspace).unwrap_err().code,
        Code::WorkspaceInUse
    );
    assert!(
        c.connection
            .execute(
                "DELETE FROM workspaces WHERE id = ?1",
                params![workspace.to_storage()]
            )
            .is_err()
    );
    assert_eq!(c.locate(location).unwrap(), Some(workspace));
    assert_eq!(
        c.workspace_personal_unit(scope, workspace).unwrap(),
        retained
    );
    assert_eq!(c.workspace_reminder_due(scope).unwrap().len(), 1);
    // And the former Profile's row can go without touching the personal data:
    // no foreign key points at it any more.
    assert!(
        c.connection
            .execute(
                "DELETE FROM profiles WHERE id = ?1",
                params![profile.id.to_storage()]
            )
            .is_err(),
        "the Workspace row still references its owner; the personal row does not"
    );
}

#[test]
fn empty_or_unknown_workspace_personal_rows_are_refused() {
    let mut c = catalog();
    let profile = a_profile(&mut c);
    let workspace = a_workspace(&mut c, profile.id);
    let scope = c.local_user_scope().unwrap();
    assert!(
        c.commit_workspace_personal_unit(scope, workspace, b"", 9)
            .is_err()
    );
    assert!(
        c.commit_workspace_personal_unit(scope, WorkspaceId::new(), b"snapshot", 9)
            .is_err()
    );
    assert!(
        !c.replace_workspace_due_index(scope, workspace, b"snapshot", b"vector", &[])
            .unwrap()
    );
    assert!(
        c.workspace_personal_unit(scope, workspace)
            .unwrap()
            .is_none()
    );
    assert_eq!(c.personal_unit_hold(scope, workspace).unwrap(), None);
    assert!(!c.retire_workspace_personal_unit(scope, workspace).unwrap());
    // A large writer id round-trips through the i64 column with its bits.
    c.commit_workspace_personal_unit(scope, workspace, b"snapshot", u64::MAX - 1)
        .unwrap();
    assert_eq!(
        c.workspace_personal_unit(scope, workspace)
            .unwrap()
            .unwrap()
            .writer_peer_id,
        u64::MAX - 1
    );
}
