// SPDX-License-Identifier: MPL-2.0

//! Explicit disk lane: never discovers or connects to a running Store.
use super::*;
use crate::SqliteWorkspace;
use koru_device::cloud::CloudReplicaPrincipalV1;
use koru_store_core::ports::WorkspaceRepo;

#[test]
fn cloud_origin_survives_real_store_apply_flush_drop_and_reopen() {
    let directory = isolated_directory();
    let catalog = directory.path().join("catalog.db");
    let v: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../contracts/vectors/cloud-sync-admission-v1/foundation.json"
    ))
    .unwrap();
    let principal: CloudReplicaPrincipalV1 =
        serde_json::from_value(v["sessions"][0]["assignment"]["dataset"].clone()).unwrap();
    let expected = principal.update_origin();
    let mut store = Store::open(
        SqliteCatalog::open(&catalog).unwrap(),
        SqliteBackend::new(directory.path()),
    )
    .unwrap();
    let workspace = workspace(&mut store);
    let (document, _) = store
        .create_document(workspace, "Cloud origin", "document", None, None, at())
        .unwrap();
    let source = loro::LoroDoc::new();
    source
        .get_map("test")
        .insert("text", "durable cloud bytes")
        .unwrap();
    source.commit();
    let delta = source.export(loro::ExportMode::all_updates()).unwrap();
    store
        .apply_update(workspace, document, delta.clone(), at(), expected.clone())
        .unwrap();
    assert!(store.flush(workspace, document, at()).unwrap().is_some());
    drop(store);
    // New file handles and a new Store instance replay the persisted bytes.
    let mut reopened = Store::open(
        SqliteCatalog::open(&catalog).unwrap(),
        SqliteBackend::new(directory.path()),
    )
    .unwrap();
    let recovered = loro::LoroDoc::new();
    recovered
        .import(&reopened.snapshot(workspace, document).unwrap())
        .unwrap();
    assert_eq!(recovered.get_deep_value(), source.get_deep_value());
    drop(reopened);
    let path = SqliteBackend::new(directory.path()).workspace_db(workspace);
    let repository = SqliteWorkspace::open(&path).unwrap();
    let updates = repository.updates_after(document, None).unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].origin, expected);
    assert_eq!(updates[0].bytes, delta);
}

fn isolated_directory() -> tempfile::TempDir {
    let root = std::path::PathBuf::from(
        std::env::var_os("KORU_ROOT").expect("isolated KORU_ROOT required"),
    );
    assert!(root.is_absolute());
    assert_eq!(root.file_name().unwrap(), "plan10-test-root");
    assert_eq!(root.parent().unwrap().file_name().unwrap(), "target");
    std::fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("cloud-origin-")
        .tempdir_in(&root)
        .unwrap()
}

#[test]
fn profile_carrier_handoff_is_causal_scoped_and_durable_for_all_three_units() {
    use koru_store_core::{
        Code, Profile, Timestamp,
        ports::Catalog,
        profile_units::{IdentityManifest, PersonalManifest, PrivateManifest, ProfileUnitRef},
    };
    use loro::{ExportMode, LoroDoc};
    for kind in 0..3 {
        for mixed in [false, true] {
            let directory = isolated_directory();
            let path = directory.path().join("catalog.db");
            let mut store = Store::open(
                SqliteCatalog::open(&path).unwrap(),
                SqliteBackend::new(directory.path()),
            )
            .unwrap();
            let profile = store
                .create_profile("Profile", None, None, Locale::default(), at())
                .unwrap();
            let workspace = store
                .create_workspace(profile.id, "Workspace", at())
                .unwrap()
                .id;
            let unit = match kind {
                0 => ProfileUnitRef::Identity {
                    profile: profile.id,
                },
                1 => ProfileUnitRef::Private {
                    profile: profile.id,
                },
                _ => ProfileUnitRef::Personal {
                    owner: profile.id,
                    workspace,
                },
            };
            if kind == 2 {
                let initial = PersonalManifest::create(profile.id, workspace)
                    .unwrap()
                    .export_snapshot()
                    .unwrap();
                profile_carrier_apply(&mut store, workspace, unit, initial).unwrap();
            }
            let repository = SqliteCatalog::open(&path).unwrap();
            let before_state = repository.profile_unit(unit).unwrap().unwrap();
            let before_profile = store.profile(profile.id).unwrap();
            let before = LoroDoc::new();
            before.import(&before_state.snapshot).unwrap();
            let source = before.fork();
            source.set_peer_id(710).unwrap();
            profile_fixture_edit(&source, kind, 0);
            let ancestor = source
                .export(ExportMode::updates(&before.oplog_vv()))
                .unwrap();
            let after_ancestor = source.oplog_vv();
            profile_fixture_edit(&source, kind, 1);
            if mixed {
                let independent = LoroDoc::new();
                independent.set_peer_id(711).unwrap();
                profile_fixture_edit(&independent, kind, 2);
                source
                    .import(&independent.export(ExportMode::all_updates()).unwrap())
                    .unwrap();
            }
            let child = source.export(ExportMode::updates(&after_ancestor)).unwrap();
            let oracle = before.fork();
            assert!(oracle.import(&child).unwrap().pending.is_some());
            assert_eq!(
                profile_carrier_apply(&mut store, workspace, unit, child.clone())
                    .unwrap_err()
                    .code(),
                Code::CrdtRejected
            );
            assert_eq!(
                repository.profile_unit(unit).unwrap().unwrap(),
                before_state
            );
            assert_eq!(store.profile(profile.id).unwrap(), before_profile);
            assert_eq!(store.profile_unit_version(unit).unwrap(), before.oplog_vv());
            // A correctly framed payload naming a different owner must not change
            // either the addressed unit or the relational Profile projection.
            let wrong =
                Profile::new("Wrong", Locale::default(), Timestamp::from_millis(42)).unwrap();
            let wrong_bytes = match kind {
                0 => IdentityManifest::create(&wrong)
                    .unwrap()
                    .export_snapshot()
                    .unwrap(),
                1 => PrivateManifest::create(wrong.id)
                    .unwrap()
                    .export_snapshot()
                    .unwrap(),
                _ => PersonalManifest::create(wrong.id, workspace)
                    .unwrap()
                    .export_snapshot()
                    .unwrap(),
            };
            // Existing staging validates a complete incoming snapshot's own scope
            // before merging, even if its metadata would lose the LWW conflict.
            assert_eq!(
                profile_carrier_apply(&mut store, workspace, unit, wrong_bytes)
                    .unwrap_err()
                    .code(),
                Code::ProfileUnitScopeMismatch
            );
            assert_eq!(
                repository.profile_unit(unit).unwrap().unwrap(),
                before_state
            );
            assert_eq!(store.profile(profile.id).unwrap(), before_profile);
            profile_carrier_apply(&mut store, workspace, unit, ancestor).unwrap();
            let after = LoroDoc::new();
            after
                .import(&repository.profile_unit(unit).unwrap().unwrap().snapshot)
                .unwrap();
            assert_eq!(
                after.oplog_vv(),
                after_ancestor,
                "refused child/subset was not retained"
            );
            profile_carrier_apply(&mut store, workspace, unit, child).unwrap();
            drop(store);
            let mut reopened = Store::open(
                SqliteCatalog::open(&path).unwrap(),
                SqliteBackend::new(directory.path()),
            )
            .unwrap();
            assert_eq!(
                reopened.profile_unit_version(unit).unwrap(),
                source.oplog_vv()
            );
            let actual = LoroDoc::new();
            actual
                .import(&repository.profile_unit(unit).unwrap().unwrap().snapshot)
                .unwrap();
            // Unit validation materializes its allowed empty map views without
            // authoring operations. Compare the same views on the raw fixture.
            if kind == 1 {
                source.get_map("reminders");
                source.get_map("personal_dictionary");
            }
            if kind == 2 {
                source.get_map("reminders");
                source.get_map("personal_attributes");
            }
            assert_eq!(actual.get_deep_value(), source.get_deep_value());
        }
    }
}

fn profile_fixture_edit(doc: &loro::LoroDoc, kind: usize, step: usize) {
    match kind {
        0 => doc
            .get_map("meta")
            .insert(["purpose", "avatar", "icon"][step], "carrier value")
            .unwrap(),
        1 => doc
            .get_map("personal_dictionary")
            .ensure_mergeable_map("en")
            .unwrap()
            .insert(["ancestor", "dependent", "independent"][step], true)
            .unwrap(),
        _ => doc
            .get_map("reminders")
            .ensure_mergeable_map("deleted")
            .unwrap()
            .insert(&koru_store_core::ReminderId::new().to_storage(), true)
            .unwrap(),
    };
    doc.commit();
}

fn profile_carrier_apply(
    store: &mut Store<SqliteCatalog, SqliteBackend>,
    workspace: WorkspaceId,
    unit: koru_store_core::profile_units::ProfileUnitRef,
    bytes: Vec<u8>,
) -> koru_store_core::Result<()> {
    use koru_device::cloud::CloudId;
    use koru_replication::cloud::{Bytes, HandshakeV1, PreparedSession, Version1, transfer::*};
    use koru_store_core::profile_units::ProfileUnitRef;
    let pid = CloudId::from_bytes(*unit.profile().as_uuid().as_bytes());
    let wid = CloudId::from_bytes(*workspace.as_uuid().as_bytes());
    let transfer_unit = match unit {
        ProfileUnitRef::Identity { .. } => Unit::Identity { profile_id: pid },
        ProfileUnitRef::Private { .. } => Unit::Private { profile_id: pid },
        ProfileUnitRef::Personal { .. } => Unit::Personal {
            profile_id: pid,
            workspace_id: wid,
        },
        _ => panic!(),
    };
    let principal = CloudReplicaPrincipalV1::Profile {
        profile_id: pid,
        lineage_id: wid,
        logical_replica_id: pid,
    };
    let accepted = PreparedSession {
        accepted: HandshakeV1::SessionAccepted {
            version: Version1,
            session_id: Bytes([0; 32]),
            generation: 1,
            expires_at_seconds: 600,
        },
        principal,
        expires_at_seconds: 600,
        generation: 1,
        claim_references: koru_replication::cloud::transfer::ClaimReferences::None,
    };
    let scope = Scope {
        principal,
        eligibility_workspace: workspace,
        profile: pid,
        manifest: store.manifest_id(workspace).unwrap(),
    };
    let budget = MemoryBudget::new(1024 * 1024 * 1024);
    let mut sender = Stream::new(&accepted, scope, 0, 0, MAX_PAYLOAD, budget.clone()).unwrap();
    let mut receiver = Stream::new(&accepted, scope, 0, 0, MAX_PAYLOAD, budget.clone()).unwrap();
    let sp = sender.initial_policy(0).unwrap();
    let rp = receiver.initial_policy(0).unwrap();
    sender.receive_control(rp, 0, 0, |_| Ok(())).unwrap();
    receiver.receive_control(sp, 0, 0, |_| Ok(())).unwrap();
    let (start, _) = sender
        .begin_outgoing(transfer_unit, [0; 8], bytes, 1, 1, |_| Ok(()))
        .unwrap();
    receiver.receive_control(start, 1, 1, |_| Ok(())).unwrap();
    while let Some(record) = sender.next_outgoing(2, |_| Ok(())).unwrap() {
        let encoded = encode_record(record.record()).unwrap();
        let Record::Peer(frame) = decode_record(encoded[4], &encoded[5..]).unwrap() else {
            panic!()
        };
        receiver.receive_peer(frame, 2, |_| Ok(())).unwrap();
    }
    // Profile Store commits its catalog transaction synchronously; unlike
    // workspace documents there is no separate group-window flush to call.
    let result = receiver
        .handoff(
            3,
            |_| Ok(()),
            |done| store.apply_profile_unit_updates(unit, &[done.bytes]),
        )
        .unwrap();
    sender.outbound_written(3).unwrap();
    assert_eq!(budget.used(), 0);
    result
}

#[test]
fn cloud_missing_ancestors_never_enter_document_or_manifest_state_or_journal() {
    use koru_store_core::{Code, ports::UpdateOrigin};
    use loro::{ExportMode, LoroDoc};
    for manifest in [false, true] {
        for mixed in [false, true] {
            let directory = isolated_directory();
            let catalog = directory.path().join("catalog.db");
            let mut store = Store::open(
                SqliteCatalog::open(&catalog).unwrap(),
                SqliteBackend::new(directory.path()),
            )
            .unwrap();
            let workspace = workspace(&mut store);
            let document = if manifest {
                store.manifest_id(workspace).unwrap()
            } else {
                store
                    .create_document(workspace, "Causal", "document", None, None, at())
                    .unwrap()
                    .0
            };
            let _ = store.take_committed();
            let before = LoroDoc::new();
            before
                .import(&store.snapshot(workspace, document).unwrap())
                .unwrap();
            let source = before.fork();
            source.set_peer_id(710).unwrap();
            source.get_map("causal_test").insert("ancestor", 1).unwrap();
            source.commit();
            let ancestor = source
                .export(ExportMode::updates(&before.oplog_vv()))
                .unwrap();
            let after_ancestor = source.oplog_vv();
            source
                .get_map("causal_test")
                .insert("dependent", 2)
                .unwrap();
            source.commit();
            if mixed {
                let independent = LoroDoc::new();
                independent.set_peer_id(711).unwrap();
                independent
                    .get_map("independent_test")
                    .insert("must_not_leak", 3)
                    .unwrap();
                independent.commit();
                source
                    .import(&independent.export(ExportMode::all_updates()).unwrap())
                    .unwrap();
            }
            let update = source.export(ExportMode::updates(&after_ancestor)).unwrap();
            let oracle = before.fork();
            let result = oracle.import(&update).unwrap();
            assert!(
                result.pending.is_some(),
                "fixture must actually lack ancestors"
            );
            if mixed {
                assert!(
                    oracle
                        .get_map("independent_test")
                        .get("must_not_leak")
                        .is_some()
                );
            }
            let origin = UpdateOrigin::Cloud {
                logical_replica_id: [9; 16],
            };
            let repository = SqliteWorkspace::open(
                &SqliteBackend::new(directory.path()).workspace_db(workspace),
            )
            .unwrap();
            let old_state = repository.document(document).unwrap();
            let old_updates = repository.updates_after(document, None).unwrap();
            let old_checkpoints = repository.checkpoints(document).unwrap();
            assert_eq!(
                carrier_apply(&mut store, workspace, document, update.clone())
                    .unwrap_err()
                    .code(),
                Code::CrdtRejected
            );
            assert_eq!(store.flush(workspace, document, at()).unwrap(), None);
            assert!(store.take_committed().is_empty());
            assert_eq!(repository.document(document).unwrap(), old_state);
            assert_eq!(
                repository.updates_after(document, None).unwrap(),
                old_updates
            );
            assert_eq!(repository.checkpoints(document).unwrap(), old_checkpoints);
            let unchanged = LoroDoc::new();
            unchanged
                .import(&store.snapshot(workspace, document).unwrap())
                .unwrap();
            assert_eq!(unchanged.get_deep_value(), before.get_deep_value());
            assert_eq!(unchanged.oplog_vv(), before.oplog_vv());
            // Deliver ancestors without retrying the refused frame: no retained
            // pending child and no previously applied independent subset may wake.
            store
                .apply_update(workspace, document, ancestor, at(), origin.clone())
                .unwrap();
            assert!(store.flush(workspace, document, at()).unwrap().is_some());
            let after = LoroDoc::new();
            after
                .import(&store.snapshot(workspace, document).unwrap())
                .unwrap();
            assert!(after.get_map("causal_test").get("dependent").is_none());
            assert!(
                after
                    .get_map("independent_test")
                    .get("must_not_leak")
                    .is_none()
            );
            let _ = store.take_committed();
            carrier_apply(&mut store, workspace, document, update).unwrap();
            assert!(store.flush(workspace, document, at()).unwrap().is_some());
            drop(store);
            let mut reopened = Store::open(
                SqliteCatalog::open(&catalog).unwrap(),
                SqliteBackend::new(directory.path()),
            )
            .unwrap();
            let actual = LoroDoc::new();
            actual
                .import(&reopened.snapshot(workspace, document).unwrap())
                .unwrap();
            assert_eq!(actual.get_deep_value(), source.get_deep_value());
        }
    }
}

/// Compose production framing/state with the real Store boundary without
/// registering a network endpoint or pretending the handoff is a durable ACK.
fn carrier_apply(
    store: &mut Store<SqliteCatalog, SqliteBackend>,
    workspace: WorkspaceId,
    document: koru_store_core::DocumentId,
    bytes: Vec<u8>,
) -> koru_store_core::Result<()> {
    use koru_device::cloud::CloudId;
    use koru_replication::cloud::{Bytes, HandshakeV1, PreparedSession, Version1, transfer::*};
    let wid = CloudId::from_bytes(*workspace.as_uuid().as_bytes());
    let did = CloudId::from_bytes(*document.as_uuid().as_bytes());
    let principal = CloudReplicaPrincipalV1::Workspace {
        workspace_id: wid,
        incarnation_id: did,
        logical_replica_id: CloudId::from_bytes([9; 16]),
    };
    let accepted = PreparedSession {
        accepted: HandshakeV1::SessionAccepted {
            version: Version1,
            session_id: Bytes([0; 32]),
            generation: 1,
            expires_at_seconds: 600,
        },
        principal,
        expires_at_seconds: 600,
        generation: 1,
        claim_references: koru_replication::cloud::transfer::ClaimReferences::None,
    };
    let scope = Scope {
        principal,
        eligibility_workspace: workspace,
        profile: did,
        manifest: store.manifest_id(workspace).unwrap(),
    };
    let budget = MemoryBudget::new(1024 * 1024 * 1024);
    let mut sender = Stream::new(&accepted, scope, 0, 0, MAX_PAYLOAD, budget.clone()).unwrap();
    let mut receiver = Stream::new(&accepted, scope, 0, 0, MAX_PAYLOAD, budget.clone()).unwrap();
    let sp = sender.initial_policy(0).unwrap();
    let rp = receiver.initial_policy(0).unwrap();
    sender.receive_control(rp, 0, 0, |_| Ok(())).unwrap();
    receiver.receive_control(sp, 0, 0, |_| Ok(())).unwrap();
    let (start, _) = sender
        .begin_outgoing(
            Unit::Document {
                workspace_id: wid,
                document_id: did,
            },
            [0; 8],
            bytes,
            1,
            1,
            |_| Ok(()),
        )
        .unwrap();
    receiver.receive_control(start, 1, 1, |_| Ok(())).unwrap();
    while let Some(record) = sender.next_outgoing(2, |_| Ok(())).unwrap() {
        let encoded = encode_record(record.record()).unwrap();
        let Record::Peer(frame) = decode_record(encoded[4], &encoded[5..]).unwrap() else {
            panic!()
        };
        receiver.receive_peer(frame, 2, |_| Ok(())).unwrap();
    }
    assert!(
        store.take_committed().is_empty(),
        "carrier cannot emit Store commit events"
    );
    let result = receiver
        .handoff(
            3,
            |_| Ok(()),
            |done| store.apply_update(workspace, document, done.bytes, at(), done.origin),
        )
        .unwrap();
    assert!(
        store.take_committed().is_empty(),
        "handoff cannot acknowledge before flush"
    );
    sender.outbound_written(3).unwrap();
    assert_eq!(budget.used(), 0);
    result
}
