// SPDX-License-Identifier: MPL-2.0

//! Plan 1.2 of the root catalog against a real catalog, entirely in named
//! memory: a move is one durable root operation, personal data follows the
//! Workspace, a private destination lands in the overlay, a bundle stages
//! before it publishes. Faults are real SQLite triggers on a second
//! connection to the same catalog and the `FaultBackend` of this module.

use super::*;
use koru_store_core::bundle::ImportMode;
use koru_store_core::ids::{ActionId, DocumentId, ReminderId, RuleRevisionId, UserScopeId};
use koru_store_core::model::{LineageKind, Profile};
use koru_store_core::personal_attributes::{EntityRef, PersonalAttributeValue};
use koru_store_core::ports::{
    RootCatalogOverlay, RootCatalogUnitCommit, RootCatalogUnitRow, RootUnitWrite,
};
use koru_store_core::profile_removal::RemovalKind;
use koru_store_core::reminders::{
    ActionKind, OccurrenceAction, OccurrenceKey, OccurrenceMoment, OccurrenceSource,
    OneTimeSchedule, ReminderRule, RuleStatus,
};
use koru_store_core::root_catalog::seed::{seed_workspace_unit, seeded_move_id};
use koru_store_core::root_catalog::{CatalogUnitRef, Placement, WorkspaceCatalogUnit};
use koru_store_core::service::build_reminder_index;
use koru_store_core::temporal::FloatingDateTime;
use koru_store_core::transport::{
    SetTransportMode, TransportMode, TransportPath, TransportSubject,
};

pub(super) struct Root {
    pub(super) store: Store<SqliteCatalog, FaultBackend>,
    pub(super) backend: FaultBackend,
    pub(super) catalog: koru_sqlite::MemoryName,
    pub(super) control: Connection,
}

impl Root {
    pub(super) fn new() -> Self {
        let backend = FaultBackend::new(None, false);
        let catalog = koru_sqlite::MemoryName::fresh("root-catalog");
        let control = koru_sqlite::open_memory(&catalog).unwrap();
        let store = Store::open(
            SqliteCatalog::in_named_memory(&catalog).unwrap(),
            backend.clone(),
        )
        .unwrap();
        Self {
            store,
            backend,
            catalog,
            control,
        }
    }

    /// A Store restart over the same bytes: the named memory outlives the
    /// handle, as a file would.
    pub(super) fn restart(&mut self) {
        self.store = Store::open(
            SqliteCatalog::in_named_memory(&self.catalog).unwrap(),
            self.backend.clone(),
        )
        .unwrap();
    }

    /// A second handle on the same catalog, for the root rows the Store
    /// keeps to itself.
    pub(super) fn catalog(&self) -> SqliteCatalog {
        SqliteCatalog::in_named_memory(&self.catalog).unwrap()
    }

    pub(super) fn scope(&self) -> UserScopeId {
        self.catalog().local_user_scope().unwrap()
    }

    pub(super) fn profile(&mut self, name: &str) -> ProfileId {
        self.store
            .create_profile(name, None, None, Locale::default(), at())
            .unwrap()
            .id
    }

    pub(super) fn set(&mut self, subject: TransportSubject, expected: i64, mode: TransportMode) {
        self.store
            .set_transport_modes(&[SetTransportMode {
                subject,
                expected_revision: expected,
                mode,
            }])
            .unwrap();
    }

    /// A Cloud Profile, which admits export for anything placed under it.
    pub(super) fn cloud_profile(&mut self, name: &str) -> ProfileId {
        let profile = self.profile(name);
        self.set(TransportSubject::Profile(profile), 1, TransportMode::Cloud);
        profile
    }

    /// A Cloud Profile holding one Workspace whose own explicit choice is
    /// Cloud too: the shape every ceiling question starts from.
    pub(super) fn cloud_pair(&mut self) -> (ProfileId, WorkspaceId) {
        let profile = self.cloud_profile("A");
        let workspace = self.store.create_workspace(profile, "W", at()).unwrap().id;
        self.set(
            TransportSubject::Workspace(workspace),
            1,
            TransportMode::Cloud,
        );
        (profile, workspace)
    }

    pub(super) fn unit(&self, workspace: WorkspaceId) -> Option<RootCatalogUnitRow> {
        self.catalog()
            .root_catalog_unit(self.scope(), CatalogUnitRef::Workspace { workspace })
            .unwrap()
    }

    pub(super) fn overlay(&self, workspace: WorkspaceId) -> Option<RootCatalogOverlay> {
        self.catalog()
            .root_catalog_overlay(self.scope(), CatalogUnitRef::Workspace { workspace })
            .unwrap()
    }

    pub(super) fn durable_placement(&self, workspace: WorkspaceId) -> Option<Placement> {
        self.unit(workspace).and_then(|row| {
            WorkspaceCatalogUnit::from_snapshot(&row.snapshot, workspace, 1)
                .unwrap()
                .placement()
        })
    }

    pub(super) fn fail_on(&self, sql: &str) {
        self.control
            .execute_batch(&format!(
                "CREATE TRIGGER fail {sql} BEGIN SELECT RAISE(ABORT, 'injected fault'); END;"
            ))
            .unwrap();
    }

    pub(super) fn recover(&self) {
        self.control.execute_batch("DROP TRIGGER fail;").unwrap();
    }

    pub(super) fn count(&self, sql: &str) -> i64 {
        self.control.query_row(sql, [], |row| row.get(0)).unwrap()
    }
}

fn text(value: &str) -> PersonalAttributeValue {
    PersonalAttributeValue::Text {
        text: value.to_string(),
    }
}

pub(super) fn reminder_in(workspace: WorkspaceId) -> (EntityRef, ReminderRule) {
    let entity = EntityRef::new(
        "node",
        workspace,
        DocumentId::new(),
        DocumentId::new().as_uuid(),
    )
    .unwrap();
    let rule = ReminderRule {
        id: ReminderId::new(),
        target: entity.clone(),
        revision: RuleRevisionId::new(),
        status: RuleStatus::Enabled,
        schedule: OneTimeSchedule::Floating {
            wall: FloatingDateTime::parse("2026-09-02T09:00:00").unwrap(),
        },
        tone_override: None,
    };
    (entity, rule)
}

fn handled(rule: &ReminderRule) -> OccurrenceAction {
    let OneTimeSchedule::Floating { wall } = &rule.schedule else {
        unreachable!("the fixture rule is floating")
    };
    let source = OccurrenceSource::OneTime {
        moment: OccurrenceMoment::Floating { wall: wall.clone() },
    };
    OccurrenceAction {
        id: ActionId::new(),
        occurrence: OccurrenceKey::derive(rule.id, rule.revision, &source),
        kind: ActionKind::Handled,
    }
}

/// Build and publish the due index for every personal unit placed under
/// `profile`, the way the asynchronous adapter does off-thread.
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

// --------------------------------------------------- personal data follows

#[test]
fn a_move_keeps_personal_data_reminders_actions_and_due_rows_under_the_destination_across_a_restart_and_the_former_profiles_removal()
 {
    let mut f = Root::new();
    let a = f.profile("A");
    let b = f.profile("B");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    let (entity, rule) = reminder_in(w);
    f.store.create_reminder(a, &rule).unwrap();
    let action = handled(&rule);
    f.store.append_reminder_action(a, rule.id, &action).unwrap();
    f.store
        .set_personal_attribute(a, &entity, "cleared", Some(&text("gone")))
        .unwrap();
    f.store
        .set_personal_attribute(a, &entity, "cleared", None)
        .unwrap();
    f.store
        .set_personal_attribute(a, &entity, "kept", Some(&text("stays")))
        .unwrap();
    index(&mut f.store, a);
    assert_eq!(f.store.reminder_index_rows(a).unwrap().len(), 1);
    assert_eq!(f.store.reminders(b).unwrap().len(), 0);

    let moved = f.store.move_workspace(w, b).unwrap();
    assert_eq!(moved.owner, b);

    // What B answers, before and after a restart, and after A is gone.
    let under_b = |store: &mut Store<SqliteCatalog, FaultBackend>| {
        let reminders = store.reminders(b).unwrap();
        assert_eq!(reminders.len(), 1);
        assert_eq!(
            reminders[0].rule.id, rule.id,
            "the reminder id never changes"
        );
        assert_eq!(
            reminders[0].actions,
            vec![action.clone()],
            "the action history came along"
        );
        let attributes = store.personal_attributes(b, &entity).unwrap();
        assert_eq!(attributes.get("kept"), Some(&text("stays")));
        assert!(
            !attributes.contains_key("cleared"),
            "a cleared attribute stays cleared"
        );
        let due = store.reminder_index_rows(b).unwrap();
        assert_eq!(
            due.len(),
            1,
            "the due index is keyed by Workspace and follows for free"
        );
        assert_eq!(due[0].reminder, rule.id);
        assert_eq!(due[0].owner, b);
        assert_eq!(
            store.root_placement(w).unwrap().map(|p| p.profile),
            Some(Some(b))
        );
    };
    // What A no longer answers, while it still exists.
    let not_under_a = |store: &mut Store<SqliteCatalog, FaultBackend>| {
        assert!(
            store.reminders(a).unwrap().is_empty(),
            "A no longer answers for W"
        );
        assert!(store.reminder_index_rows(a).unwrap().is_empty());
    };
    under_b(&mut f.store);
    not_under_a(&mut f.store);
    f.restart();
    under_b(&mut f.store);
    not_under_a(&mut f.store);

    // A can go now: it has no live Workspace, and W is not its business.
    let mark = f
        .store
        .begin_profile_removal(a, RemovalKind::Local, at())
        .unwrap();
    assert_eq!(
        f.store
            .remove_profile_local(a, Some(mark.attempt), at())
            .unwrap(),
        koru_store_core::ports::LocalProfileRemoval::Removed
    );
    assert_eq!(f.store.workspace(w).unwrap().owner, b);
    assert_eq!(
        f.store.reminders(a).unwrap_err().code(),
        Code::ProfileRemoved,
        "the former Profile is gone; nothing of W went with it"
    );
    under_b(&mut f.store);
    f.restart();
    under_b(&mut f.store);
}

// ------------------------------------------------------------ the unit row

#[test]
fn a_move_to_the_same_owner_leaves_the_unit_alone_two_moves_advance_it_and_a_stale_revision_is_refused()
 {
    let mut f = Root::new();
    let (a, w) = f.cloud_pair();
    let a2 = f.cloud_profile("A2");
    let a3 = f.cloud_profile("A3");
    let created = f
        .unit(w)
        .expect("creation publishes the unit with its row (Plan 1.3)");
    assert_eq!(created.revision, 1);
    assert_eq!(f.durable_placement(w).unwrap().profile, Some(a));

    f.store.move_workspace(w, a2).unwrap();
    let first = f.unit(w).unwrap();
    assert_eq!(first.revision, 2);
    assert_eq!(f.durable_placement(w).unwrap().profile, Some(a2));
    assert!(
        f.overlay(w).is_none(),
        "an exportable destination writes the durable unit"
    );

    // Moving to the owner it already has changes nothing, not even the unit.
    let same = f.store.move_workspace(w, a2).unwrap();
    assert_eq!(same.owner, a2);
    assert_eq!(f.unit(w).unwrap(), first);

    f.store.move_workspace(w, a3).unwrap();
    let second = f.unit(w).unwrap();
    assert_eq!(second.revision, 3);
    assert_ne!(second.snapshot, first.snapshot);
    assert_eq!(f.durable_placement(w).unwrap().profile, Some(a3));

    // The compare-and-set: a write that read the revision `first` had cannot
    // land on the one after it, and the projection it carried does not land
    // either.
    let mut stale_row = f.store.workspace(w).unwrap();
    stale_row.owner = a;
    let refused = f
        .catalog()
        .publish_root_unit(
            f.scope(),
            CatalogUnitRef::Workspace { workspace: w },
            RootUnitWrite::Durable {
                commit: RootCatalogUnitCommit {
                    unit: CatalogUnitRef::Workspace { workspace: w },
                    snapshot: first.snapshot.clone(),
                    expected_revision: Some(first.revision),
                },
                clear_overlay: false,
            },
            Some(&stale_row),
        )
        .unwrap_err();
    assert_eq!(refused.code(), Code::IdempotencyConflict);
    assert_eq!(f.unit(w).unwrap(), second);
    assert_eq!(f.store.workspace(w).unwrap().owner, a3);
}

#[test]
fn a_failed_root_publication_leaves_owner_unit_overlay_and_generation_untouched() {
    let mut f = Root::new();
    let (a, w) = f.cloud_pair();
    let a2 = f.cloud_profile("A2");
    let generation = f.store.policy_generation();

    // The unit exists from creation (Plan 1.3), so the placement write is the
    // compare-and-set *update* of that row: arming `BEFORE INSERT` here would
    // inject no fault at all.
    let created = f.unit(w).expect("creation published the unit");
    for fault in [
        "BEFORE UPDATE ON root_catalog_units",
        "BEFORE UPDATE ON workspaces",
    ] {
        f.fail_on(fault);
        assert_eq!(
            f.store.move_workspace(w, a2).unwrap_err().code(),
            Code::StorageFailed,
            "{fault}"
        );
        f.recover();
        assert_eq!(f.store.workspace(w).unwrap().owner, a, "{fault}");
        assert_eq!(
            f.unit(w).unwrap(),
            created,
            "{fault}: the placement write rolled back"
        );
        assert_eq!(f.durable_placement(w).unwrap().profile, Some(a), "{fault}");
        assert!(f.overlay(w).is_none(), "{fault}");
        assert_eq!(
            f.store.policy_generation(),
            generation,
            "{fault}: a failed move invalidates nothing"
        );
        assert_eq!(
            f.store
                .transport_mode(TransportSubject::Workspace(w))
                .unwrap()
                .owner,
            a,
            "{fault}: the mode row stayed filed under the old owner"
        );
    }

    f.store.move_workspace(w, a2).unwrap();
    assert_eq!(f.unit(w).unwrap().revision, 2);
    assert_ne!(f.store.policy_generation(), generation);

    // The overlay path fails the same way: the private row is the only
    // thing the transaction writes about the unit, and it rolls back whole.
    let b = f.profile("B");
    f.fail_on("BEFORE INSERT ON root_catalog_overlays");
    assert!(f.store.move_workspace(w, b).is_err());
    f.recover();
    assert_eq!(f.store.workspace(w).unwrap().owner, a2);
    assert!(f.overlay(w).is_none());
    assert_eq!(f.unit(w).unwrap().revision, 2);
}

// ---------------------------------------------------- the private overlay

#[test]
fn a_move_into_a_private_profile_waits_in_an_overlay_and_is_published_when_both_subjects_allow_p2p()
{
    let mut f = Root::new();
    let (_a, w) = f.cloud_pair();
    let a2 = f.cloud_profile("A2");
    f.store.move_workspace(w, a2).unwrap();
    let durable_before = f.unit(w).unwrap();
    // B is Local-only: migration 12's seeded default, nobody's choice.
    let b = f.profile("B");
    let generation = f.store.policy_generation();

    let moved = f.store.move_workspace(w, b).unwrap();
    assert_eq!(moved.owner, b, "the local move succeeds (§9)");

    // The durable unit is byte for byte what it was; the overlay holds the
    // new placement under its own writer.
    assert_eq!(f.unit(w).unwrap(), durable_before);
    assert_eq!(f.durable_placement(w).unwrap().profile, Some(a2));
    let overlay = f.overlay(w).unwrap();
    assert_ne!(overlay.writer_peer_id, f.store.peer_id());
    let private = WorkspaceCatalogUnit::from_snapshot(&overlay.private_snapshot, w, 1)
        .unwrap()
        .placement()
        .unwrap();
    assert_eq!(private.profile, Some(b));
    assert_eq!(
        f.store.root_placement(w).unwrap(),
        Some(private),
        "reads prefer the overlay"
    );
    // Nothing about the new placement may leave: the seam answers nothing.
    assert_eq!(
        f.store
            .eligible_export_source(
                CatalogUnitRef::Workspace { workspace: w },
                TransportPath::Peer
            )
            .unwrap(),
        None
    );
    // The ceiling mismatch is reported, not repaired, and the choice is kept.
    let admission = f
        .store
        .transport_admission(TransportSubject::Workspace(w))
        .unwrap();
    assert!(admission.ceiling_mismatch);
    assert_eq!(admission.stored_mode, TransportMode::Cloud);
    assert!(admission.explicit_choice);
    assert_eq!(admission.mode, TransportMode::LocalOnly);
    assert_eq!(
        admission.revision, 2,
        "the mode row's revision did not move"
    );
    assert_eq!(
        f.store
            .admit_transport(TransportSubject::Workspace(w), TransportPath::Cloud)
            .unwrap_err()
            .code(),
        Code::CorruptState
    );
    assert_ne!(
        f.store.policy_generation(),
        generation,
        "late callbacks recheck"
    );
    let row = f
        .store
        .transport_mode(TransportSubject::Workspace(w))
        .unwrap();
    assert_eq!(row.owner, b, "re-keyed in the move's transaction");
    f.restart();
    assert_eq!(f.store.root_placement(w).unwrap(), Some(private));

    // Enabling P2P on the Profile and the Workspace in one group makes W
    // exportable, and the commit path publishes the overlay.
    f.store
        .set_transport_modes(&[
            SetTransportMode {
                subject: TransportSubject::Profile(b),
                expected_revision: 1,
                mode: TransportMode::P2p,
            },
            SetTransportMode {
                subject: TransportSubject::Workspace(w),
                expected_revision: 2,
                mode: TransportMode::P2p,
            },
        ])
        .unwrap();
    assert!(f.overlay(w).is_none(), "cleared with the publication");
    let published = f.unit(w).unwrap();
    assert_eq!(
        published.revision, 3,
        "creation, the first move, then the overlay's publication"
    );
    assert_eq!(
        f.durable_placement(w),
        Some(private),
        "the overlay's own move, not a new one"
    );
    assert_eq!(f.store.root_placement(w).unwrap(), Some(private));
    assert_eq!(
        f.store
            .eligible_export_source(
                CatalogUnitRef::Workspace { workspace: w },
                TransportPath::Peer
            )
            .unwrap(),
        Some(published.snapshot)
    );
    // Publishing again has nothing to do.
    assert!(
        !f.store
            .publish_pending_overlay(CatalogUnitRef::Workspace { workspace: w })
            .unwrap()
    );
}

#[test]
fn a_workspace_with_no_unit_moved_into_a_private_profile_exists_only_in_its_overlay_until_a_move_admits_export()
 {
    let mut f = Root::new();
    let (_a, w) = f.cloud_pair();
    // A Workspace from before catalog 13 that the seed never reached: the one
    // unit-less state STORE.md still tolerates. Creation publishes a unit
    // since Plan 1.3, so the state is made here by removing that row.
    f.control
        .execute(
            "DELETE FROM root_catalog_units WHERE unit_kind = 'workspace_catalog'",
            [],
        )
        .unwrap();
    assert!(f.unit(w).is_none());
    let b = f.profile("B");
    f.store.move_workspace(w, b).unwrap();
    assert!(f.unit(w).is_none());
    let overlay = f.overlay(w).unwrap();
    assert_eq!(overlay.base_vv, loro::VersionVector::new().encode());
    assert_eq!(
        f.store.root_placement(w).unwrap().map(|p| p.profile),
        Some(Some(b))
    );

    // A later move to an exportable Profile folds the private history into
    // a fresh durable unit and clears the overlay in the same transaction.
    let c = f.cloud_profile("C");
    f.store.move_workspace(w, c).unwrap();
    assert!(f.overlay(w).is_none());
    let unit = f.unit(w).unwrap();
    assert_eq!(unit.revision, 1);
    let durable = WorkspaceCatalogUnit::from_snapshot(&unit.snapshot, w, 1).unwrap();
    assert_eq!(durable.placement().unwrap().profile, Some(c));
    assert!(
        durable.version().get(&overlay.writer_peer_id).is_some(),
        "the private move to B kept its causal place"
    );
}

// --------------------------------------------------------------- holders

#[test]
fn a_placement_under_a_holder_reads_as_unassigned() {
    let mut f = Root::new();
    let a = f.profile("A");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    let (_, rule) = reminder_in(w);
    f.store.create_reminder(a, &rule).unwrap();
    assert_eq!(f.store.reminders(a).unwrap().len(), 1);

    // What the migration seeds for a legacy Workspace owned by a Holder row,
    // written over the unit creation published (Plan 1.3) as the migration
    // would have written it in its place.
    let holder = Profile::holder(Locale::default(), at()).unwrap();
    let created = f.unit(w).expect("creation published the unit").revision;
    let mut catalog = f.catalog();
    catalog.put_profile(&holder).unwrap();
    catalog
        .commit_root_catalog_units(
            f.scope(),
            &[RootCatalogUnitCommit {
                unit: CatalogUnitRef::Workspace { workspace: w },
                snapshot: seed_workspace_unit(w, holder.id, None, f.store.peer_id()).unwrap(),
                expected_revision: Some(created),
            }],
        )
        .unwrap();

    assert_eq!(
        f.store.root_placement(w).unwrap(),
        Some(Placement {
            profile: None,
            move_id: seeded_move_id(w),
        }),
        "the register still names the Holder; the reading does not"
    );
    assert!(
        f.store.reminders(a).unwrap().is_empty(),
        "the root unit, not the owner projection, says where W is"
    );
    assert!(
        f.store.reminders(holder.id).unwrap().is_empty(),
        "a Holder places nothing"
    );
    assert_eq!(
        f.store.move_workspace(w, holder.id).unwrap_err().code(),
        Code::ProfileReserved,
        "and is never a destination"
    );
}

// --------------------------------------------------------- bundle import

#[test]
fn an_import_publishes_unit_row_locations_and_receipt_together_after_the_content() {
    let mut f = Root::new();
    let a = f.profile("A");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    f.store
        .create_document(w, "One", "page", None, None, at())
        .unwrap();
    let bundle = f.store.export_workspace(w, at()).unwrap();

    let mut there = Root::new();
    let b = there.profile("B");
    let restored = there
        .store
        .import_workspace(&bundle, ImportMode::Restore, b, at())
        .unwrap()
        .workspace;
    assert_eq!(restored, w);
    let unit = there.unit(w).unwrap();
    assert_eq!(unit.revision, 1);
    assert_eq!(there.durable_placement(w).unwrap().profile, Some(b));
    assert_eq!(there.store.workspace(w).unwrap().owner, b);
    for document in &bundle.manifest.documents {
        assert_eq!(there.store.locate(document.id).unwrap(), Some(w));
    }
    let receipt = there.store.lineage(w).unwrap();
    assert_eq!(receipt.len(), 1);
    assert_eq!(receipt[0].kind, LineageKind::Restore);
    assert_eq!(receipt[0].derived_from, w);
    assert_eq!(receipt[0].at, at());
    // The same bundle again cannot list a second Workspace.
    assert_eq!(
        there
            .store
            .import_workspace(&bundle, ImportMode::Restore, b, at())
            .unwrap_err()
            .code(),
        Code::WorkspaceExists
    );
    assert_eq!(there.store.workspaces_including_deleted().unwrap().len(), 1);
}

#[test]
fn a_failed_import_publication_lists_nothing_while_the_staged_repository_was_written_and_cleaned() {
    let mut f = Root::new();
    let a = f.profile("A");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    f.store
        .create_document(w, "One", "page", None, None, at())
        .unwrap();
    let bundle = f.store.export_workspace(w, at()).unwrap();

    // The publication's last and first statements, and a staging fault.
    for fault in [
        Some("BEFORE INSERT ON workspace_lineage"),
        Some("BEFORE INSERT ON root_catalog_units"),
        None,
    ] {
        let mut there = Root::new();
        let b = there.profile("B");
        match fault {
            Some(sql) => there.fail_on(sql),
            None => there.backend.0.fault.set(Some("journal")),
        }
        let error = there
            .store
            .import_workspace(&bundle, ImportMode::Restore, b, at())
            .unwrap_err();
        assert_eq!(error.code(), Code::StorageFailed, "{fault:?}");
        assert!(
            there.store.workspace_including_deleted(w).is_err(),
            "{fault:?}: no workspaces row"
        );
        assert!(there.unit(w).is_none(), "{fault:?}: no unit");
        assert_eq!(
            there.count("SELECT COUNT(*) FROM document_locations"),
            0,
            "{fault:?}"
        );
        assert_eq!(
            there.count("SELECT COUNT(*) FROM workspace_lineage"),
            0,
            "{fault:?}"
        );
        assert_eq!(
            there.backend.0.created.borrow().as_slice(),
            &[w],
            "{fault:?}: the content was staged before the publication"
        );
        assert_eq!(
            there.backend.0.deleted.borrow().as_slice(),
            &[w],
            "{fault:?}: and the claimed repository was cleaned up"
        );
        assert!(there.store.take_committed().is_empty(), "{fault:?}");
    }
}

#[test]
fn a_restore_meets_root_deletion_evidence_but_a_duplicate_mints_a_new_unit() {
    let mut f = Root::new();
    let (_a, w) = f.cloud_pair();
    let a2 = f.cloud_profile("A2");
    f.store.move_workspace(w, a2).unwrap();
    let bundle = f.store.export_workspace(w, at()).unwrap();

    // The lifecycle writer records the deletion fact on the root unit (Plan
    // 1.3), then the retention purge removes every content row.
    f.store.delete_workspace(w, at()).unwrap();
    let deleted = WorkspaceCatalogUnit::from_snapshot(&f.unit(w).unwrap().snapshot, w, 1).unwrap();
    assert_eq!(deleted.active_deletions().len(), 1);
    let expiry = Timestamp::from_millis(at().millis() + koru_store_core::TRASH_RETENTION_MS);
    assert_eq!(f.store.purge_deleted(expiry).unwrap().purged, vec![w]);
    assert!(
        f.store.workspace_including_deleted(w).is_err(),
        "the row is gone"
    );
    assert!(f.unit(w).is_some(), "the evidence is not");

    let refused = f
        .store
        .import_workspace(&bundle, ImportMode::Restore, a2, at())
        .unwrap_err();
    assert_eq!(refused.code(), Code::WorkspaceExists);
    assert!(
        refused.message().contains("deleted here"),
        "{}",
        refused.message()
    );
    assert!(f.store.workspace_including_deleted(w).is_err());

    let copy = f
        .store
        .import_workspace(&bundle, ImportMode::Duplicate, a2, at())
        .unwrap()
        .workspace;
    assert_ne!(copy, w);
    assert_eq!(f.unit(copy).unwrap().revision, 1);
    assert_eq!(f.durable_placement(copy).unwrap().profile, Some(a2));
    let lineage = f.store.lineage(copy).unwrap();
    assert_eq!(lineage.len(), 1);
    assert_eq!(lineage[0].kind, LineageKind::Duplicate);
    assert_eq!(lineage[0].derived_from, w);
}

// ------------------------------------------- Plan 3.1: the account path

/// The Store half of the account binding (root-catalog TECHNICAL.final
/// §4.1) and the Cloud variant of the export gate (§4.4): one account per
/// catalog, never merged; a P2P-only subject leaves for a peer and not for
/// Cloud; the account relationship stages and commits through the same join
/// as a peer, gated by the Cloud question.
#[test]
fn the_account_scope_binds_once_refuses_a_second_account_and_gates_export_by_path() {
    use koru_store_core::root_catalog::{DeletionRecord, ProfileCatalogUnit};
    use koru_store_core::service::{AccountScopeBinding, RootGrant, RootJoinOutcome};
    use koru_store_core::{Code, DeletionId};

    let mut f = Root::new();
    let (cloud_profile, cloud_workspace) = f.cloud_pair();
    // A second Profile the person keeps between paired devices only.
    let p2p_profile = f.profile("B");
    f.set(
        TransportSubject::Profile(p2p_profile),
        1,
        TransportMode::P2p,
    );
    let p2p_workspace = f.store.create_workspace(p2p_profile, "V", at()).unwrap().id;
    f.set(
        TransportSubject::Workspace(p2p_workspace),
        1,
        TransportMode::P2p,
    );

    // The binding: idempotent for the same account and scope, refused for
    // another account or a renamed scope, and durable across a restart.
    let (account, scope_id) = (UserScopeId::new().as_uuid(), UserScopeId::new().as_uuid());
    assert_eq!(f.store.account_binding().unwrap(), None);
    assert_eq!(f.store.account_scope(account).unwrap(), None);
    let bound = f.store.bind_account_scope(account, scope_id, 5).unwrap();
    assert_eq!(bound, f.scope());
    assert_eq!(
        f.store.bind_account_scope(account, scope_id, 9).unwrap(),
        bound
    );
    assert_eq!(
        f.store.account_binding().unwrap(),
        Some(AccountScopeBinding {
            account_id: account,
            catalog_scope_id: scope_id,
            scope: bound,
            bound_at_ms: 5,
        }),
        "the second bind rewrote nothing"
    );
    let other = UserScopeId::new().as_uuid();
    assert_eq!(
        f.store
            .bind_account_scope(other, UserScopeId::new().as_uuid(), 9)
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
    assert_eq!(
        f.store
            .bind_account_scope(account, UserScopeId::new().as_uuid(), 9)
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
    assert_eq!(f.store.account_scope(account).unwrap(), Some(bound));
    assert_eq!(f.store.account_scope(other).unwrap(), None);
    f.restart();
    assert_eq!(f.store.account_scope(account).unwrap(), Some(bound));

    // The export gate by path: the Cloud pair leaves on both paths, the P2P
    // pair on the peer path only, and nothing of the P2P Profile is visible
    // to the account's Cloud grant.
    let cloud_unit = CatalogUnitRef::Workspace {
        workspace: cloud_workspace,
    };
    let p2p_unit = CatalogUnitRef::Workspace {
        workspace: p2p_workspace,
    };
    for path in [TransportPath::Peer, TransportPath::Cloud] {
        assert!(
            f.store
                .eligible_export_source(cloud_unit, path)
                .unwrap()
                .is_some()
        );
    }
    assert!(
        f.store
            .eligible_export_source(p2p_unit, TransportPath::Peer)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        f.store
            .eligible_export_source(p2p_unit, TransportPath::Cloud)
            .unwrap(),
        None
    );
    let account_grant = RootGrant::account();
    let visible = f.store.exportable_profiles(&account_grant).unwrap();
    assert!(visible.contains(&cloud_profile));
    assert!(!visible.contains(&p2p_profile));
    let inventory = f.store.root_export_inventory(&account_grant).unwrap();
    let listed: Vec<_> = inventory.units.iter().map(|(unit, _)| *unit).collect();
    assert!(listed.contains(&cloud_unit));
    assert!(listed.contains(&CatalogUnitRef::Profile {
        profile: cloud_profile
    }));
    assert!(!listed.contains(&p2p_unit));
    assert!(!listed.contains(&CatalogUnitRef::Profile {
        profile: p2p_profile
    }));
    // `held` counts only what the recipient could have known: the P2P-only
    // Workspace is one the account grant covers and the mode withholds, so it
    // is a number; the Profile the grant cannot see — and therefore its unit —
    // is not even that. Two would mean the hidden Profile had been counted.
    assert_eq!(
        inventory.held, 1,
        "the hidden Profile's unit was counted as well"
    );
    let peer_grant = RootGrant {
        path: TransportPath::Peer,
        ..RootGrant::account()
    };
    let peer_listed: Vec<_> = f
        .store
        .root_export_inventory(&peer_grant)
        .unwrap()
        .units
        .iter()
        .map(|(unit, _)| *unit)
        .collect();
    assert!(peer_listed.contains(&p2p_unit));

    // The account relationship joins through the same stage/commit as a
    // peer: a trash fact for the Cloud Workspace, authored elsewhere, lands
    // on this device's unit.
    let row = f.unit(cloud_workspace).unwrap();
    let remote =
        WorkspaceCatalogUnit::from_snapshot(&row.snapshot, cloud_workspace, 4_242).unwrap();
    let before = remote.version();
    remote
        .record_deletion(DeletionId::new(), &DeletionRecord::new(10, None))
        .unwrap();
    let update = remote.updates_since(&before).unwrap();
    assert_eq!(
        f.store
            .root_unit_frontier(cloud_unit, TransportPath::Cloud)
            .unwrap(),
        Some(before.clone())
    );
    let staged = f
        .store
        .stage_root_unit(account, cloud_unit, &[update])
        .unwrap();
    assert!(!staged.is_pending());
    let outcome = f
        .store
        .commit_staged_root_unit(account, staged, None)
        .unwrap();
    assert!(matches!(
        outcome,
        RootJoinOutcome::Applied {
            lifecycle_changed: true,
            ..
        }
    ));
    assert!(
        f.store
            .workspace_including_deleted(cloud_workspace)
            .unwrap()
            .deleted_at
            .is_some()
    );
    // A P2P subject refuses inbound root metadata over the Cloud path
    // (§4.4: the mode closes the path in both directions) ...
    assert_eq!(
        f.store
            .root_unit_frontier(p2p_unit, TransportPath::Cloud)
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
    assert_eq!(
        f.store
            .stage_root_unit(account, p2p_unit, &[Vec::new()])
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
    // ... and an unbound relationship stages nothing at all.
    let p2p_profile_unit = ProfileCatalogUnit::create(p2p_profile, 1).unwrap();
    assert_eq!(
        f.store
            .stage_root_unit(
                other,
                CatalogUnitRef::Profile {
                    profile: p2p_profile
                },
                &[p2p_profile_unit.snapshot().unwrap()]
            )
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
}

// ------------------- Q10: the unit the v1 Cloud window withheld

use koru_store_core::model::Workspace;
use koru_store_core::service::{ProvisionedReplica, ReplicaPlacement, RootGrant};

/// A Workspace this device bootstrapped from Cloud during the v1 window: the
/// content and the row exist, placed under `profile` by the descriptor's
/// legacy owner column, and **no** Workspace unit was published
/// (`ReplicaPlacement::HintWithoutUnit`, QUESTIONS.md Q7).
fn window_bootstrap(f: &mut Root, profile: ProfileId, name: &str) -> WorkspaceId {
    let row = Workspace::new(profile, name, at()).unwrap();
    let provisioned = f
        .store
        .provision_replica_placed(
            None,
            &row,
            DocumentId::new(),
            ReplicaPlacement::HintWithoutUnit(profile),
            at(),
        )
        .unwrap();
    assert_eq!(provisioned, ProvisionedReplica::Ready);
    assert!(f.unit(row.id).is_none(), "the window withholds the unit");
    row.id
}

#[test]
fn the_repair_publishes_the_withheld_unit_placed_by_the_hint_with_a_fresh_move_id() {
    let mut f = Root::new();
    let profile = f.cloud_profile("A");
    let workspace = window_bootstrap(&mut f, profile, "W");
    // Before the repair the Workspace is absent from the account's export
    // inventory, which is the whole defect: no Cloud placement for it.
    let listed = |f: &Root| -> bool {
        f.store
            .root_export_inventory(&RootGrant::account())
            .unwrap()
            .units
            .iter()
            .any(|(unit, _)| *unit == CatalogUnitRef::Workspace { workspace })
    };
    assert!(!listed(&f));

    let repaired = f.store.repair_bootstrap_units(at()).unwrap();

    assert_eq!(repaired.len(), 1);
    assert_eq!(repaired[0].workspace, workspace);
    assert!(repaired[0].durable, "a Cloud Profile admits export");
    let placement = f.durable_placement(workspace).expect("the unit exists now");
    assert_eq!(
        placement.profile,
        Some(profile),
        "placed by the hint the row projected"
    );
    assert_eq!(placement.move_id, repaired[0].repair.move_id);
    assert_ne!(
        placement.move_id,
        seeded_move_id(workspace),
        "a fresh move id, never the deterministic seed two devices would agree on"
    );
    assert!(listed(&f), "the Workspace is in the export inventory now");
    // The row itself did not move: the repair writes a unit, not a placement
    // change, so there is nothing to project.
    let row = f.store.workspace_including_deleted(workspace).unwrap();
    assert_eq!(row.owner, profile);
}

#[test]
fn the_journal_record_identifies_the_repair_operation_and_survives_a_restart() {
    let mut f = Root::new();
    let profile = f.cloud_profile("A");
    let workspace = window_bootstrap(&mut f, profile, "W");

    let repaired = f.store.repair_bootstrap_units(at()).unwrap();
    let written = repaired[0].repair;
    f.restart();

    let record = f
        .store
        .bootstrap_repair(workspace)
        .unwrap()
        .expect("the record is durable");
    assert_eq!(record, written);
    assert_eq!(record.profile, profile);
    assert_eq!(record.at_ms, at().millis());
    // The identity is the one operation `set_placement` wrote, and it names a
    // place in the unit's own history — which is what the later concurrency
    // check compares an arriving placement write against.
    let row = f.unit(workspace).unwrap();
    let doc = loro::LoroDoc::new();
    doc.import(&row.snapshot).unwrap();
    assert_eq!(doc.oplog_frontiers(), record.frontiers());
    assert_eq!(
        doc.cmp_frontiers(&record.frontiers(), &doc.oplog_frontiers()),
        Ok(Some(std::cmp::Ordering::Equal)),
        "the recorded operation is in the published unit's history"
    );
    assert!(
        f.store
            .bootstrap_repair(WorkspaceId::new())
            .unwrap()
            .is_none(),
        "one key per Workspace; an untouched Workspace has no record"
    );
}

#[test]
fn a_workspace_that_already_has_a_unit_is_never_repaired() {
    let mut f = Root::new();
    let (profile, ordinary) = f.cloud_pair();
    let withheld = window_bootstrap(&mut f, profile, "W2");
    let before = f.unit(ordinary).expect("creation publishes a unit");

    let repaired = f.store.repair_bootstrap_units(at()).unwrap();

    assert_eq!(
        repaired.iter().map(|one| one.workspace).collect::<Vec<_>>(),
        vec![withheld],
        "only the unit-less row is a candidate"
    );
    assert_eq!(
        f.unit(ordinary).unwrap(),
        before,
        "the Workspace that had a unit was not rewritten, revision included"
    );
    assert!(f.store.bootstrap_repair(ordinary).unwrap().is_none());

    // And the repair does not run twice: the row now has a unit, so a second
    // pass finds no candidate and leaves the first record exactly as it was.
    let first = f.store.bootstrap_repair(withheld).unwrap().unwrap();
    assert!(f.store.repair_bootstrap_units(at()).unwrap().is_empty());
    assert_eq!(f.store.bootstrap_repair(withheld).unwrap().unwrap(), first);
}

#[test]
fn a_local_only_hint_profile_repairs_into_the_private_overlay_and_stays_out_of_the_inventory() {
    let mut f = Root::new();
    // Migration 12 seeds every row Local-only and only a person opens it, so
    // this is the ordinary state of a Profile nobody has answered for.
    let profile = f.profile("A");
    let workspace = window_bootstrap(&mut f, profile, "W");

    let repaired = f.store.repair_bootstrap_units(at()).unwrap();

    assert_eq!(repaired.len(), 1);
    assert!(
        !repaired[0].durable,
        "the destination Profile's ceiling routes the write"
    );
    assert!(
        f.unit(workspace).is_none(),
        "the durable bytes are untouched"
    );
    let overlay = f.overlay(workspace).expect("the write is in the overlay");
    assert_eq!(
        repaired[0].repair.writer_peer, overlay.writer_peer_id,
        "the record names the writer the operation was actually authored under"
    );
    // Q5/D258: the Local-only rule working, not a residual defect. The
    // placement is live on this device and leaves it nowhere.
    assert_eq!(
        f.store
            .root_placement(workspace)
            .unwrap()
            .map(|p| p.profile),
        Some(Some(profile))
    );
    assert!(
        !f.store
            .root_export_inventory(&RootGrant::account())
            .unwrap()
            .units
            .iter()
            .any(|(unit, _)| *unit == CatalogUnitRef::Workspace { workspace })
    );
    // And the existing pending-overlay path publishes it once the person
    // answers the Profile prompt for the Profile and its Workspaces (D258) —
    // the transport-mode commit runs that publication itself.
    f.store
        .choose_transport_mode(profile, TransportMode::Cloud, true)
        .unwrap();
    assert!(f.overlay(workspace).is_none());
    assert_eq!(
        f.durable_placement(workspace).unwrap().move_id,
        repaired[0].repair.move_id,
        "the repair's own placement is what finally leaves the device"
    );
    assert_eq!(
        f.store.bootstrap_repair(workspace).unwrap().unwrap(),
        repaired[0].repair,
        "publishing the overlay does not rewrite the record"
    );
    assert!(
        f.store
            .root_export_inventory(&RootGrant::account())
            .unwrap()
            .units
            .iter()
            .any(|(unit, _)| *unit == CatalogUnitRef::Workspace { workspace })
    );
}

#[test]
fn a_trashed_unit_less_workspace_is_left_to_the_lifecycle_writers() {
    let mut f = Root::new();
    let profile = f.cloud_profile("A");
    let mut row = Workspace::new(profile, "W", at()).unwrap();
    row.deleted_at = Some(at());
    f.store
        .provision_replica_placed(
            None,
            &row,
            DocumentId::new(),
            ReplicaPlacement::HintWithoutUnit(profile),
            at(),
        )
        .unwrap();
    assert!(f.unit(row.id).is_none());

    assert!(f.store.repair_bootstrap_units(at()).unwrap().is_empty());

    assert!(
        f.unit(row.id).is_none(),
        "a live placement must not be asserted over a row that is in the Trash"
    );
    assert!(f.store.bootstrap_repair(row.id).unwrap().is_none());
}
