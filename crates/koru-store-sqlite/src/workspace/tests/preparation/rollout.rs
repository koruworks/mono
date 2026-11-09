// SPDX-License-Identifier: MPL-2.0

//! Root-catalog Plan 4.2's two verification sentences
//! (`docs/features/root-catalog/ROLLOUT.md`), against a real catalog in
//! named memory: "stop/restart and disable-enablement preserve new writes"
//! and "rollback cannot restore old owner/lifecycle/personal writers". Each
//! test is named for the sentence it covers.

use super::root_catalog::{Root, reminder_in};
use super::*;
use koru_store_core::profile_units::ProfileUnitRef;
use koru_store_core::root_catalog::{CatalogUnitKind, CatalogUnitRef, WorkspaceCatalogUnit};
use koru_store_core::service::build_reminder_index;
use koru_store_core::transport::{SetTransportMode, TransportMode};

fn index(store: &mut Store<SqliteCatalog, FaultBackend>, profile: ProfileId) {
    for candidate in store.reminder_index_candidates(profile).unwrap() {
        let (version, rows) = build_reminder_index(&candidate).unwrap();
        assert!(
            store
                .publish_reminder_index(&candidate, &version, &rows)
                .unwrap()
        );
    }
}

/// One Profile's group, children included, set the way a person does on
/// Manage › Koru Cloud: lowering carries the children, raising lifts them.
fn arm_group(f: &mut Root, profile: ProfileId, mode: TransportMode) {
    let group = f.store.transport_mode_group(profile).unwrap();
    let mut requests = vec![SetTransportMode {
        subject: group.profile.subject,
        expected_revision: group.profile.revision,
        mode,
    }];
    for child in &group.children {
        requests.push(SetTransportMode {
            subject: child.subject,
            expected_revision: child.revision,
            mode,
        });
    }
    f.store.set_transport_modes(&requests).unwrap();
}

fn count(f: &Root, sql: &str) -> i64 {
    f.control.query_row(sql, [], |row| row.get(0)).unwrap()
}

/// The unit as every Store read sees it: **the private overlay first**, then
/// the durable row. A fact written while the subject may not export lives in
/// the overlay until it is published, and asking only the durable row would
/// report that fact as missing.
fn unit(f: &Root, workspace: WorkspaceId) -> WorkspaceCatalogUnit {
    match f.overlay(workspace) {
        Some(overlay) => {
            WorkspaceCatalogUnit::from_snapshot(&overlay.private_snapshot, workspace, 1).unwrap()
        }
        None => {
            let row = f.unit(workspace).expect("the unit row exists");
            WorkspaceCatalogUnit::from_snapshot(&row.snapshot, workspace, 1).unwrap()
        }
    }
}

/// A move, a trash and a personal reminder written after the migration
/// survive a Store restart over the same bytes, a Cloud → Local-only →
/// Cloud cycle on both Profiles with a restart in the middle, and a write
/// made while the subject could not export; every write is there exactly
/// once — one unit per subject, one fact per lifecycle act, one personal
/// row and one due row per reminder.
#[test]
fn stop_restart_and_disable_enablement_preserve_new_writes() {
    let mut f = Root::new();
    let a = f.cloud_profile("A");
    let b = f.cloud_profile("B");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    let v = f.store.create_workspace(a, "V", at()).unwrap().id;
    let (_, rule) = reminder_in(w);
    f.store.create_reminder(a, &rule).unwrap();
    index(&mut f.store, a);

    // The three new writes after the migration.
    f.store.move_workspace(w, b).unwrap();
    f.store.delete_workspace(v, at()).unwrap();
    index(&mut f.store, b);

    let intact = |f: &mut Root, restores_on_v: usize| {
        let scope = f.scope();
        assert_eq!(
            f.catalog()
                .root_catalog_units(scope, CatalogUnitKind::Workspace)
                .unwrap()
                .len(),
            2,
            "one unit per Workspace"
        );
        assert_eq!(
            f.catalog()
                .root_catalog_units(scope, CatalogUnitKind::Profile)
                .unwrap()
                .len(),
            2,
            "one unit per Profile"
        );
        assert_eq!(
            f.store.root_placement(w).unwrap().unwrap().profile,
            Some(b),
            "the move stands"
        );
        assert_eq!(f.store.workspace(w).unwrap().owner, b);
        let v_unit = unit(f, v);
        assert_eq!(v_unit.deletions().len(), 1, "one deletion fact on V");
        assert_eq!(v_unit.restores().len(), restores_on_v);
        assert_eq!(
            v_unit.is_deleted(),
            restores_on_v == 0,
            "the unit says what the last lifecycle act said"
        );
        assert_eq!(
            f.store
                .workspace_including_deleted(v)
                .unwrap()
                .deleted_at
                .is_some(),
            restores_on_v == 0,
            "and the projection agrees"
        );
        let reminders = f.store.reminders(b).unwrap();
        assert_eq!(reminders.len(), 1, "the reminder followed W to B");
        assert_eq!(reminders[0].rule.id, rule.id);
        assert!(f.store.reminders(a).unwrap().is_empty());
        assert_eq!(
            count(f, "SELECT COUNT(*) FROM workspace_personal_crdts"),
            1,
            "one personal row"
        );
        assert_eq!(
            count(f, "SELECT COUNT(*) FROM workspace_reminder_due"),
            1,
            "one due row"
        );
        assert_eq!(f.store.reminder_index_rows(b).unwrap().len(), 1);
    };
    intact(&mut f, 0);

    // Stop and start over the same bytes.
    f.restart();
    intact(&mut f, 0);

    // Disable Cloud/P2P on both Profiles (children follow), restart, and
    // write once more while nothing may leave the device: the restore of V
    // lands in A's private overlay, not in the durable unit.
    arm_group(&mut f, a, TransportMode::LocalOnly);
    arm_group(&mut f, b, TransportMode::LocalOnly);
    f.restart();
    intact(&mut f, 0);
    f.store.restore_workspace(v).unwrap();
    assert!(
        f.overlay(v).is_some(),
        "a write under a Local-only Profile waits in the overlay"
    );
    f.restart();
    assert!(f.overlay(v).is_some(), "the overlay survives a restart");
    intact(&mut f, 1);

    // Re-enable: the overlay folds into the durable unit exactly once.
    arm_group(&mut f, a, TransportMode::Cloud);
    arm_group(&mut f, b, TransportMode::Cloud);
    assert!(
        f.overlay(v).is_none(),
        "published on the eligibility change"
    );
    assert!(
        f.catalog()
            .root_catalog_overlay(f.scope(), CatalogUnitRef::Workspace { workspace: w })
            .unwrap()
            .is_none()
    );
    intact(&mut f, 1);
    f.restart();
    intact(&mut f, 1);
    assert!(f.store.workspace(v).is_ok(), "V is live again");
    assert_eq!(
        count(&f, "SELECT COUNT(*) FROM root_catalog_overlays"),
        0,
        "no overlay is left behind"
    );
}

/// A build that understands catalog 12 refuses the 13 file and leaves its
/// stamp alone; the legacy personal tables are gone; the v1 personal write
/// port refuses; and the old owner projection, written on its own, moves
/// nothing the Store reasons from. Nothing a rollback could ship revives a
/// writer the root catalog retired.
#[test]
fn rollback_cannot_restore_old_owner_lifecycle_or_personal_writers() {
    let mut f = Root::new();
    let a = f.cloud_profile("A");
    let b = f.cloud_profile("B");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    let (_, rule) = reminder_in(w);
    f.store.create_reminder(a, &rule).unwrap();

    // 1. The older build meets the file: refused, and the file is untouched.
    let refused = match SqliteCatalog::open_named_memory_as_twelve(&f.catalog) {
        Ok(_) => panic!("a 12 build opened a 13 catalog"),
        Err(error) => error,
    };
    assert_eq!(refused.code(), Code::SchemaTooNew);
    assert_eq!(count(&f, "PRAGMA user_version"), 13, "never rewritten");
    assert_eq!(
        count(
            &f,
            "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('root_catalog_units', 'root_catalog_overlays', 'workspace_personal_crdts', 'workspace_reminder_due', 'root_catalog_migration_receipts')"
        ),
        5,
        "the 13 tables are all still there"
    );

    // 2. The old personal writers have nothing to write to.
    assert_eq!(
        count(
            &f,
            "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('profile_personal_crdts', 'profile_reminder_due')"
        ),
        0,
        "the legacy personal tables are dropped"
    );
    let personal = ProfileUnitRef::Personal {
        owner: a,
        workspace: w,
    };
    let mut catalog = f.catalog();
    assert_eq!(
        catalog
            .commit_profile_unit(personal, b"v1")
            .unwrap_err()
            .code(),
        Code::ProfileUnitScopeMismatch,
        "the v1 personal write port refuses"
    );
    assert!(
        catalog.profile_unit(personal).unwrap().is_none(),
        "and the v1 address answers nothing"
    );

    // 3. The old owner projection written alone — what a pre-13 writer did
    //    — moves nothing: placement and the personal facade read the root.
    f.control
        .execute(
            "UPDATE workspaces SET owner = ?1 WHERE id = ?2",
            [b.to_storage(), w.to_storage()],
        )
        .unwrap();
    f.restart();
    assert_eq!(
        f.store.root_placement(w).unwrap().unwrap().profile,
        Some(a),
        "the root unit is the placement"
    );
    assert_eq!(f.store.reminders(a).unwrap().len(), 1);
    assert!(
        f.store.reminders(b).unwrap().is_empty(),
        "the personal facade follows the root, not the column"
    );
    assert_eq!(unit(&f, w).deletions().len(), 0);
    // The manifest's own deletion state as an old lifecycle writer is
    // `acceptance::an_old_lifecycle_writer_cannot_undelete_a_unit_holding_workspace`.
}
