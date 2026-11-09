// SPDX-License-Identifier: MPL-2.0

//! Root-catalog Plan 4.1 against a real catalog, entirely in named memory
//! (`docs/features/root-catalog/VERIFICATION.md`): crash injection at every
//! durable seam the Store writes after the migration, and the adversarial
//! matrix of TECHNICAL.final §9 where one Store suffices. Faults are real
//! SQLite triggers on a second connection to the same catalog, the
//! `FaultBackend` of this module, and a trigger on a Workspace's own
//! repository for the one effect that is written after the root commit.
//!
//! The shape of every seam test is the same: dump the catalog's root and
//! projection tables, fault one statement group, run the operation, lift
//! the fault, and assert the dump is byte for byte what it was and nothing
//! was acknowledged; then run the operation again and assert it completed
//! once — one fact, one row, one receipt. Each test names the seam in its
//! name so `VERIFICATION.md`'s evidence table can cite it.

use super::root_catalog::{Root, reminder_in};
use super::*;
use koru_store_core::bundle::{Bundle, ImportMode, ImportPublication, ImportedWorkspace};
use koru_store_core::ids::{DeletionId, DocumentId, UserScopeId};
use koru_store_core::model::{DocumentKind, LineageKind};
use koru_store_core::ports::{
    HistoryReason, PurgeHold, RootCatalogUnitCommit, UpdateBatch, UpdateOrigin, WorkspaceRepo,
};
use koru_store_core::profile_units::PersonalManifest;
use koru_store_core::root_catalog::{
    CatalogUnitRef, DeletionRecord, Derived, ExportSubject, ProfileCatalogUnit,
    WorkspaceCatalogUnit,
};
use koru_store_core::service::{
    EnsuredWorkspace, JoinPhase, JoinUnit, RootGrant, RootJoinOutcome, build_reminder_index,
};
use koru_store_core::transport::{
    SetTransportMode, TransportMode, TransportPath, TransportSubject,
};
use koru_store_core::{Manifest, TRASH_RETENTION_MS};
use std::collections::BTreeMap;

/// The catalog tables a root operation may touch, dumped row by row. A
/// failed operation must leave every one of them exactly as it was.
const ROOT_TABLES: [&str; 9] = [
    "root_catalog_units",
    "root_catalog_overlays",
    "workspaces",
    "profiles",
    "store_state",
    "document_locations",
    "local_transport_modes",
    "workspace_personal_crdts",
    "workspace_reminder_due",
];

fn dump(f: &Root) -> BTreeMap<&'static str, Vec<String>> {
    let mut out = BTreeMap::new();
    for table in ROOT_TABLES {
        let mut statement = f
            .control
            .prepare(&format!("SELECT * FROM {table}"))
            .unwrap();
        let columns = statement.column_count();
        let mut rows: Vec<String> = statement
            .query_map([], |row| {
                let mut text = String::new();
                for index in 0..columns {
                    // Readable for the id and enum columns a drift is usually
                    // about; exact, so a changed blob still fails.
                    match row.get_ref(index)? {
                        rusqlite::types::ValueRef::Null => text.push_str("null"),
                        rusqlite::types::ValueRef::Integer(value) => {
                            text.push_str(&value.to_string())
                        }
                        rusqlite::types::ValueRef::Real(value) => text.push_str(&value.to_string()),
                        rusqlite::types::ValueRef::Text(value) => {
                            text.push_str(&String::from_utf8_lossy(value))
                        }
                        rusqlite::types::ValueRef::Blob(value) => {
                            text.push_str(&format!("blob:{value:?}"))
                        }
                    }
                    text.push('|');
                }
                Ok(text)
            })
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        rows.sort();
        out.insert(table, rows);
    }
    out
}

/// Fault each statement group in turn: the operation fails with
/// `StorageFailed`, the dump is unchanged, nothing was acknowledged.
fn faulted(f: &mut Root, faults: &[&str], mut operation: impl FnMut(&mut Root) -> Result<()>) {
    for fault in faults {
        let before = dump(f);
        // Only this operation's acknowledgements are the question, so the
        // queue starts empty: the fixture's own setup (creation, a document)
        // acknowledges writes of its own.
        f.store.take_committed();
        f.fail_on(fault);
        let error = operation(f).expect_err(fault);
        assert_eq!(error.code(), Code::StorageFailed, "{fault}");
        f.recover();
        // Table by table, so a drift says which table it is rather than
        // printing nine of them.
        let after = dump(f);
        for table in ROOT_TABLES {
            assert_eq!(
                after[table], before[table],
                "{fault}: {table} is not the pre-state"
            );
        }
        assert!(
            f.store.take_committed().is_empty(),
            "{fault}: a failed root operation acknowledged something"
        );
    }
}

fn workspace_unit(f: &Root, workspace: WorkspaceId) -> WorkspaceCatalogUnit {
    match f.overlay(workspace) {
        Some(overlay) => {
            WorkspaceCatalogUnit::from_snapshot(&overlay.private_snapshot, workspace, 1).unwrap()
        }
        None => {
            WorkspaceCatalogUnit::from_snapshot(&f.unit(workspace).unwrap().snapshot, workspace, 1)
                .unwrap()
        }
    }
}

fn profile_unit(f: &Root, profile: ProfileId) -> Option<ProfileCatalogUnit> {
    let unit = CatalogUnitRef::Profile { profile };
    let catalog = f.catalog();
    if let Some(overlay) = catalog.root_catalog_overlay(f.scope(), unit).unwrap() {
        return Some(
            ProfileCatalogUnit::from_snapshot(&overlay.private_snapshot, profile, 1).unwrap(),
        );
    }
    catalog
        .root_catalog_unit(f.scope(), unit)
        .unwrap()
        .map(|row| ProfileCatalogUnit::from_snapshot(&row.snapshot, profile, 1).unwrap())
}

/// Another device's converged write landing in the durable Workspace unit.
fn remote_edit(f: &Root, workspace: WorkspaceId, edit: impl Fn(&WorkspaceCatalogUnit)) {
    assert!(
        f.overlay(workspace).is_none(),
        "a remote edit reaches the durable unit"
    );
    let row = f.unit(workspace).unwrap();
    let unit = WorkspaceCatalogUnit::from_snapshot(&row.snapshot, workspace, 77).unwrap();
    edit(&unit);
    f.catalog()
        .commit_root_catalog_units(
            f.scope(),
            &[RootCatalogUnitCommit {
                unit: CatalogUnitRef::Workspace { workspace },
                snapshot: unit.snapshot().unwrap(),
                expected_revision: Some(row.revision),
            }],
        )
        .unwrap();
}

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

fn after_retention(from: Timestamp) -> Timestamp {
    Timestamp::from_millis(from.millis() + TRASH_RETENTION_MS)
}

/// Arm one Profile's group, children included, as a person does on this
/// device: lowering carries the children, raising lifts them with it.
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

/// Every byte of a frame's history as JSON, keys included, so a hidden id
/// can be searched for wherever a Loro value could carry it.
fn history_json(bytes: &[u8]) -> serde_json::Value {
    let doc = loro::LoroDoc::new();
    let status = doc.import(bytes).unwrap();
    assert!(status.pending.is_none(), "derived bytes apply completely");
    serde_json::to_value(doc.export_json_updates_without_peer_compression(
        &loro::VersionVector::default(),
        &doc.oplog_vv(),
    ))
    .unwrap()
}

fn mentions(value: &serde_json::Value, needle: &str) -> bool {
    match value {
        serde_json::Value::String(text) => text.contains(needle),
        serde_json::Value::Array(items) => items.iter().any(|item| mentions(item, needle)),
        serde_json::Value::Object(map) => map
            .iter()
            .any(|(key, item)| key.contains(needle) || mentions(item, needle)),
        _ => false,
    }
}

fn peer_grant() -> RootGrant<'static> {
    RootGrant {
        path: TransportPath::Peer,
        ..RootGrant::account()
    }
}

// ============================================================ A1: seams

// ------------------------------------------------- publish_root: move

/// Seam: `publish_root` for a move — the unit's compare-and-set, the
/// `workspaces` projection and the mode row re-keyed with it. Each
/// statement group faulted; then the move completes once, with one unit
/// revision step and one re-keyed row.
#[test]
fn seam_publish_root_move_faults_at_each_statement_group_roll_back_whole_and_the_retry_moves_once()
{
    let mut f = Root::new();
    let (a, w) = f.cloud_pair();
    let a2 = f.cloud_profile("A2");
    let generation = f.store.policy_generation();
    faulted(
        &mut f,
        &[
            "BEFORE UPDATE ON root_catalog_units",
            "BEFORE INSERT ON workspaces",
            "BEFORE INSERT ON local_transport_modes",
        ],
        |f| f.store.move_workspace(w, a2).map(|_| ()),
    );
    assert_eq!(f.store.workspace(w).unwrap().owner, a);
    assert_eq!(f.store.policy_generation(), generation);
    let revision_before = f.unit(w).unwrap().revision;
    f.store.move_workspace(w, a2).unwrap();
    assert_eq!(f.unit(w).unwrap().revision, revision_before + 1);
    assert_eq!(f.durable_placement(w).unwrap().profile, Some(a2));
    assert_eq!(
        f.store
            .transport_mode(TransportSubject::Workspace(w))
            .unwrap()
            .owner,
        a2
    );
    f.restart();
    assert_eq!(
        f.store.root_placement(w).unwrap().unwrap().profile,
        Some(a2)
    );
}

// ------------------------------------------ publish_root: trash, restore

/// Seam: `publish_root` for a trash and for a restore — unit, `workspaces`
/// row, routing. After the faults, one deletion fact and one restore fact,
/// each projected once.
#[test]
fn seam_publish_root_trash_and_restore_faults_leave_one_fact_each_after_the_retry() {
    let mut f = Root::new();
    let a = f.cloud_profile("A");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    let (document, _) = f
        .store
        .create_document(w, "One", "page", None, None, at())
        .unwrap();
    faulted(
        &mut f,
        &[
            "BEFORE UPDATE ON root_catalog_units",
            "BEFORE INSERT ON workspaces",
            "BEFORE DELETE ON document_locations",
        ],
        |f| f.store.delete_workspace(w, at()).map(|_| ()),
    );
    assert!(f.store.workspace(w).is_ok(), "still live");
    assert_eq!(f.store.locate(document).unwrap(), Some(w));
    assert_eq!(
        f.store.workspace_manifest_deletion(w).unwrap(),
        Some(None),
        "no hint before a commit: the manifest records no deletion"
    );
    f.store.delete_workspace(w, at()).unwrap();
    assert_eq!(workspace_unit(&f, w).deletions().len(), 1);
    assert_eq!(f.store.locate(document).unwrap(), None);
    // Opening a trashed Workspace — which every lifecycle write does before
    // it plans anything — repairs the **disposable** location index
    // (`reconcile_document_locations`). That repair is not part of the root
    // transaction and is not what the seam below is about, so it is settled
    // here rather than counted as drift.
    assert_eq!(
        f.store.workspace_manifest_deletion(w).unwrap(),
        Some(Some(at())),
        "the hint followed the commit"
    );
    assert_eq!(
        f.store.locate(document).unwrap(),
        Some(w),
        "the open repaired the route it had removed"
    );

    faulted(
        &mut f,
        &[
            "BEFORE UPDATE ON root_catalog_units",
            "BEFORE INSERT ON workspaces",
            "BEFORE INSERT ON document_locations",
        ],
        |f| f.store.restore_workspace(w).map(|_| ()),
    );
    assert!(f.store.workspace(w).is_err(), "still in the Trash");
    assert_eq!(workspace_unit(&f, w).restores().len(), 0);
    f.store.restore_workspace(w).unwrap();
    let unit = workspace_unit(&f, w);
    assert_eq!(unit.deletions().len(), 1);
    assert_eq!(unit.restores().len(), 1);
    assert!(unit.is_live());
    assert_eq!(f.store.locate(document).unwrap(), Some(w));
    f.restart();
    assert!(f.store.workspace(w).is_ok());
}

// ------------------------------------- publish_root: the deletion batch

/// Seam: `publish_root` for a Profile deletion batch and its restore —
/// the Profile unit, every target unit, every target row, the Profile's
/// visibility and the job row. One batch id, one job, one fact per target.
#[test]
fn seam_publish_root_batch_delete_and_batch_restore_faults_roll_back_every_unit_row_and_job() {
    let mut f = Root::new();
    let p = f.cloud_profile("P");
    let w1 = f.store.create_workspace(p, "W1", at()).unwrap().id;
    let w2 = f.store.create_workspace(p, "W2", at()).unwrap().id;
    let preview = f.store.profile_deletion_preview(p).unwrap();
    faulted(
        &mut f,
        &[
            "BEFORE UPDATE ON root_catalog_units WHEN NEW.unit_kind = 'profile_catalog'",
            "BEFORE UPDATE ON root_catalog_units WHEN NEW.unit_kind = 'workspace_catalog'",
            "BEFORE INSERT ON workspaces",
            "BEFORE UPDATE ON profiles",
            "BEFORE INSERT ON store_state WHEN NEW.key LIKE 'root_catalog.batch.%'",
        ],
        |f| f.store.delete_profile(p, &preview, at()).map(|_| ()),
    );
    assert!(f.store.profile(p).is_ok());
    assert!(profile_unit(&f, p).unwrap().is_live());
    assert!(workspace_unit(&f, w1).is_live());
    let batch = f.store.delete_profile(p, &preview, at()).unwrap();
    assert_eq!(batch.targets.len(), 2);
    assert_eq!(
        f.count("SELECT COUNT(*) FROM store_state WHERE key LIKE 'root_catalog.batch.%'"),
        1,
        "one job"
    );
    for (workspace, _) in &batch.targets {
        assert_eq!(workspace_unit(&f, *workspace).deletions().len(), 1);
        // As in the trash seam: opening a trashed target repairs its
        // disposable location index outside any root transaction, so it is
        // settled before the restore seam compares the pre-state.
        assert_eq!(
            f.store.workspace_manifest_deletion(*workspace).unwrap(),
            Some(Some(at())),
            "the batch wrote each target's hint"
        );
    }

    faulted(
        &mut f,
        &[
            "BEFORE UPDATE ON root_catalog_units WHEN NEW.unit_kind = 'profile_catalog'",
            "BEFORE UPDATE ON root_catalog_units WHEN NEW.unit_kind = 'workspace_catalog'",
            "BEFORE INSERT ON workspaces",
            "BEFORE UPDATE ON profiles",
            "BEFORE INSERT ON document_locations",
            "BEFORE INSERT ON store_state WHEN NEW.key LIKE 'root_catalog.batch.%'",
        ],
        |f| {
            f.store
                .restore_profile(p, batch.profile_deletion)
                .map(|_| ())
        },
    );
    assert!(f.store.profile(p).is_err(), "still deleted");
    assert_eq!(f.store.profile_deletions(at()).unwrap().len(), 1);
    let outcome = f.store.restore_profile(p, batch.profile_deletion).unwrap();
    assert_eq!(outcome.restored, vec![w1, w2]);
    for workspace in [w1, w2] {
        let unit = workspace_unit(&f, workspace);
        assert_eq!(unit.restores().len(), 1, "one restore fact");
        assert!(unit.is_live());
    }
    assert_eq!(profile_unit(&f, p).unwrap().restores().len(), 1);
    assert!(f.store.profile_deletions(at()).unwrap().is_empty());
    f.restart();
    assert!(f.store.profile(p).is_ok());
}

// -------------------------------------------- after the commit: the hint

/// Seam: the post-commit effect. The manifest deletion hint is written to
/// the Workspace's own repository *after* the root commit; a fault there
/// is logged and changes nothing about the committed lifecycle, a restart
/// reads the unit, and lifting the fault lets the next lifecycle write
/// carry the hint.
#[test]
fn seam_post_commit_manifest_hint_failure_leaves_the_root_fact_authoritative() {
    let mut f = Root::new();
    let a = f.cloud_profile("A");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    let repo = f.backend.open_workspace(w).unwrap();
    repo.connection
        .execute_batch(
            "CREATE TRIGGER hint_fault BEFORE INSERT ON loro_updates
             BEGIN SELECT RAISE(ABORT, 'injected fault'); END;",
        )
        .unwrap();

    f.store.delete_workspace(w, at()).unwrap();
    assert!(
        workspace_unit(&f, w).is_deleted(),
        "the root fact committed"
    );
    assert_eq!(
        f.store.workspace_including_deleted(w).unwrap().deleted_at,
        Some(at()),
        "and projected"
    );
    assert_eq!(
        f.store.workspace_manifest_deletion(w).unwrap(),
        Some(None),
        "the hint was not written"
    );
    assert!(f.store.workspace(w).is_err());
    f.restart();
    assert!(
        f.store.workspace(w).is_err(),
        "a restart reads the unit, not the hint"
    );
    assert!(workspace_unit(&f, w).is_deleted());

    repo.connection
        .execute_batch("DROP TRIGGER hint_fault;")
        .unwrap();
    f.store.restore_workspace(w).unwrap();
    assert!(f.store.workspace(w).is_ok());
    assert_eq!(
        f.store.workspace_manifest_deletion(w).unwrap(),
        Some(None),
        "a live Workspace's hint is no deletion"
    );
    f.store.delete_workspace(w, at()).unwrap();
    assert_eq!(
        f.store.workspace_manifest_deletion(w).unwrap(),
        Some(Some(at())),
        "with the fault lifted the hint follows the commit"
    );
}

// ------------------------------------------------- overlay publication

/// Seam: the overlay publication — the fold of a private overlay into the
/// durable unit when a subject becomes exportable. A fault inside it is
/// logged by the mode-commit hook and returned by the job; the overlay
/// stays exactly as it was, the durable bytes too, and the next job run
/// publishes it once.
#[test]
fn seam_overlay_publication_fault_keeps_the_overlay_and_the_job_publishes_it_once_later() {
    let mut f = Root::new();
    let (_a, w) = f.cloud_pair();
    let a2 = f.cloud_profile("A2");
    f.store.move_workspace(w, a2).unwrap();
    let durable_before = f.unit(w).unwrap();
    let b = f.profile("B");
    f.store.move_workspace(w, b).unwrap();
    let overlay_before = f
        .overlay(w)
        .expect("a move into a Local-only Profile waits");

    for fault in [
        "BEFORE UPDATE ON root_catalog_units",
        "BEFORE DELETE ON root_catalog_overlays",
    ] {
        f.fail_on(fault);
        // The hook's failure is logged, never returned: the mode rows commit.
        arm_group(&mut f, b, TransportMode::P2p);
        assert_eq!(f.overlay(w), Some(overlay_before.clone()), "{fault}");
        assert_eq!(f.unit(w), Some(durable_before.clone()), "{fault}");
        // The job reports the same hold and changes nothing either.
        let outcome = f.store.publish_pending_overlays().unwrap();
        assert_eq!((outcome.published, outcome.held), (0, 1), "{fault}");
        assert_eq!(f.overlay(w), Some(overlay_before.clone()), "{fault}");
        assert_eq!(
            f.store
                .eligible_export_source(
                    CatalogUnitRef::Workspace { workspace: w },
                    TransportPath::Peer
                )
                .unwrap(),
            None,
            "{fault}: nothing leaves while the overlay is pending"
        );
        f.recover();
        // Back to Local-only for the next fault, so the hook fires again.
        arm_group(&mut f, b, TransportMode::LocalOnly);
        assert_eq!(f.overlay(w), Some(overlay_before.clone()));
    }

    // Lifted: the hook (or the job) publishes once; a second run has
    // nothing to do.
    arm_group(&mut f, b, TransportMode::P2p);
    if f.overlay(w).is_some() {
        let outcome = f.store.publish_pending_overlays().unwrap();
        assert_eq!(outcome.published, 1);
    }
    assert!(f.overlay(w).is_none());
    let published = f.unit(w).unwrap();
    assert_eq!(published.revision, durable_before.revision + 1);
    assert_eq!(f.durable_placement(w).unwrap().profile, Some(b));
    let outcome = f.store.publish_pending_overlays().unwrap();
    assert_eq!((outcome.published, outcome.held), (0, 0));
    assert_eq!(f.unit(w).unwrap(), published, "published once");
}

// ------------------------------------------------------ the join commit

/// Seam: `commit_staged_root_unit` — the unit's compare-and-set, the
/// projection and the receipt in `store_state`. A fault at any of them
/// leaves the unit, the row and the receipt as they were; staging again
/// and committing applies once, and the receipt never regresses.
#[test]
fn seam_join_commit_faults_leave_unit_row_and_receipt_and_the_restaged_commit_applies_once() {
    let mut f = Root::new();
    let (a, w) = f.cloud_pair();
    let rel = UserScopeId::new().as_uuid();
    f.store.bind_peer_scope(rel).unwrap();
    let unit = CatalogUnitRef::Workspace { workspace: w };
    let receipt_unit = JoinUnit::WorkspaceCatalog { workspace: w };
    // Another device's trash of W, as the frames the peer sends.
    let row = f.unit(w).unwrap();
    let remote = WorkspaceCatalogUnit::from_snapshot(&row.snapshot, w, 4_242).unwrap();
    let before = remote.version();
    remote
        .record_deletion(DeletionId::new(), &DeletionRecord::new(10, None))
        .unwrap();
    let update = remote.updates_since(&before).unwrap();

    for fault in [
        "BEFORE UPDATE ON root_catalog_units",
        "BEFORE INSERT ON workspaces",
        "BEFORE INSERT ON store_state WHEN NEW.key LIKE 'root_catalog.join.%'",
    ] {
        let staged = f
            .store
            .stage_root_unit(rel, unit, std::slice::from_ref(&update))
            .unwrap();
        assert!(!staged.is_pending());
        let snapshot = dump(&f);
        f.fail_on(fault);
        let error = f
            .store
            .commit_staged_root_unit(rel, staged, None)
            .unwrap_err();
        assert_eq!(error.code(), Code::StorageFailed, "{fault}");
        f.recover();
        assert_eq!(dump(&f), snapshot, "{fault}");
        assert_eq!(
            f.unit(w),
            Some(row.clone()),
            "{fault}: the unit is untouched"
        );
        assert!(
            f.store.join_receipt(rel, receipt_unit).unwrap().is_none(),
            "{fault}: no receipt"
        );
        assert!(f.store.workspace(w).is_ok(), "{fault}: still live here");
    }

    let staged = f
        .store
        .stage_root_unit(rel, unit, std::slice::from_ref(&update))
        .unwrap();
    let outcome = f.store.commit_staged_root_unit(rel, staged, None).unwrap();
    assert!(matches!(
        outcome,
        RootJoinOutcome::Applied {
            lifecycle_changed: true,
            ..
        }
    ));
    assert_eq!(f.unit(w).unwrap().revision, row.revision + 1);
    assert_eq!(workspace_unit(&f, w).deletions().len(), 1);
    assert!(f.store.workspace(w).is_err(), "projected");
    assert_eq!(
        f.store.workspace(w).unwrap_err().code(),
        Code::WorkspaceNotFound
    );
    let receipt = f.store.join_receipt(rel, receipt_unit).unwrap().unwrap();
    assert_eq!(receipt.phase, JoinPhase::Reconciled);

    // The same frames again: nothing is rewritten, the receipt stands.
    let staged = f
        .store
        .stage_root_unit(rel, unit, std::slice::from_ref(&update))
        .unwrap();
    let again = f.store.commit_staged_root_unit(rel, staged, None).unwrap();
    assert!(matches!(
        again,
        RootJoinOutcome::Applied {
            lifecycle_changed: false,
            placement_changed: false,
            ..
        }
    ));
    assert_eq!(f.unit(w).unwrap().revision, row.revision + 1);
    assert_eq!(
        f.store
            .join_receipt(rel, receipt_unit)
            .unwrap()
            .unwrap()
            .frontier,
        receipt.frontier
    );
    // And an older frame (the unit as it was before the trash) regresses
    // neither the unit nor the receipt.
    let staged = f
        .store
        .stage_root_unit(rel, unit, std::slice::from_ref(&row.snapshot))
        .unwrap();
    f.store.commit_staged_root_unit(rel, staged, None).unwrap();
    assert_eq!(f.unit(w).unwrap().revision, row.revision + 1);
    assert_eq!(
        f.store
            .join_receipt(rel, receipt_unit)
            .unwrap()
            .unwrap()
            .frontier,
        receipt.frontier
    );
    assert_eq!(f.store.workspace_including_deleted(w).unwrap().owner, a);
}

// ----------------------------------------------------------- purge steps

/// Seam: purge step 1, the evidence commit. A fault there leaves no
/// evidence, the row, the bytes and the personal row; the pass reports the
/// Workspace pending and the retry purges it with one purge fact.
#[test]
fn seam_purge_step_1_evidence_fault_leaves_everything_and_the_retry_purges_once() {
    let mut f = Root::new();
    // A Cloud Profile: the facts are durable, so the evidence write is the
    // unit's compare-and-set (a Local-only Profile's would be its overlay).
    let a = f.cloud_profile("A");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    let (_, rule) = reminder_in(w);
    f.store.create_reminder(a, &rule).unwrap();
    f.store.delete_workspace(w, at()).unwrap();
    let before = dump(&f);
    f.fail_on("BEFORE UPDATE ON root_catalog_units");
    let outcome = f.store.purge_deleted(after_retention(at())).unwrap();
    assert_eq!(outcome.pending, vec![(w, Code::StorageFailed)]);
    assert!(outcome.purged.is_empty());
    f.recover();
    assert_eq!(
        dump(&f),
        before,
        "no evidence, no retirement, no row change"
    );
    assert!(workspace_unit(&f, w).purged().is_empty());
    assert!(f.backend.0.deleted.borrow().is_empty(), "no bytes went");

    let retry = f.store.purge_deleted(after_retention(at())).unwrap();
    assert_eq!(retry.purged, vec![w]);
    assert!(retry.pending.is_empty());
    assert_eq!(workspace_unit(&f, w).purged().len(), 1, "one fact");
    assert!(f.store.workspace_including_deleted(w).is_err());
    assert!(
        f.catalog()
            .workspace_personal_unit(f.scope(), w)
            .unwrap()
            .is_none()
    );
    assert_eq!(f.count("SELECT COUNT(*) FROM workspace_reminder_due"), 0);
}

/// Seam: purge step 3, the personal retirement. The evidence is committed
/// and survives; the personal row, the due rows, the bytes and the
/// `workspaces` row are untouched; the retry resumes from step 2 and writes
/// no second fact.
#[test]
fn seam_purge_step_3_retirement_fault_keeps_the_personal_row_and_the_retry_resumes_without_a_second_fact()
 {
    let mut f = Root::new();
    let a = f.profile("A");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    let (_, rule) = reminder_in(w);
    f.store.create_reminder(a, &rule).unwrap();
    index(&mut f.store, a);
    f.store.delete_workspace(w, at()).unwrap();
    f.fail_on("BEFORE DELETE ON workspace_personal_crdts");
    let outcome = f.store.purge_deleted(after_retention(at())).unwrap();
    assert_eq!(outcome.pending, vec![(w, Code::StorageFailed)]);
    f.recover();
    assert_eq!(
        workspace_unit(&f, w).purged().len(),
        1,
        "the intent was committed before the failing step"
    );
    assert!(
        f.catalog()
            .workspace_personal_unit(f.scope(), w)
            .unwrap()
            .is_some(),
        "the personal row is intact"
    );
    assert_eq!(f.count("SELECT COUNT(*) FROM workspace_reminder_due"), 1);
    assert!(f.store.workspace_including_deleted(w).is_ok());
    assert!(f.backend.0.deleted.borrow().is_empty());

    let retry = f.store.purge_deleted(after_retention(at())).unwrap();
    assert_eq!(retry.purged, vec![w]);
    assert_eq!(workspace_unit(&f, w).purged().len(), 1, "no second fact");
    assert!(
        f.catalog()
            .workspace_personal_unit(f.scope(), w)
            .unwrap()
            .is_none()
    );
    assert_eq!(f.count("SELECT COUNT(*) FROM workspace_reminder_due"), 0);
    assert_eq!(f.backend.0.deleted.borrow().as_slice(), &[w]);
    assert!(f.store.workspace_including_deleted(w).is_err());
}

/// Seam: purge step 4, the backend bytes. The evidence and the retirement
/// stand, the row stays, the Workspace is pending; the retry removes the
/// bytes and the row with no second fact and no second retirement.
#[test]
fn seam_purge_step_4_bytes_fault_leaves_the_row_pending_and_the_retry_finishes() {
    let mut f = Root::new();
    let a = f.profile("A");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    let (_, rule) = reminder_in(w);
    f.store.create_reminder(a, &rule).unwrap();
    f.store.delete_workspace(w, at()).unwrap();
    f.backend.0.cleanup_fails.set(true);
    let outcome = f.store.purge_deleted(after_retention(at())).unwrap();
    assert_eq!(outcome.pending, vec![(w, Code::StorageFailed)]);
    assert_eq!(workspace_unit(&f, w).purged().len(), 1);
    assert!(
        f.catalog()
            .workspace_personal_unit(f.scope(), w)
            .unwrap()
            .is_none(),
        "step 3 completed before the bytes failed"
    );
    assert!(
        f.store.workspace_including_deleted(w).is_ok(),
        "step 5 did not run"
    );
    // Pending again on the next pass, not escalated.
    let again = f.store.purge_deleted(after_retention(at())).unwrap();
    assert_eq!(again.pending, vec![(w, Code::StorageFailed)]);
    assert_eq!(workspace_unit(&f, w).purged().len(), 1);

    f.backend.0.cleanup_fails.set(false);
    let retry = f.store.purge_deleted(after_retention(at())).unwrap();
    assert_eq!(retry.purged, vec![w]);
    assert!(f.store.workspace_including_deleted(w).is_err());
    assert_eq!(workspace_unit(&f, w).purged().len(), 1);
    assert!(f.backend.0.deleted.borrow().contains(&w));
    f.restart();
    assert_eq!(
        f.store.purge_deleted(after_retention(at())).unwrap(),
        koru_store_core::service::PurgeOutcome::default(),
        "nothing left to do"
    );
}

// ------------------------------------------------- account scope binding

/// Seam: the account-scope binding's one `store_state` row. A fault leaves
/// the device unbound; the retry binds once, and the binding survives a
/// restart.
#[test]
fn seam_account_scope_binding_fault_leaves_the_device_unbound_and_the_retry_binds_once() {
    let mut f = Root::new();
    let (account, scope_id) = (UserScopeId::new().as_uuid(), UserScopeId::new().as_uuid());
    f.fail_on("BEFORE INSERT ON store_state WHEN NEW.key = 'root_catalog.account_scope'");
    assert_eq!(
        f.store
            .bind_account_scope(account, scope_id, 5)
            .unwrap_err()
            .code(),
        Code::StorageFailed
    );
    f.recover();
    assert_eq!(f.store.account_binding().unwrap(), None);
    assert_eq!(f.store.account_scope(account).unwrap(), None);
    let bound = f.store.bind_account_scope(account, scope_id, 5).unwrap();
    assert_eq!(bound, f.scope());
    assert_eq!(
        f.store.bind_account_scope(account, scope_id, 9).unwrap(),
        bound
    );
    assert_eq!(
        f.count("SELECT COUNT(*) FROM store_state WHERE key = 'root_catalog.account_scope'"),
        1
    );
    f.restart();
    assert_eq!(f.store.account_scope(account).unwrap(), Some(bound));
}

// ---------------------------------------------------------- bundle import

/// What a Restore that crashed mid-staging leaves behind: the bundle's
/// repository claimed under its origin's id, holding `complete` of the
/// bundle's documents with their one update and `bare` of them as rows
/// without it (a crash between `create_document` and `commit_updates`), and
/// nothing in the catalog. Written through the inner backend, so the
/// `FaultBackend`'s `created` ledger stays empty and a test can see whether
/// the import claimed anything itself.
fn stage_by_hand(f: &Root, bundle: &Bundle, complete: &[DocumentId], bare: &[DocumentId]) {
    let mut repo = f
        .backend
        .0
        .backend
        .create_workspace(bundle.manifest.workspace)
        .unwrap();
    for document in &bundle.manifest.documents {
        if !complete.contains(&document.id) && !bare.contains(&document.id) {
            continue;
        }
        repo.create_document(document.id, document.kind, at())
            .unwrap();
        if complete.contains(&document.id) {
            repo.commit_updates(&UpdateBatch {
                document: document.id,
                updates: vec![bundle.snapshot_of(document.id).unwrap().to_vec()],
                version_vector: Vec::new(),
                frontiers: Vec::new(),
                received_at: at(),
                origins: vec![UpdateOrigin::Store],
            })
            .unwrap();
        }
    }
}

/// Every document row of the repository under `workspace` with its journal
/// head, read through the Store's own backend: what was actually written.
/// (An in-memory backend opens a missing repository as an empty one, so an
/// empty map is also what "no repository" looks like.)
fn staged_documents(f: &Root, workspace: WorkspaceId) -> BTreeMap<DocumentId, Option<u64>> {
    let repo = f.backend.open_workspace(workspace).unwrap();
    repo.documents()
        .unwrap()
        .into_iter()
        .map(|state| {
            let head = repo.journal_since(state.id, None).unwrap().head;
            (state.id, head)
        })
        .collect()
}

/// A Workspace with the given pages and one history marker on the first,
/// exported: the bundle every staging test below restores.
fn exported_bundle(f: &mut Root, titles: &[&str]) -> (WorkspaceId, Vec<DocumentId>, Bundle) {
    let a = f.profile("A");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    let mut pages = Vec::new();
    for title in titles {
        pages.push(
            f.store
                .create_document(w, title, "page", None, None, at())
                .unwrap()
                .0,
        );
    }
    f.store
        .capture_history(
            w,
            pages[0],
            None,
            HistoryReason::Manual,
            at(),
            Some("Milestone"),
            None,
            None,
        )
        .unwrap();
    let bundle = f.store.export_workspace(w, at()).unwrap();
    assert_eq!(bundle.manifest.history_entries.len(), 1);
    (w, pages, bundle)
}

/// Seam: the bundle publication. The existing Plan 1.2 test faults the
/// publication's first and last statements and a staging write; this one
/// faults the `workspaces` and `document_locations` statements and then
/// the case EXECUTION.md carried forward and QUESTIONS.md Q6 settled — a
/// crash between staging and publication leaves the staged repository
/// behind, and the Restore retry meets it. The retry adopts it as its own
/// interrupted staging (same id, the bundle's manifest, no row, no routing),
/// writes nothing twice, publishes once and says so; a further retry is the
/// ordinary `WorkspaceExists`, because the row now exists.
#[test]
fn seam_bundle_publication_faults_and_the_retry_resumes_the_staged_repository() {
    let mut f = Root::new();
    let (w, pages, bundle) = exported_bundle(&mut f, &["One"]);

    for fault in [
        "BEFORE INSERT ON workspaces",
        "BEFORE INSERT ON document_locations",
    ] {
        let mut there = Root::new();
        let b = there.profile("B");
        there.fail_on(fault);
        let error = there
            .store
            .import_workspace(&bundle, ImportMode::Restore, b, at())
            .unwrap_err();
        assert_eq!(error.code(), Code::StorageFailed, "{fault}");
        there.recover();
        assert!(there.unit(w).is_none(), "{fault}");
        assert!(
            there.store.workspace_including_deleted(w).is_err(),
            "{fault}"
        );
        assert_eq!(
            there.count("SELECT COUNT(*) FROM workspace_lineage"),
            0,
            "{fault}"
        );
        assert_eq!(
            there.backend.0.deleted.borrow().as_slice(),
            &[w],
            "{fault}: cleaned up"
        );
        // The retry after a clean failure claims afresh and lists exactly one
        // Workspace.
        assert_eq!(
            there
                .store
                .import_workspace(&bundle, ImportMode::Restore, b, at())
                .unwrap(),
            ImportedWorkspace {
                workspace: w,
                publication: ImportPublication::Fresh,
            },
            "{fault}"
        );
        assert_eq!(there.store.workspaces_including_deleted().unwrap().len(), 1);
    }

    // The crash case: the publication fails and the repository cannot be
    // cleaned (as when the process dies between the two). Nothing lists the
    // staged repository, and every content row of it was written.
    let mut there = Root::new();
    let b = there.profile("B");
    there.backend.0.cleanup_fails.set(true);
    there.fail_on("BEFORE INSERT ON workspaces");
    assert_eq!(
        there
            .store
            .import_workspace(&bundle, ImportMode::Restore, b, at())
            .unwrap_err()
            .code(),
        Code::StorageFailed
    );
    there.recover();
    there.backend.0.cleanup_fails.set(false);
    assert!(there.store.workspace_including_deleted(w).is_err());
    assert!(there.unit(w).is_none());
    let left_behind = staged_documents(&there, w);
    assert_eq!(left_behind.len(), bundle.manifest.documents.len());
    assert!(left_behind.values().all(|head| *head == Some(1)));

    // The retry adopts the leftover and publishes once.
    assert_eq!(
        there
            .store
            .import_workspace(&bundle, ImportMode::Restore, b, at())
            .unwrap(),
        ImportedWorkspace {
            workspace: w,
            publication: ImportPublication::Resumed,
        }
    );
    assert_eq!(
        there.backend.0.created.borrow().as_slice(),
        &[w],
        "claimed once, by the crashed attempt"
    );
    assert_eq!(
        there.backend.0.deleted.borrow().as_slice(),
        &[w],
        "the failed cleanup, and nothing deleted since"
    );
    assert_eq!(there.store.workspaces_including_deleted().unwrap().len(), 1);
    assert_eq!(there.store.workspace(w).unwrap().owner, b);
    assert_eq!(there.unit(w).unwrap().revision, 1);
    let receipt = there.store.lineage(w).unwrap();
    assert_eq!(receipt.len(), 1);
    assert_eq!(receipt[0].kind, LineageKind::Restore);
    for document in &bundle.manifest.documents {
        assert_eq!(there.store.locate(document.id).unwrap(), Some(w));
    }
    // Nothing was written twice: one update per document, one marker.
    assert_eq!(staged_documents(&there, w), left_behind);
    assert_eq!(
        there
            .store
            .history_entries(w, pages[0], None, 50)
            .unwrap()
            .len(),
        1
    );
    let titles: Vec<String> = there
        .store
        .documents(w)
        .unwrap()
        .into_iter()
        .map(|entry| entry.title)
        .collect();
    assert_eq!(titles, ["One"]);

    // Published once: the row now exists, so a further retry is the ordinary
    // refusal and changes nothing.
    let again = there
        .store
        .import_workspace(&bundle, ImportMode::Restore, b, at())
        .unwrap_err();
    assert_eq!(again.code(), Code::WorkspaceExists);
    assert!(
        again.message().contains("already in the store"),
        "{}",
        again.message()
    );
    assert_eq!(there.store.workspaces_including_deleted().unwrap().len(), 1);
    assert_eq!(there.store.lineage(w).unwrap().len(), 1);
}

/// A repository under the origin's id that is not the bundle's own staging
/// — its manifest document is another, or it holds a document the bundle
/// does not name — is somebody's, and the Restore keeps today's refusal
/// without writing into it, claiming or deleting anything.
#[test]
fn a_leftover_with_another_manifest_identity_or_a_stranger_document_is_refused_and_untouched() {
    let mut f = Root::new();
    let (w, _, bundle) = exported_bundle(&mut f, &["One"]);
    let every: Vec<DocumentId> = bundle.manifest.documents.iter().map(|d| d.id).collect();

    for (case, staged, stranger) in [
        ("another manifest identity", &[][..], DocumentKind::Manifest),
        ("a stranger document", &every[..], DocumentKind::Document),
    ] {
        let mut there = Root::new();
        let b = there.profile("B");
        stage_by_hand(&there, &bundle, staged, &[]);
        there
            .backend
            .0
            .backend
            .open_workspace(w)
            .unwrap()
            .create_document(DocumentId::new(), stranger, at())
            .unwrap();
        let before = staged_documents(&there, w);

        let refused = there
            .store
            .import_workspace(&bundle, ImportMode::Restore, b, at())
            .unwrap_err();
        assert_eq!(refused.code(), Code::WorkspaceExists, "{case}");
        assert!(
            there.store.workspace_including_deleted(w).is_err(),
            "{case}"
        );
        assert!(there.unit(w).is_none(), "{case}");
        assert_eq!(
            there.count("SELECT COUNT(*) FROM document_locations"),
            0,
            "{case}"
        );
        assert_eq!(
            there.count("SELECT COUNT(*) FROM workspace_lineage"),
            0,
            "{case}"
        );
        assert!(
            there.backend.0.created.borrow().is_empty(),
            "{case}: nothing claimed"
        );
        assert!(
            there.backend.0.deleted.borrow().is_empty(),
            "{case}: nothing deleted"
        );
        assert_eq!(
            staged_documents(&there, w),
            before,
            "{case}: not written into"
        );
    }
}

/// A repository whose id has a `workspaces` row — here a tombstoned one, the
/// Workspace in the Trash with its routing already dropped — is a Workspace,
/// not staging: the Restore is refused, the row stands, and the repository
/// is not touched.
#[test]
fn a_leftover_whose_id_has_a_row_even_tombstoned_is_refused() {
    let mut f = Root::new();
    let (w, _, bundle) = exported_bundle(&mut f, &["One"]);

    let mut there = Root::new();
    let b = there.profile("B");
    assert_eq!(
        there
            .store
            .import_workspace(&bundle, ImportMode::Restore, b, at())
            .unwrap()
            .publication,
        ImportPublication::Fresh
    );
    there.store.delete_workspace(w, at()).unwrap();
    assert!(
        there
            .store
            .workspace_including_deleted(w)
            .unwrap()
            .deleted_at
            .is_some(),
        "tombstoned: the row stays"
    );
    assert_eq!(there.count("SELECT COUNT(*) FROM document_locations"), 0);
    let before = staged_documents(&there, w);

    let refused = there
        .store
        .import_workspace(&bundle, ImportMode::Restore, b, at())
        .unwrap_err();
    assert_eq!(refused.code(), Code::WorkspaceExists);
    assert_eq!(there.store.workspaces_including_deleted().unwrap().len(), 1);
    assert!(
        there
            .store
            .workspace_including_deleted(w)
            .unwrap()
            .deleted_at
            .is_some()
    );
    assert_eq!(
        there.store.lineage(w).unwrap().len(),
        1,
        "no second receipt"
    );
    assert_eq!(there.backend.0.created.borrow().as_slice(), &[w]);
    assert!(there.backend.0.deleted.borrow().is_empty());
    assert_eq!(staged_documents(&there, w), before);
}

/// A repository with document routing in the catalog but no `workspaces`
/// row is not staging either — staging publishes the row and the routing
/// together or not at all — so the Restore is refused and nothing is
/// written.
#[test]
fn a_leftover_with_document_routing_is_refused() {
    let mut f = Root::new();
    let (w, pages, bundle) = exported_bundle(&mut f, &["One"]);
    let every: Vec<DocumentId> = bundle.manifest.documents.iter().map(|d| d.id).collect();

    let mut there = Root::new();
    let b = there.profile("B");
    stage_by_hand(&there, &bundle, &every, &[]);
    there
        .control
        .execute(
            "INSERT INTO document_locations (document_id, workspace_id) VALUES (?1, ?2)",
            params![pages[0].to_storage(), w.to_storage()],
        )
        .unwrap();
    let before = staged_documents(&there, w);

    let refused = there
        .store
        .import_workspace(&bundle, ImportMode::Restore, b, at())
        .unwrap_err();
    assert_eq!(refused.code(), Code::WorkspaceExists);
    assert!(there.store.workspace_including_deleted(w).is_err());
    assert!(there.unit(w).is_none());
    assert_eq!(
        there.count("SELECT COUNT(*) FROM document_locations"),
        1,
        "the routing is as it was"
    );
    assert_eq!(there.count("SELECT COUNT(*) FROM workspace_lineage"), 0);
    assert!(there.backend.0.created.borrow().is_empty());
    assert!(there.backend.0.deleted.borrow().is_empty());
    assert_eq!(staged_documents(&there, w), before);
}

/// A crash in the middle of staging: the manifest and one page were written
/// with their update, a second page's row exists without it, the third was
/// never reached and neither was the marker. The retry writes exactly what
/// is missing, and the published Workspace holds the bundle's document set
/// — one update each — and its one marker.
#[test]
fn a_partially_staged_repository_is_completed_to_the_bundles_document_set() {
    let mut f = Root::new();
    let (w, pages, bundle) = exported_bundle(&mut f, &["One", "Two", "Three"]);
    let manifest = bundle.manifest_document().unwrap();
    assert_eq!(bundle.manifest.documents.len(), 4);

    let mut there = Root::new();
    let b = there.profile("B");
    stage_by_hand(&there, &bundle, &[manifest, pages[0]], &[pages[1]]);
    let before = staged_documents(&there, w);
    assert_eq!(before.len(), 3);
    assert_eq!(before[&pages[1]], None, "a row without its update");

    assert_eq!(
        there
            .store
            .import_workspace(&bundle, ImportMode::Restore, b, at())
            .unwrap(),
        ImportedWorkspace {
            workspace: w,
            publication: ImportPublication::Resumed,
        }
    );
    let expected: BTreeMap<DocumentId, Option<u64>> = bundle
        .manifest
        .documents
        .iter()
        .map(|document| (document.id, Some(1)))
        .collect();
    assert_eq!(
        staged_documents(&there, w),
        expected,
        "every document of the bundle once, with exactly one update"
    );
    assert!(
        there.backend.0.created.borrow().is_empty(),
        "nothing claimed afresh"
    );
    assert!(there.backend.0.deleted.borrow().is_empty());
    let mut titles: Vec<String> = there
        .store
        .documents(w)
        .unwrap()
        .into_iter()
        .map(|entry| entry.title)
        .collect();
    titles.sort();
    assert_eq!(titles, ["One", "Three", "Two"]);
    assert_eq!(
        there
            .store
            .history_entries(w, pages[0], None, 50)
            .unwrap()
            .len(),
        1
    );
    for document in &bundle.manifest.documents {
        assert_eq!(there.store.locate(document.id).unwrap(), Some(w));
    }
    assert_eq!(there.unit(w).unwrap().revision, 1);
    assert_eq!(there.store.lineage(w).unwrap().len(), 1);
    assert_eq!(there.store.workspaces_including_deleted().unwrap().len(), 1);
}

/// Bundles across root evidence: a Restore is refused while the origin's
/// deletion is active (in the Trash) as well as after the purge, a
/// Duplicate is allowed in both states and mints a new unit, and neither
/// resurrects or re-lists the origin.
#[test]
fn bundle_restore_is_refused_across_an_active_deletion_and_a_purge_while_duplicate_is_allowed() {
    let mut f = Root::new();
    let (_a, w) = f.cloud_pair();
    let bundle = f.store.export_workspace(w, at()).unwrap();
    f.store.delete_workspace(w, at()).unwrap();
    let a2 = f.cloud_profile("A2");
    let refused = f
        .store
        .import_workspace(&bundle, ImportMode::Restore, a2, at())
        .unwrap_err();
    assert_eq!(refused.code(), Code::WorkspaceExists);
    assert!(f.store.workspace(w).is_err(), "still in the Trash");
    let copy = f
        .store
        .import_workspace(&bundle, ImportMode::Duplicate, a2, at())
        .unwrap()
        .workspace;
    assert_ne!(copy, w);
    assert!(workspace_unit(&f, copy).is_live());
    assert!(
        workspace_unit(&f, w).is_deleted(),
        "the origin is untouched"
    );
    assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 2);

    f.store.purge_deleted(after_retention(at())).unwrap();
    assert!(f.store.workspace_including_deleted(w).is_err());
    assert_eq!(
        f.store
            .import_workspace(&bundle, ImportMode::Restore, a2, at())
            .unwrap_err()
            .code(),
        Code::WorkspaceExists
    );
    assert!(
        f.store.workspace_including_deleted(w).is_err(),
        "nothing resurrected"
    );
    assert!(!workspace_unit(&f, w).purged().is_empty());
    let second = f
        .store
        .import_workspace(&bundle, ImportMode::Duplicate, a2, at())
        .unwrap()
        .workspace;
    assert_ne!(second, copy);
    assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 2);
}

// ============================================================ A2: matrix

// ------------------------------------------- independently upgraded store

/// A Store opened over a catalog that was 12 with legacy personal rows: the
/// migrated reminders are answered under the Workspace's placement with
/// their ids unchanged, once each, and the due index holds one row per
/// reminder. `catalog/tests/root_catalog_faults.rs` proves two such stores
/// carry identical operation ids.
#[test]
fn a_store_over_an_upgraded_twelve_answers_migrated_reminders_once_with_unchanged_ids() {
    const STORE_PEER: u64 = 4_242;
    let name = koru_sqlite::MemoryName::fresh("upgraded-twelve");
    let keeper = koru_sqlite::open_memory(&name).unwrap();
    let mut connection = koru_sqlite::open_memory(&name).unwrap();
    crate::db::migrate(
        &mut connection,
        "catalog.db",
        &crate::catalog::MIGRATIONS[..12],
        12,
    )
    .unwrap();
    let owner = koru_store_core::Profile::new("Owner", Locale::default(), at()).unwrap();
    connection
        .execute(
            "INSERT INTO profiles
               (id, name, purpose, avatar, account, locale, created_at, icon, color, kind)
             VALUES (?1, ?2, NULL, NULL, NULL, ?3, ?4, NULL, NULL, ?5)",
            params![
                owner.id.to_storage(),
                owner.name,
                owner.locale.as_str(),
                owner.created_at.millis(),
                owner.kind.as_str(),
            ],
        )
        .unwrap();
    let workspace = koru_store_core::Workspace::new(owner.id, "W", at()).unwrap();
    connection
        .execute(
            "INSERT INTO workspaces (id, owner, name, created_at, position)
             VALUES (?1, ?2, ?3, ?4, 0)",
            params![
                workspace.id.to_storage(),
                owner.id.to_storage(),
                workspace.name,
                workspace.created_at.millis(),
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO store_state (key, value) VALUES (?1, ?2)",
            params![koru_store_core::service::K_PEER_ID, STORE_PEER.to_string()],
        )
        .unwrap();
    let (_, rule) = reminder_in(workspace.id);
    let legacy = PersonalManifest::create(owner.id, workspace.id).unwrap();
    legacy.set_peer_id(STORE_PEER).unwrap();
    legacy.put_reminder_rule(&rule).unwrap();
    connection
        .execute(
            "INSERT INTO profile_personal_crdts (owner_profile_id, workspace_id, snapshot)
             VALUES (?1, ?2, ?3)",
            params![
                owner.id.to_storage(),
                workspace.id.to_storage(),
                legacy.export_snapshot().unwrap()
            ],
        )
        .unwrap();
    drop(connection);

    let backend = FaultBackend::new(None, false);
    let mut store = Store::open(SqliteCatalog::in_named_memory(&name).unwrap(), backend).unwrap();
    assert_eq!(store.peer_id(), STORE_PEER, "the Store peer is what it was");
    let reminders = store.reminders(owner.id).unwrap();
    assert_eq!(reminders.len(), 1);
    assert_eq!(reminders[0].rule.id, rule.id, "the id never changes");
    index(&mut store, owner.id);
    assert_eq!(store.reminder_index_rows(owner.id).unwrap().len(), 1);
    assert_eq!(
        keeper
            .query_row("SELECT COUNT(*) FROM workspace_reminder_due", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        store.root_placement(workspace.id).unwrap().unwrap().profile,
        Some(owner.id),
        "placed where the projection said"
    );
    drop(keeper);
}

// ------------------------------------------------- privacy cycles (§9)

/// LocalOnly → P2P → LocalOnly → P2P with a trash, a restore and a move
/// through a hidden Profile pending in between: the overlay carries the
/// private edits, the publication folds them once (one deletion, one
/// restore, the placement that was last written), nothing is lost, and the
/// bytes that leave for a peer name neither hidden Profile's UUID nor any
/// sentinel name, in keys or values of the exported history.
#[test]
fn a_privacy_cycle_with_pending_lifecycle_facts_folds_once_and_discloses_no_hidden_subject() {
    const HIDDEN_ORIGIN: &str = "SENTINEL-HIDDEN-ORIGIN";
    const HIDDEN_STOP: &str = "SENTINEL-HIDDEN-STOP";
    let mut f = Root::new();
    // W is born under a Local-only Profile, so its history names it.
    let origin = f.profile(HIDDEN_ORIGIN);
    let w = f.store.create_workspace(origin, "W", at()).unwrap().id;
    let a = f.cloud_profile("A");
    f.store.move_workspace(w, a).unwrap();
    // W was seeded Local-only under its origin; arming A's group lifts it,
    // and the hook publishes the private move into the durable unit.
    arm_group(&mut f, a, TransportMode::Cloud);
    let stop = f.profile(HIDDEN_STOP);
    assert!(
        f.overlay(w).is_none(),
        "A admits export: the move is durable"
    );
    let durable_before = f.unit(w).unwrap();

    // Cycle 1: disable, trash, restore, enable.
    arm_group(&mut f, a, TransportMode::LocalOnly);
    f.store.delete_workspace(w, at()).unwrap();
    f.store.restore_workspace(w).unwrap();
    assert!(f.overlay(w).is_some(), "both facts wait in the overlay");
    assert_eq!(
        f.unit(w),
        Some(durable_before.clone()),
        "nothing left the device"
    );
    assert_eq!(
        f.store
            .eligible_export_source(
                CatalogUnitRef::Workspace { workspace: w },
                TransportPath::Peer
            )
            .unwrap(),
        None
    );
    arm_group(&mut f, a, TransportMode::Cloud);
    assert!(f.overlay(w).is_none(), "folded on the eligibility change");
    let after_one = workspace_unit(&f, w);
    assert_eq!(after_one.deletions().len(), 1);
    assert_eq!(after_one.restores().len(), 1);
    assert!(after_one.is_live());

    // Cycle 2: disable, move through a hidden Profile and back, enable.
    arm_group(&mut f, a, TransportMode::LocalOnly);
    f.store.move_workspace(w, stop).unwrap();
    assert!(f.overlay(w).is_some());
    f.store.move_workspace(w, a).unwrap();
    assert!(
        f.overlay(w).is_some(),
        "a destination that is Local-only keeps it private"
    );
    arm_group(&mut f, a, TransportMode::Cloud);
    if f.overlay(w).is_some() {
        assert_eq!(f.store.publish_pending_overlays().unwrap().published, 1);
    }
    assert!(f.overlay(w).is_none());
    let after_two = workspace_unit(&f, w);
    assert_eq!(after_two.deletions().len(), 1, "folded once, not twice");
    assert_eq!(after_two.restores().len(), 1);
    assert_eq!(after_two.placement().unwrap().profile, Some(a));
    assert!(after_two.is_live());
    assert_eq!(f.store.publish_pending_overlays().unwrap().published, 0);

    // What a peer may see: the inventory lists A and W only, and the derived
    // history of W names no hidden Profile anywhere.
    let grant = peer_grant();
    let visible = f.store.exportable_profiles(&grant).unwrap();
    assert!(visible.contains(&a));
    assert!(!visible.contains(&origin));
    assert!(!visible.contains(&stop));
    let inventory = f.store.root_export_inventory(&grant).unwrap();
    let listed: Vec<_> = inventory.units.iter().map(|(unit, _)| *unit).collect();
    assert!(listed.contains(&CatalogUnitRef::Workspace { workspace: w }));
    assert!(listed.contains(&CatalogUnitRef::Profile { profile: a }));
    assert!(!listed.contains(&CatalogUnitRef::Profile { profile: origin }));
    assert!(!listed.contains(&CatalogUnitRef::Profile { profile: stop }));
    assert_eq!(inventory.held, 0, "a hidden Profile is not even a number");
    let source = f
        .store
        .root_export_source(&grant, ExportSubject::WorkspaceCatalog(w))
        .unwrap()
        .expect("W may leave");
    let Derived::Publish(publication) = source.derive(None, "peer".into()).unwrap() else {
        panic!("W's current placement is visible; nothing holds it");
    };
    assert!(publication.redacted_ops >= 2, "the origin and the stop");
    let history = history_json(&publication.bytes);
    let raw = String::from_utf8_lossy(&publication.bytes).into_owned();
    for needle in [
        origin.to_storage(),
        stop.to_storage(),
        HIDDEN_ORIGIN.to_owned(),
        HIDDEN_STOP.to_owned(),
    ] {
        assert!(
            !mentions(&history, &needle),
            "the exported history names {needle}"
        );
        assert!(!raw.contains(&needle), "the exported bytes carry {needle}");
    }
    assert!(mentions(&history, &a.to_storage()));
    // The recipient reads the same current state and lifecycle.
    let recipient = WorkspaceCatalogUnit::from_frames(&[publication.bytes.clone()], w, 9).unwrap();
    let koru_store_core::root_catalog::Staged::Applied(recipient) = recipient else {
        panic!("a snapshot is never pending");
    };
    assert_eq!(recipient.placement().unwrap().profile, Some(a));
    assert_eq!(recipient.deletions().len(), 1);
    assert_eq!(recipient.restores().len(), 1);
    assert!(recipient.is_live());
    assert_eq!(recipient.version(), after_two.version(), "one lineage");
}

// -------------------------------------------- partial cascades (§6.3)

/// A batch restore whose target's unit never arrived on this device: the
/// target is reported, no row is created or deleted for it, and the rest of
/// the batch restores. (The remaining partial-cascade cases — a target
/// moved out, one deleted earlier independently, one purged — are
/// `lifecycle.rs`'s batch test.)
#[test]
fn a_batch_restore_reports_a_target_whose_unit_is_absent_and_invents_nothing() {
    let mut f = Root::new();
    let p = f.cloud_profile("P");
    let kept = f.store.create_workspace(p, "Kept", at()).unwrap().id;
    let absent = f.store.create_workspace(p, "Absent", at()).unwrap().id;
    let preview = f.store.profile_deletion_preview(p).unwrap();
    let batch = f.store.delete_profile(p, &preview, at()).unwrap();
    // The unit this device holds for `absent` goes away, as for a batch
    // whose job arrived here before every target's unit did.
    f.control
        .execute(
            "DELETE FROM root_catalog_units WHERE unit_kind = 'workspace_catalog' AND subject_id = ?1",
            params![absent.to_storage()],
        )
        .unwrap();
    let rows_before = f.store.workspaces_including_deleted().unwrap().len();

    let listed = f.store.profile_deletions(at()).unwrap();
    assert_eq!(listed.len(), 1);
    assert!(
        listed[0].expired_or_purged.contains(&absent),
        "reported as unavailable"
    );
    let outcome = f.store.restore_profile(p, batch.profile_deletion).unwrap();
    assert_eq!(outcome.restored, vec![kept]);
    assert_eq!(
        outcome.purged,
        vec![absent],
        "reported under the unavailable heading"
    );
    assert!(outcome.left_in_trash.is_empty());
    assert_eq!(
        f.store.workspaces_including_deleted().unwrap().len(),
        rows_before,
        "no row created, none removed"
    );
    assert!(f.store.workspace(kept).is_ok());
    assert!(
        f.store.workspace(absent).is_err(),
        "the row without a unit stays where it was, not resurrected"
    );
    assert!(f.unit(absent).is_none(), "and no unit was invented for it");
    assert!(f.store.profile(p).is_ok());
}

// ---------------------------------------------- old lifecycle writers

/// A manifest `deleted_at` written by a pre-13 writer has no authority in
/// either direction. `lifecycle.rs` covers a "deleted" hint on a live
/// Workspace; here a "live" hint meets a Workspace the root has trashed.
/// The manifest of a trashed Workspace stays writable on purpose — it is
/// the channel a peer's lifecycle tombstone arrives on (`ensure_document`
/// admits the manifest of a deleted Workspace and refuses every other
/// document of it) — so what is held here is that the frame undeletes
/// nothing: no durable row moves, the root fact and its projection stand,
/// and a restart reads them with the hint the root wrote.
#[test]
fn an_old_lifecycle_writer_cannot_undelete_a_unit_holding_workspace() {
    let mut f = Root::new();
    let a = f.cloud_profile("A");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    let manifest = f.store.manifest_id(w).unwrap();
    // The pre-13 peer's manifest, forked while W was live, saying "live".
    let snapshot = f.store.snapshot(w, manifest).unwrap();
    let remote = Manifest::from_snapshot(&snapshot).unwrap();
    remote.set_peer_id(99).unwrap();
    let before = remote.version();
    remote
        .set_workspace_deleted_at(Some(Timestamp::from_millis(at().millis() + 1)))
        .unwrap();
    remote.set_workspace_deleted_at(None).unwrap();
    let update = remote.export_updates_since(&before).unwrap();

    f.store.delete_workspace(w, at()).unwrap();
    assert_eq!(
        f.store.workspace_manifest_deletion(w).unwrap(),
        Some(Some(at())),
        "the hint followed the commit"
    );
    // That read opened the trashed Workspace, which repairs its disposable
    // location index; the pre-state is taken after it, so the frame below is
    // the only thing the comparison is about.
    let dumped = dump(&f);
    f.store
        .apply_update(
            w,
            manifest,
            update,
            at(),
            koru_store_core::ports::UpdateOrigin::Store,
        )
        .unwrap();
    assert_eq!(
        f.store
            .apply_update(
                w,
                DocumentId::new(),
                Vec::new(),
                at(),
                koru_store_core::ports::UpdateOrigin::Store,
            )
            .unwrap_err()
            .code(),
        Code::DocumentNotFound,
        "nothing but the manifest of a trashed Workspace is writable"
    );
    assert_eq!(dump(&f), dumped, "nothing durable changed");
    assert!(workspace_unit(&f, w).is_deleted(), "the root fact stands");
    assert_eq!(
        f.store.workspace_including_deleted(w).unwrap().deleted_at,
        Some(at()),
        "and so does the projection"
    );
    assert!(f.store.workspace(w).is_err(), "still in the Trash");
    f.restart();
    assert!(f.store.workspace(w).is_err());
    assert!(workspace_unit(&f, w).is_deleted());
    assert_eq!(
        f.store.workspace_manifest_deletion(w).unwrap(),
        Some(Some(at())),
        "the hint on disk is still what the root wrote"
    );
}

// ------------------------------------------------- duplicate reminders

/// One reminder is one due row after a move, a trash and a restore, and
/// after a restart; the index is keyed by Workspace and never doubles.
#[test]
fn the_due_index_holds_one_row_per_reminder_after_move_trash_restore_and_restart() {
    let mut f = Root::new();
    let a = f.cloud_profile("A");
    let b = f.cloud_profile("B");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    let (_, rule) = reminder_in(w);
    f.store.create_reminder(a, &rule).unwrap();
    index(&mut f.store, a);
    let one_row = |f: &mut Root, owner: ProfileId| {
        index(&mut f.store, owner);
        let rows = f.store.reminder_index_rows(owner).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reminder, rule.id);
        assert_eq!(f.count("SELECT COUNT(*) FROM workspace_reminder_due"), 1);
    };
    one_row(&mut f, a);
    f.store.move_workspace(w, b).unwrap();
    one_row(&mut f, b);
    assert!(f.store.reminder_index_rows(a).unwrap().is_empty());
    f.store.delete_workspace(w, at()).unwrap();
    assert_eq!(f.count("SELECT COUNT(*) FROM workspace_reminder_due"), 1);
    f.store.restore_workspace(w).unwrap();
    one_row(&mut f, b);
    f.restart();
    one_row(&mut f, b);
    assert_eq!(f.store.reminders(b).unwrap().len(), 1);
}

// ---------------------------------------------- no phantom workspaces

/// Every path that once manufactured a default is asserted not to: the
/// empty Profile on open, after the last Workspace moves out, after a
/// remote placement empties it, after a batch restore meets a purged
/// target, and across a restart. The `workspaces` row count and the set
/// of Workspace units move only when the person creates something.
#[test]
fn no_path_creates_a_phantom_workspace_or_a_default() {
    let mut f = Root::new();
    let p = f.cloud_profile("P");
    let q = f.cloud_profile("Q");
    let units = |f: &Root| {
        f.count("SELECT COUNT(*) FROM root_catalog_units WHERE unit_kind = 'workspace_catalog'")
    };
    let rows = |f: &Root| f.count("SELECT COUNT(*) FROM workspaces");
    assert_eq!(
        f.store.ensure_live_workspace(p, "Default", at()).unwrap(),
        EnsuredWorkspace::Empty
    );
    assert_eq!((rows(&f), units(&f)), (0, 0));

    let w = f.store.create_workspace(p, "W", at()).unwrap().id;
    assert_eq!((rows(&f), units(&f)), (1, 1));
    // The last Workspace moves out: P stays empty.
    f.store.move_workspace(w, q).unwrap();
    assert_eq!(
        f.store.ensure_live_workspace(p, "Default", at()).unwrap(),
        EnsuredWorkspace::Empty
    );
    assert_eq!((rows(&f), units(&f)), (1, 1));
    // A remote placement empties Q: unassigned, not replaced.
    remote_edit(&f, w, |unit| {
        unit.set_placement(None, DocumentId::new().as_uuid())
            .unwrap();
    });
    assert_eq!(
        f.store.ensure_live_workspace(q, "Default", at()).unwrap(),
        EnsuredWorkspace::Empty
    );
    assert_eq!(f.store.unassigned_workspaces().unwrap().len(), 1);
    assert_eq!((rows(&f), units(&f)), (1, 1));
    f.store.move_workspace(w, q).unwrap();
    // Trash and purge the only Workspace: Q is empty, nothing stands in.
    f.store.delete_workspace(w, at()).unwrap();
    assert_eq!(
        f.store.purge_deleted(after_retention(at())).unwrap().purged,
        vec![w]
    );
    assert_eq!(
        f.store.ensure_live_workspace(q, "Default", at()).unwrap(),
        EnsuredWorkspace::Empty
    );
    assert_eq!((rows(&f), units(&f)), (0, 1), "the unit stays as evidence");
    assert!(f.store.restore_workspace(w).is_err(), "nothing to restore");
    assert_eq!((rows(&f), units(&f)), (0, 1));
    // A batch restore meeting a purged target reports it and creates nothing.
    let v = f.store.create_workspace(p, "V", at()).unwrap().id;
    let preview = f.store.profile_deletion_preview(p).unwrap();
    let batch = f.store.delete_profile(p, &preview, at()).unwrap();
    remote_edit(&f, v, |unit| {
        unit.record_deletion(
            DeletionId::new(),
            &DeletionRecord {
                at_ms: at().millis(),
                retention_ms: 0,
                batch_id: None,
            },
        )
        .unwrap();
    });
    assert_eq!(f.store.purge_deleted(at()).unwrap().purged, vec![v]);
    let outcome = f.store.restore_profile(p, batch.profile_deletion).unwrap();
    assert_eq!(outcome.purged, vec![v]);
    assert!(outcome.restored.is_empty());
    assert!(f.store.profile(p).is_ok(), "the Profile is back, empty");
    assert_eq!(
        f.store.ensure_live_workspace(p, "Default", at()).unwrap(),
        EnsuredWorkspace::Empty
    );
    assert_eq!((rows(&f), units(&f)), (0, 2));
    f.restart();
    for profile in [p, q] {
        assert_eq!(
            f.store
                .ensure_live_workspace(profile, "Default", at())
                .unwrap(),
            EnsuredWorkspace::Empty
        );
    }
    assert_eq!((rows(&f), units(&f)), (0, 2));
    assert!(f.store.unassigned_workspaces().unwrap().is_empty());
}

// ------------------------------------------------ retirement and holds

/// Retirement at purge removes only this scope's row of this Workspace; a
/// held foreign-scope unit holds only its own Workspace and is never
/// deleted around; a second pass reports the same hold.
#[test]
fn retirement_removes_only_this_scopes_row_and_a_foreign_hold_is_scoped_to_its_workspace() {
    let mut f = Root::new();
    let a = f.profile("A");
    let mine = f.store.create_workspace(a, "Mine", at()).unwrap().id;
    let theirs = f.store.create_workspace(a, "Theirs", at()).unwrap().id;
    let (_, rule) = reminder_in(mine);
    f.store.create_reminder(a, &rule).unwrap();
    let (_, rule2) = reminder_in(theirs);
    f.store.create_reminder(a, &rule2).unwrap();
    let other = UserScopeId::new();
    f.catalog()
        .commit_workspace_personal_unit(
            other,
            theirs,
            &PersonalManifest::create_v2(theirs, 5)
                .unwrap()
                .export_snapshot()
                .unwrap(),
            5,
        )
        .unwrap();
    f.store.delete_workspace(mine, at()).unwrap();
    f.store.delete_workspace(theirs, at()).unwrap();
    let outcome = f.store.purge_deleted(after_retention(at())).unwrap();
    assert_eq!(outcome.purged, vec![mine]);
    assert_eq!(outcome.held, vec![(theirs, PurgeHold::ForeignScope)]);
    assert!(
        f.catalog()
            .workspace_personal_unit(f.scope(), mine)
            .unwrap()
            .is_none()
    );
    assert!(
        f.catalog()
            .workspace_personal_unit(f.scope(), theirs)
            .unwrap()
            .is_some(),
        "this scope's row of the held Workspace is not retired either: the hold comes first"
    );
    assert!(
        f.catalog()
            .workspace_personal_unit(other, theirs)
            .unwrap()
            .is_some()
    );
    assert_eq!(f.count("SELECT COUNT(*) FROM workspace_personal_crdts"), 2);
    let again = f.store.purge_deleted(after_retention(at())).unwrap();
    assert_eq!(again.held, vec![(theirs, PurgeHold::ForeignScope)]);
    assert!(again.purged.is_empty());
    assert_eq!(f.count("SELECT COUNT(*) FROM workspace_personal_crdts"), 2);
}
