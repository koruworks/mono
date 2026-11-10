// SPDX-License-Identifier: MPL-2.0

//! Local export -> fresh Loro replica, using actual in-memory Store/SQLite.
use super::*;
use client_store::ExportRequest;
use loro::VersionVector;

fn request(scope: Scope, unit: Unit, known: VersionVector, budget: &MemoryBudget) -> ExportRequest {
    ExportRequest::new(scope, unit, known, budget.clone()).unwrap()
}

#[test]
fn local_export_and_advertised_version_refuse_uncommitted_document_and_manifest() {
    for manifest in [false, true] {
        let mut f = Fixture::new();
        let document = if manifest { f.manifest } else { f.document };
        let scope = f.scope(false);
        let unit = f.unit(document);
        let (bytes, source) = f.payload(document, false);
        f.store
            .apply_update(f.workspace, document, bytes, at(), UpdateOrigin::Store)
            .unwrap();
        let budget = MemoryBudget::new(2 * transfer::MAX_PAYLOAD);
        f.deny_commit();
        assert!(matches!(
            client_store::version(&mut f.store, scope, unit, at(), |_, _, _| Ok(())),
            Err(Failure::Store(_))
        ));
        assert!(matches!(
            request(scope, unit, VersionVector::new(), &budget).run(
                &mut f.store,
                at(),
                |_, _, _| Ok(())
            ),
            Err(Failure::Store(_))
        ));
        assert_eq!(budget.used(), 0);
        assert!(f.store.take_committed().is_empty());
        f.control
            .execute_batch("DROP TRIGGER deny_client_commit")
            .unwrap();
        let export = request(scope, unit, VersionVector::new(), &budget)
            .run(&mut f.store, at(), |_, _, _| Ok(()))
            .unwrap()
            .unwrap();
        let restored = LoroDoc::new();
        restored.import(export.bytes()).unwrap();
        assert_eq!(restored.oplog_vv(), source.oplog_vv());
        assert_eq!(*export.version(), source.oplog_vv());
        assert_eq!(restored.get_deep_value(), source.get_deep_value());
        let repo = SqliteWorkspace::from_connection(connect(&f.uri)).unwrap();
        let durable = repo.document(document).unwrap().unwrap().version_vector;
        assert_eq!(VersionVector::decode(&durable).unwrap(), *export.version());
        assert!(!f.store.take_committed().is_empty());
        let advertised =
            client_store::version(&mut f.store, scope, unit, at(), |_, _, _| Ok(())).unwrap();
        assert_eq!(advertised, *export.version());
        let (bytes, guard) = export.into_payload();
        assert!(budget.used() >= bytes.capacity());
        drop(bytes);
        assert!(budget.used() > 0, "upload guard survives the moved payload");
        drop(guard);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn local_export_uses_vv_inclusion_for_equal_ahead_and_concurrent_replicas() {
    let mut f = Fixture::new();
    let scope = f.scope(false);
    let unit = f.unit(f.document);
    // A newly cataloged empty test document has no Loro operations and correctly
    // needs no transfer. Seed actual persisted content before requesting a base.
    let (seed, _) = f.payload(f.document, false);
    f.store
        .apply_update(f.workspace, f.document, seed, at(), UpdateOrigin::Store)
        .unwrap();
    let budget = MemoryBudget::new(2 * transfer::MAX_PAYLOAD);
    let remote = LoroDoc::new();
    let initial = request(scope, unit, VersionVector::new(), &budget)
        .run(&mut f.store, at(), |_, _, _| Ok(()))
        .unwrap()
        .unwrap();
    remote.import(initial.bytes()).unwrap();
    let base = remote.oplog_vv();
    drop(initial);
    for ahead in [false, true] {
        if ahead {
            remote.set_peer_id(888).unwrap();
            remote
                .get_text("remote-only")
                .insert(0, "remote edit")
                .unwrap();
            remote.commit();
        }
        assert!(
            request(scope, unit, remote.oplog_vv(), &budget)
                .run(&mut f.store, at(), |_, _, _| Ok(()))
                .unwrap()
                .is_none()
        );
        assert_eq!(budget.used(), 0);
    }
    let (bytes, source) = f.payload(f.document, true);
    f.store
        .apply_update(f.workspace, f.document, bytes, at(), UpdateOrigin::Store)
        .unwrap();
    let delta = request(scope, unit, remote.oplog_vv(), &budget)
        .run(&mut f.store, at(), |_, _, _| Ok(()))
        .unwrap()
        .unwrap();
    assert!(delta.bytes().len() > transfer::RESTRICTED_LIMIT);
    let imported = remote.import(delta.bytes()).unwrap();
    assert!(imported.pending.is_none());
    assert!(remote.oplog_vv().includes_vv(delta.version()));
    assert!(remote.oplog_vv().includes_vv(&base));
    assert_eq!(remote.get_text("remote-only").to_string(), "remote edit");
    assert_eq!(
        remote.get_text("client-receive").to_string(),
        source.get_text("client-receive").to_string()
    );
    assert!(
        request(scope, unit, remote.oplog_vv(), &budget)
            .run(&mut f.store, at(), |_, _, _| Ok(()))
            .unwrap()
            .is_none()
    );
}

#[test]
fn local_export_profiles_reuse_exact_committed_unit_snapshots_and_refuse_placeholders() {
    let mut f = Fixture::new();
    let scope = f.scope(true);
    let profile_id = scope.profile;
    let personal_unit = ProfileUnitRef::Personal {
        owner: f.profile,
        workspace: f.workspace,
    };
    let personal = Unit::Personal {
        profile_id,
        workspace_id: CloudId::from_bytes(f.workspace.as_uuid().into_bytes()),
    };
    let budget = MemoryBudget::new(2 * transfer::MAX_PAYLOAD);
    // Personal units are v2 (catalog 13). A complete v1 snapshot is still
    // admitted on the v1 address — into the Workspace's v2 unit, through the
    // migration's normalization — but nothing personal leaves on the v1
    // carrier until Plan 2.1's negotiated exchange: the export is refused,
    // before and after admission, rather than answered empty.
    let fenced = |failure: &Failure| {
        matches!(
            failure,
            Failure::Store(koru_store_core::Code::ProfileUnitScopeMismatch)
        )
    };
    assert!(
        client_store::version(&mut f.store, scope, personal, at(), |_, _, _| Ok(()))
            .is_err_and(|failure| fenced(&failure))
    );
    assert!(f.store.profile_unit_snapshot(personal_unit).is_err());
    let snapshot = PersonalManifest::create(f.profile, f.workspace)
        .unwrap()
        .export_snapshot()
        .unwrap();
    f.store
        .apply_profile_unit_updates(personal_unit, &[snapshot])
        .unwrap();
    assert!(
        client_store::version(&mut f.store, scope, personal, at(), |_, _, _| Ok(()))
            .is_err_and(|failure| fenced(&failure))
    );
    assert!(
        request(scope, personal, VersionVector::new(), &budget)
            .run(&mut f.store, at(), |_, _, _| Ok(()))
            .is_err_and(|failure| fenced(&failure))
    );
    assert_eq!(budget.used(), 0);
    for (unit, stored) in [
        (
            Unit::Identity { profile_id },
            ProfileUnitRef::Identity { profile: f.profile },
        ),
        (
            Unit::Private { profile_id },
            ProfileUnitRef::Private { profile: f.profile },
        ),
    ] {
        let exact = f.store.profile_unit_snapshot(stored).unwrap();
        let export = request(scope, unit, VersionVector::new(), &budget)
            .run(&mut f.store, at(), |_, _, _| Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(export.unit(), unit);
        assert_eq!(export.bytes(), exact);
        let restored = LoroDoc::new();
        restored.import(export.bytes()).unwrap();
        assert_eq!(*export.version(), restored.oplog_vv());
        let base = restored.oplog_vv();
        assert!(
            request(scope, unit, restored.oplog_vv(), &budget)
                .run(&mut f.store, at(), |_, _, _| Ok(()))
                .unwrap()
                .is_none()
        );
        drop(export);
        assert_eq!(budget.used(), 0);
        let edited = match unit {
            Unit::Identity { .. } => {
                let doc = IdentityManifest::from_snapshot(&exact).unwrap();
                doc.set_peer_id(883).unwrap();
                doc.set_name("Exported identity").unwrap();
                doc.export_snapshot().unwrap()
            }
            Unit::Private { .. } => {
                let doc = PrivateManifest::from_snapshot(&exact).unwrap();
                doc.set_peer_id(883).unwrap();
                doc.add_dictionary_word("en", "exportword").unwrap();
                doc.export_snapshot().unwrap()
            }
            Unit::Personal { .. } => {
                let doc = PersonalManifest::from_snapshot(&exact).unwrap();
                doc.set_peer_id(883).unwrap();
                doc.delete_reminder(koru_store_core::ReminderId::new())
                    .unwrap();
                doc.export_snapshot().unwrap()
            }
            Unit::Document { .. } => unreachable!(),
        };
        let source = LoroDoc::new();
        source.import(&edited).unwrap();
        let bytes = source.export(ExportMode::updates(&base)).unwrap();
        f.store
            .apply_profile_unit_updates(stored, &[bytes])
            .unwrap();
        let delta = request(scope, unit, base, &budget)
            .run(&mut f.store, at(), |_, _, _| Ok(()))
            .unwrap()
            .unwrap();
        let status = restored.import(delta.bytes()).unwrap();
        assert!(status.pending.is_none());
        assert_eq!(restored.oplog_vv(), source.oplog_vv());
        assert_eq!(*delta.version(), source.oplog_vv());
        // Typed Profile readers declare empty roots without CRDT operations.
        // A delta cannot transmit those declarations. Normalize the same
        // documented read-only roots before comparing semantic content.
        match unit {
            Unit::Private { .. } => {
                restored.get_map("reminders");
            }
            Unit::Personal { .. } => {
                restored.get_map("reminders");
                restored.get_map("personal_attributes");
            }
            _ => {}
        }
        assert_eq!(restored.get_deep_value(), source.get_deep_value());
        assert_eq!(restored.oplog_vv(), source.oplog_vv());
        drop(delta);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn local_export_reserves_before_store_access_and_rechecks_authority_and_manifest() {
    let mut f = Fixture::new();
    let scope = f.scope(false);
    let unit = f.unit(f.document);
    let small = MemoryBudget::new(transfer::MAX_PAYLOAD - 1);
    assert!(matches!(
        ExportRequest::new(scope, unit, VersionVector::new(), small.clone()),
        Err(transfer::Error::Unavailable)
    ));
    assert_eq!(small.used(), 0);
    let budget = MemoryBudget::new(transfer::MAX_PAYLOAD);
    assert!(matches!(
        request(scope, unit, VersionVector::new(), &budget)
            .run(&mut f.store, at(), |_, _, _| Err(Refusal::CloudDisabled)),
        Err(Failure::Transfer(transfer::Error::Authority(
            Refusal::CloudDisabled
        )))
    ));
    assert_eq!(budget.used(), 0);
    let wrong = Scope {
        manifest: DocumentId::new(),
        ..scope
    };
    assert!(matches!(
        request(wrong, unit, VersionVector::new(), &budget)
            .run(&mut f.store, at(), |_, _, _| Ok(())),
        Err(Failure::Transfer(transfer::Error::Invalid))
    ));
    assert_eq!(budget.used(), 0);
    assert!(f.store.take_committed().is_empty());
}

#[test]
fn local_export_refuses_actual_oversized_loro_bytes_without_fragmenting_or_checkpoint_fallback() {
    let mut f = Fixture::new();
    let scope = f.scope(false);
    let unit = f.unit(f.document);
    let source = LoroDoc::new();
    source
        .import(&f.store.snapshot(f.workspace, f.document).unwrap())
        .unwrap();
    source.set_peer_id(889).unwrap();
    let known = source.oplog_vv();
    let mut state = 0x12345678u32;
    let data: Vec<u8> = (0..transfer::MAX_PAYLOAD + 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    source
        .get_map("export")
        .insert("oversized", loro::LoroValue::Binary(data.into()))
        .unwrap();
    source.commit();
    let bytes = source.export(ExportMode::updates(&known)).unwrap();
    assert!(bytes.len() > transfer::MAX_PAYLOAD);
    f.store
        .apply_update(f.workspace, f.document, bytes, at(), UpdateOrigin::Store)
        .unwrap();
    let budget = MemoryBudget::new(transfer::MAX_PAYLOAD);
    assert!(matches!(
        request(scope, unit, known, &budget).run(&mut f.store, at(), |_, _, _| Ok(())),
        Err(Failure::Transfer(transfer::Error::TooLarge))
    ));
    assert_eq!(budget.used(), 0);
    assert_eq!(
        f.store.version(f.workspace, f.document).unwrap(),
        source.oplog_vv()
    );
}
