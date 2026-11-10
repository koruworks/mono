// SPDX-License-Identifier: MPL-2.0

//! Memory-only: in-memory Loro documents, no disk, no Store, no network.

use super::*;
use crate::ids::DeletionId;
use crate::service::TRASH_RETENTION_MS;
use crate::version::{CATALOG_UNIT_FORMAT_VERSION, Compatibility};
use loro::{ExportMode, LoroDoc, LoroValue};

fn placed(writer: u64) -> (WorkspaceCatalogUnit, WorkspaceId, ProfileId, Uuid) {
    let workspace = WorkspaceId::new();
    let profile = ProfileId::new();
    let move_id = Uuid::new_v4();
    let unit = WorkspaceCatalogUnit::create(workspace, writer).unwrap();
    unit.set_placement(Some(profile), move_id).unwrap();
    (unit, workspace, profile, move_id)
}

/// The raw document behind a valid snapshot, for tampering one thing at a
/// time. Every tamper is committed and re-exported through `load`.
fn tampered(
    unit: &WorkspaceCatalogUnit,
    tamper: impl FnOnce(&LoroDoc),
) -> Result<WorkspaceCatalogUnit, CatalogUnitRefusal> {
    let doc = LoroDoc::new();
    doc.import(&unit.snapshot().unwrap()).unwrap();
    tamper(&doc);
    doc.commit();
    WorkspaceCatalogUnit::from_snapshot(
        &doc.export(ExportMode::snapshot()).unwrap(),
        unit.workspace(),
        99,
    )
}

/// The document's history as Loro's typed JSON, **without peer
/// compression**: the compressed form rewrites every operation id's peer to
/// an index into its own `peers` table, so a document written only by peer 7
/// reports `0@0` and an assertion about which peer authored something reads
/// the table's indexes rather than the ids.
fn history(doc: &LoroDoc) -> serde_json::Value {
    serde_json::to_value(doc.export_json_updates_without_peer_compression(
        &loro::VersionVector::default(),
        &doc.oplog_vv(),
    ))
    .unwrap()
}

fn record(pairs: &[(&str, serde_json::Value)]) -> LoroValue {
    let mut object = serde_json::Map::new();
    for (key, value) in pairs {
        object.insert((*key).to_string(), value.clone());
    }
    LoroValue::from(serde_json::Value::Object(object))
}

#[test]
fn unit_ref_names_the_row_columns_and_round_trips() {
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    let p = CatalogUnitRef::Profile { profile };
    let w = CatalogUnitRef::Workspace { workspace };
    assert_eq!(p.kind().as_str(), "profile_catalog");
    assert_eq!(w.kind().as_str(), "workspace_catalog");
    assert_eq!(p.subject_id(), profile.to_storage());
    assert_eq!(w.subject_id(), workspace.to_storage());
    for unit in [p, w] {
        assert_eq!(
            CatalogUnitRef::parse(unit.kind().as_str(), &unit.subject_id()),
            Some(unit)
        );
        let json = serde_json::to_value(unit).unwrap();
        assert_eq!(json["type"], unit.kind().as_str());
        assert_eq!(
            serde_json::from_value::<CatalogUnitRef>(json).unwrap(),
            unit
        );
    }
    assert_eq!(
        CatalogUnitRef::parse("identity", &profile.to_storage()),
        None
    );
    assert_eq!(
        CatalogUnitRef::parse("profile_catalog", &profile.to_storage().to_uppercase()),
        None,
        "a non-canonical subject never resolves to an address"
    );
    assert!(
        serde_json::from_value::<CatalogUnitRef>(serde_json::json!({
            "type": "profile_catalog", "profile": profile, "workspace": workspace
        }))
        .is_err()
    );
    assert_eq!(p.profile(), Some(profile));
    assert_eq!(p.workspace(), None);
    assert_eq!(w.workspace(), Some(workspace));
}

#[test]
fn fresh_units_carry_only_the_header_under_the_writer_peer_with_no_messages() {
    let profile = ProfileCatalogUnit::create(ProfileId::new(), 11).unwrap();
    let (workspace, _, _, _) = placed(12);
    workspace
        .record_deletion(DeletionId::new(), &DeletionRecord::new(5, None))
        .unwrap();
    assert_eq!(profile.writer_peer(), 11);
    assert_eq!(workspace.writer_peer(), 12);
    for (doc, peer) in [(&profile.inner.doc, 11), (&workspace.inner.doc, 12)] {
        let decoded = history(doc);
        let changes = decoded["changes"].as_array().unwrap();
        assert!(!changes.is_empty());
        for change in changes {
            assert!(change["msg"].is_null(), "catalog commits carry no message");
            assert!(
                change["id"]
                    .as_str()
                    .unwrap()
                    .ends_with(&format!("@{peer}"))
            );
        }
    }
    let LoroValue::Map(roots) = profile.inner.doc.get_deep_value() else {
        unreachable!()
    };
    assert_eq!(roots.keys().collect::<Vec<_>>(), vec!["meta"]);
    let meta = profile.inner.doc.get_map("meta");
    assert_eq!(
        crate::unit_doc::integer(&meta, "schema_version"),
        Some(CATALOG_UNIT_FORMAT_VERSION)
    );
    assert_eq!(
        crate::unit_doc::string(&meta, "unit_kind").as_deref(),
        Some("profile_catalog")
    );
    assert_eq!(
        crate::unit_doc::string(&meta, "profile_id"),
        Some(profile.profile().to_storage())
    );
    assert!(meta.get("workspace_id").is_none());
    assert_eq!(
        ProfileCatalogUnit::from_snapshot(&profile.snapshot().unwrap(), profile.profile(), 3)
            .unwrap()
            .writer_peer(),
        3,
        "the loader hands the writer stream to the caller's persisted peer"
    );
}

#[test]
fn loaders_refuse_header_and_address_mismatches() {
    let (unit, workspace, _, _) = placed(1);
    let bytes = unit.snapshot().unwrap();
    assert_eq!(
        WorkspaceCatalogUnit::from_snapshot(&bytes, WorkspaceId::new(), 1).unwrap_err(),
        CatalogUnitRefusal::HeaderMismatch {
            key: "workspace_id"
        }
    );
    assert_eq!(
        ProfileCatalogUnit::from_snapshot(&bytes, ProfileId::from_uuid(workspace.as_uuid()), 1)
            .unwrap_err(),
        CatalogUnitRefusal::WrongUnitKind {
            found: Some("workspace_catalog".into())
        },
        "the same UUID under the other family is a different unit"
    );
    assert_eq!(
        tampered(&unit, |doc| {
            doc.get_map("meta")
                .insert("workspace_id", workspace.to_storage().to_uppercase())
                .unwrap();
        })
        .unwrap_err(),
        CatalogUnitRefusal::HeaderMismatch {
            key: "workspace_id"
        },
        "a non-canonical spelling of the right id is not the right header"
    );
    assert_eq!(
        tampered(&unit, |doc| {
            doc.get_map("meta").delete("unit_kind").unwrap();
        })
        .unwrap_err(),
        CatalogUnitRefusal::WrongUnitKind { found: None }
    );
    assert_eq!(
        tampered(&unit, |doc| {
            doc.get_map("meta").delete("schema_version").unwrap();
        })
        .unwrap_err(),
        CatalogUnitRefusal::SchemaAbsent
    );
    assert_eq!(
        WorkspaceCatalogUnit::from_snapshot(b"not loro", workspace, 1).unwrap_err(),
        CatalogUnitRefusal::UndecodableSnapshot
    );
}

#[test]
fn validation_refuses_every_malformed_shape_with_a_typed_reason() {
    let (unit, _, profile, _) = placed(1);
    let deletion = DeletionId::new();
    let restore = RestoreId::new();
    type Tamper = Box<dyn FnOnce(&LoroDoc)>;
    let cases: Vec<(&str, Tamper, CatalogUnitRefusal)> = vec![
        (
            "unknown root",
            Box::new(|doc| {
                doc.get_map("owner").insert("x", true).unwrap();
            }),
            CatalogUnitRefusal::UnknownRoot {
                root: "owner".into(),
            },
        ),
        (
            "root is not a map",
            Box::new(|doc| {
                doc.get_list("deletions").push(1).unwrap();
            }),
            CatalogUnitRefusal::RootNotMap {
                root: "deletions".into(),
            },
        ),
        (
            "unknown meta key",
            Box::new(|doc| {
                doc.get_map("meta")
                    .insert("transport_mode", "cloud")
                    .unwrap();
            }),
            CatalogUnitRefusal::UnknownKey {
                root: "meta",
                key: "transport_mode".into(),
            },
        ),
        (
            "meta names the other family's id too",
            Box::new(move |doc| {
                doc.get_map("meta")
                    .insert("profile_id", profile.to_storage())
                    .unwrap();
            }),
            CatalogUnitRefusal::UnknownKey {
                root: "meta",
                key: "profile_id".into(),
            },
        ),
        (
            "unknown placement key",
            Box::new(|doc| {
                doc.get_map("placement")
                    .insert("previous", LoroValue::Null)
                    .unwrap();
            }),
            CatalogUnitRefusal::UnknownKey {
                root: "placement",
                key: "previous".into(),
            },
        ),
        (
            "placement missing a field",
            Box::new(move |doc| {
                doc.get_map("placement")
                    .insert(
                        "current",
                        record(&[("profile_id", serde_json::json!(profile.to_storage()))]),
                    )
                    .unwrap();
            }),
            CatalogUnitRefusal::RecordShape {
                path: "placement.current".into(),
            },
        ),
        (
            "placement with an extra field",
            Box::new(move |doc| {
                doc.get_map("placement")
                    .insert(
                        "current",
                        record(&[
                            ("profile_id", serde_json::json!(profile.to_storage())),
                            ("move_id", serde_json::json!(Uuid::new_v4().to_string())),
                            ("name", serde_json::json!("Work")),
                        ]),
                    )
                    .unwrap();
            }),
            CatalogUnitRefusal::RecordShape {
                path: "placement.current".into(),
            },
        ),
        (
            "placement profile is not canonical",
            Box::new(move |doc| {
                doc.get_map("placement")
                    .insert(
                        "current",
                        record(&[
                            (
                                "profile_id",
                                serde_json::json!(profile.to_storage().to_uppercase()),
                            ),
                            ("move_id", serde_json::json!(Uuid::new_v4().to_string())),
                        ]),
                    )
                    .unwrap();
            }),
            CatalogUnitRefusal::MalformedUuid {
                path: "placement.current.profile_id".into(),
            },
        ),
        (
            "placement move id is not a UUID",
            Box::new(move |doc| {
                doc.get_map("placement")
                    .insert(
                        "current",
                        record(&[
                            ("profile_id", serde_json::Value::Null),
                            ("move_id", serde_json::json!("move-1")),
                        ]),
                    )
                    .unwrap();
            }),
            CatalogUnitRefusal::MalformedUuid {
                path: "placement.current.move_id".into(),
            },
        ),
        (
            "placement as a child container",
            Box::new(|doc| {
                let placement = doc.get_map("placement");
                placement.delete("current").unwrap();
                placement
                    .ensure_mergeable_map("current")
                    .unwrap()
                    .insert("move_id", Uuid::new_v4().to_string())
                    .unwrap();
            }),
            CatalogUnitRefusal::NotAtomic {
                path: "placement.current".into(),
            },
        ),
        (
            "deletion key is not a UUID",
            Box::new(|doc| {
                doc.get_map("deletions")
                    .insert(
                        "first",
                        record(&[
                            ("at_ms", serde_json::json!(1)),
                            ("retention_ms", serde_json::json!(1)),
                            ("batch_id", serde_json::Value::Null),
                        ]),
                    )
                    .unwrap();
            }),
            CatalogUnitRefusal::MalformedUuid {
                path: "deletions.first".into(),
            },
        ),
        (
            "negative retention",
            Box::new(move |doc| {
                doc.get_map("deletions")
                    .insert(
                        &deletion.to_storage(),
                        record(&[
                            ("at_ms", serde_json::json!(1)),
                            ("retention_ms", serde_json::json!(-1)),
                            ("batch_id", serde_json::Value::Null),
                        ]),
                    )
                    .unwrap();
            }),
            CatalogUnitRefusal::NegativeMilliseconds {
                path: format!("deletions.{deletion}.retention_ms"),
            },
        ),
        (
            "negative time",
            Box::new(move |doc| {
                doc.get_map("purged")
                    .insert(
                        &deletion.to_storage(),
                        record(&[("at_ms", serde_json::json!(-5))]),
                    )
                    .unwrap();
            }),
            CatalogUnitRefusal::NegativeMilliseconds {
                path: format!("purged.{deletion}.at_ms"),
            },
        ),
        (
            "batch id of the wrong type",
            Box::new(move |doc| {
                doc.get_map("deletions")
                    .insert(
                        &deletion.to_storage(),
                        record(&[
                            ("at_ms", serde_json::json!(1)),
                            ("retention_ms", serde_json::json!(1)),
                            ("batch_id", serde_json::json!(true)),
                        ]),
                    )
                    .unwrap();
            }),
            CatalogUnitRefusal::RecordShape {
                path: format!("deletions.{deletion}.batch_id"),
            },
        ),
        (
            "observed deletion is not a UUID",
            Box::new(move |doc| {
                doc.get_map("restores")
                    .insert(
                        &restore.to_storage(),
                        record(&[
                            ("at_ms", serde_json::json!(1)),
                            ("observed_deletions", serde_json::json!(["d1"])),
                            ("batch_id", serde_json::Value::Null),
                        ]),
                    )
                    .unwrap();
            }),
            CatalogUnitRefusal::MalformedUuid {
                path: format!("restores.{restore}.observed_deletions"),
            },
        ),
        (
            "restore observed nothing and has no batch",
            Box::new(move |doc| {
                doc.get_map("restores")
                    .insert(
                        &restore.to_storage(),
                        record(&[
                            ("at_ms", serde_json::json!(1)),
                            ("observed_deletions", serde_json::json!([])),
                            ("batch_id", serde_json::Value::Null),
                        ]),
                    )
                    .unwrap();
            }),
            CatalogUnitRefusal::RestoreWithoutObservations { restore },
        ),
        (
            "purge record with an extra field",
            Box::new(move |doc| {
                doc.get_map("purged")
                    .insert(
                        &deletion.to_storage(),
                        record(&[
                            ("at_ms", serde_json::json!(1)),
                            ("by", serde_json::json!("x")),
                        ]),
                    )
                    .unwrap();
            }),
            CatalogUnitRefusal::RecordShape {
                path: format!("purged.{deletion}"),
            },
        ),
    ];
    for (name, tamper, expected) in cases {
        assert_eq!(tampered(&unit, tamper).unwrap_err(), expected, "{name}");
    }
    // A Profile unit has no placement root at all.
    let profile_unit = ProfileCatalogUnit::create(profile, 1).unwrap();
    let doc = LoroDoc::new();
    doc.import(&profile_unit.snapshot().unwrap()).unwrap();
    doc.get_map("placement")
        .insert("current", LoroValue::Null)
        .unwrap();
    doc.commit();
    assert_eq!(
        ProfileCatalogUnit::from_snapshot(&doc.export(ExportMode::snapshot()).unwrap(), profile, 1)
            .unwrap_err(),
        CatalogUnitRefusal::UnknownRoot {
            root: "placement".into()
        }
    );
    // Validation is also what gates a write: a bad restore never lands.
    let before = unit.version();
    assert_eq!(
        unit.record_restore(
            restore,
            &RestoreRecord {
                at_ms: 1,
                observed_deletions: vec![],
                batch_id: None,
            },
        )
        .unwrap_err(),
        CatalogUnitRefusal::RestoreWithoutObservations { restore }
    );
    assert_eq!(
        unit.record_deletion(
            deletion,
            &DeletionRecord {
                at_ms: -1,
                retention_ms: 0,
                batch_id: None,
            },
        )
        .unwrap_err(),
        CatalogUnitRefusal::NegativeMilliseconds {
            path: format!("deletions.{deletion}.at_ms"),
        }
    );
    assert_eq!(unit.version(), before);
}

#[test]
fn a_committed_live_workspace_unit_needs_a_placement_but_a_staged_one_is_pending() {
    let workspace = WorkspaceId::new();
    let unit = WorkspaceCatalogUnit::create(workspace, 1).unwrap();
    assert_eq!(unit.placement(), None, "pending on a fresh unit");
    assert!(unit.is_live());
    assert_eq!(
        unit.snapshot().unwrap_err(),
        CatalogUnitRefusal::PlacementRequired
    );
    let raw = unit.inner.snapshot().unwrap();
    assert_eq!(
        WorkspaceCatalogUnit::from_snapshot(&raw, workspace, 2).unwrap_err(),
        CatalogUnitRefusal::PlacementRequired,
        "bytes that slipped past the writer are still refused on the way in"
    );
    // Explicitly unassigned is a placement.
    let move_id = Uuid::new_v4();
    unit.set_placement(None, move_id).unwrap();
    assert_eq!(
        unit.placement(),
        Some(Placement {
            profile: None,
            move_id
        })
    );
    let reloaded =
        WorkspaceCatalogUnit::from_snapshot(&unit.snapshot().unwrap(), workspace, 2).unwrap();
    assert_eq!(reloaded.placement(), unit.placement());
    // A unit that is not live may be committed without one: a held tombstone.
    let trashed = WorkspaceCatalogUnit::create(WorkspaceId::new(), 1).unwrap();
    trashed
        .record_deletion(DeletionId::new(), &DeletionRecord::new(1, None))
        .unwrap();
    assert!(!trashed.is_live());
    assert!(trashed.snapshot().is_ok());
    // A Profile unit never has one.
    assert!(
        ProfileCatalogUnit::create(ProfileId::new(), 1)
            .unwrap()
            .snapshot()
            .is_ok()
    );
}

#[test]
fn lifecycle_is_reduced_from_observed_facts_not_flags() {
    let (unit, _, profile, _) = placed(1);
    let d1 = DeletionId::new();
    let d2 = DeletionId::new();
    let batch = Uuid::new_v4();
    assert_eq!(
        DeletionRecord::new(0, None).retention_ms,
        TRASH_RETENTION_MS
    );
    assert_eq!(TRASH_RETENTION_MS, 2_592_000_000);
    unit.record_deletion(d1, &DeletionRecord::new(10, None))
        .unwrap();
    unit.record_deletion(d2, &DeletionRecord::new(20, Some(batch)))
        .unwrap();
    assert!(unit.is_deleted());
    assert_eq!(unit.active_deletions().into_iter().collect::<Vec<_>>(), {
        let mut ids = vec![d1, d2];
        ids.sort();
        ids
    });
    // Facts are immutable: a second record under the same id is refused.
    assert_eq!(
        unit.record_deletion(d1, &DeletionRecord::new(99, None))
            .unwrap_err(),
        CatalogUnitRefusal::DuplicateLifecycleId { id: d1.as_uuid() }
    );
    assert_eq!(unit.deletions()[&d1].at_ms, 10);
    // A restore clears exactly what it observed.
    let r1 = RestoreId::new();
    unit.record_restore(
        r1,
        &RestoreRecord {
            at_ms: 30,
            observed_deletions: vec![d1],
            batch_id: None,
        },
    )
    .unwrap();
    assert_eq!(
        unit.active_deletions().into_iter().collect::<Vec<_>>(),
        vec![d2]
    );
    assert!(unit.is_deleted());
    // An empty observation list from a batch action clears nothing.
    unit.record_restore(
        RestoreId::new(),
        &RestoreRecord {
            at_ms: 40,
            observed_deletions: vec![],
            batch_id: Some(batch),
        },
    )
    .unwrap();
    assert_eq!(
        unit.active_deletions().into_iter().collect::<Vec<_>>(),
        vec![d2]
    );
    // Clearing the last active deletion makes it live again; the facts stay.
    unit.record_restore(
        RestoreId::new(),
        &RestoreRecord {
            at_ms: 50,
            observed_deletions: vec![d2],
            batch_id: Some(batch),
        },
    )
    .unwrap();
    assert!(!unit.is_deleted());
    assert!(unit.is_live());
    assert_eq!(unit.deletions().len(), 2);
    assert_eq!(unit.restores().len(), 3);
    // Batch ids are opaque: stored as given, compared to nothing. One that
    // happens to equal the Profile's UUID is still just a batch id.
    let d3 = DeletionId::new();
    unit.record_deletion(d3, &DeletionRecord::new(60, Some(profile.as_uuid())))
        .unwrap();
    assert_eq!(unit.deletions()[&d3].batch_id, Some(profile.as_uuid()));
    let reloaded =
        WorkspaceCatalogUnit::from_snapshot(&unit.snapshot().unwrap(), unit.workspace(), 2)
            .unwrap();
    assert_eq!(reloaded.deletions(), unit.deletions());
    assert_eq!(reloaded.restores(), unit.restores());
    assert_eq!(reloaded.active_deletions(), unit.active_deletions());
}

#[test]
fn an_unseen_concurrent_delete_stays_active_after_a_restore() {
    let (mut a, workspace, _, _) = placed(1);
    let d1 = DeletionId::new();
    a.record_deletion(d1, &DeletionRecord::new(1, None))
        .unwrap();
    let mut b = WorkspaceCatalogUnit::from_snapshot(&a.snapshot().unwrap(), workspace, 2).unwrap();
    // A restores what it has seen; B concurrently deletes again.
    a.record_restore(
        RestoreId::new(),
        &RestoreRecord {
            at_ms: 2,
            observed_deletions: vec![d1],
            batch_id: None,
        },
    )
    .unwrap();
    let d2 = DeletionId::new();
    b.record_deletion(d2, &DeletionRecord::new(3, None))
        .unwrap();
    let from_a = a.updates_since(&b.version()).unwrap();
    let from_b = b.updates_since(&a.version()).unwrap();
    assert!(!a.import_frames(&[from_b]).unwrap().is_pending());
    assert!(!b.import_frames(&[from_a]).unwrap().is_pending());
    for unit in [&a, &b] {
        assert_eq!(
            unit.active_deletions().into_iter().collect::<Vec<_>>(),
            vec![d2]
        );
        assert!(unit.is_deleted());
    }
}

#[test]
fn purge_evidence_is_retained_on_its_own_and_never_makes_a_unit_live() {
    let workspace = WorkspaceId::new();
    let deletion = DeletionId::new();
    // No placement, no content: only the deletion and its purge remain.
    let unit = WorkspaceCatalogUnit::create(workspace, 1).unwrap();
    unit.record_deletion(deletion, &DeletionRecord::new(1, None))
        .unwrap();
    unit.record_purge(deletion, &PurgeRecord { at_ms: 2 })
        .unwrap();
    assert_eq!(
        unit.record_purge(deletion, &PurgeRecord { at_ms: 3 })
            .unwrap_err(),
        CatalogUnitRefusal::DuplicateLifecycleId {
            id: deletion.as_uuid()
        }
    );
    let evidence =
        WorkspaceCatalogUnit::from_snapshot(&unit.snapshot().unwrap(), workspace, 2).unwrap();
    assert_eq!(evidence.purged()[&deletion], PurgeRecord { at_ms: 2 });
    assert!(evidence.is_deleted());
    assert!(!evidence.is_live());
    assert_eq!(evidence.placement(), None);
    // A late restore naming the purged deletion clears the deletion fact but
    // the purge evidence still refuses to call the unit live.
    evidence
        .record_restore(
            RestoreId::new(),
            &RestoreRecord {
                at_ms: 4,
                observed_deletions: vec![deletion],
                batch_id: None,
            },
        )
        .unwrap();
    assert!(!evidence.is_deleted());
    assert!(!evidence.is_live());
    assert!(evidence.snapshot().is_ok());
}

#[test]
fn concurrent_moves_converge_by_loro_map_order_without_an_application_clock() {
    // Equal Lamport: both moves directly succeed the same state, so Loro's
    // deterministic tie-break — the greater peer id — decides, on both sides.
    let (mut a, workspace, _, _) = placed(1);
    let mut b = WorkspaceCatalogUnit::from_snapshot(&a.snapshot().unwrap(), workspace, 2).unwrap();
    let (pa, ma) = (ProfileId::new(), Uuid::new_v4());
    let (pb, mb) = (ProfileId::new(), Uuid::new_v4());
    a.set_placement(Some(pa), ma).unwrap();
    b.set_placement(Some(pb), mb).unwrap();
    let from_a = a.updates_since(&b.version()).unwrap();
    let from_b = b.updates_since(&a.version()).unwrap();
    assert!(!a.import_frames(&[from_b]).unwrap().is_pending());
    assert!(!b.import_frames(&[from_a]).unwrap().is_pending());
    assert_eq!(a.placement(), b.placement());
    assert_eq!(
        a.placement(),
        Some(Placement {
            profile: Some(pb),
            move_id: mb
        }),
        "equal Lamport: the greater peer id wins"
    );
    assert_eq!(a.version(), b.version());

    // Unequal Lamport: the replica that moved twice has the later logical
    // clock and wins even with the smaller peer id.
    let (mut a, workspace, _, _) = placed(1);
    let mut b = WorkspaceCatalogUnit::from_snapshot(&a.snapshot().unwrap(), workspace, 2).unwrap();
    a.set_placement(Some(ProfileId::new()), Uuid::new_v4())
        .unwrap();
    let (pa2, ma2) = (ProfileId::new(), Uuid::new_v4());
    a.set_placement(Some(pa2), ma2).unwrap();
    b.set_placement(Some(ProfileId::new()), Uuid::new_v4())
        .unwrap();
    let from_a = a.updates_since(&b.version()).unwrap();
    let from_b = b.updates_since(&a.version()).unwrap();
    assert!(!a.import_frames(&[from_b]).unwrap().is_pending());
    assert!(!b.import_frames(&[from_a]).unwrap().is_pending());
    assert_eq!(a.placement(), b.placement());
    assert_eq!(
        a.placement(),
        Some(Placement {
            profile: Some(pa2),
            move_id: ma2
        }),
        "greater Lamport wins before any peer tie-break"
    );
    // Both outcomes survive a round trip through committed bytes.
    for unit in [&a, &b] {
        let reloaded =
            WorkspaceCatalogUnit::from_snapshot(&unit.snapshot().unwrap(), workspace, 7).unwrap();
        assert_eq!(reloaded.placement(), a.placement());
    }
}

#[test]
fn the_causal_successor_of_an_observed_move_wins() {
    let (mut a, workspace, _, _) = placed(9);
    let mut b = WorkspaceCatalogUnit::from_snapshot(&a.snapshot().unwrap(), workspace, 2).unwrap();
    a.set_placement(Some(ProfileId::new()), Uuid::new_v4())
        .unwrap();
    assert!(
        !b.import_frames(&[a.updates_since(&b.version()).unwrap()])
            .unwrap()
            .is_pending()
    );
    // B moves after seeing A's move: a causal successor, from the smaller peer.
    let (pb, mb) = (ProfileId::new(), Uuid::new_v4());
    b.set_placement(Some(pb), mb).unwrap();
    assert!(
        !a.import_frames(&[b.updates_since(&a.version()).unwrap()])
            .unwrap()
            .is_pending()
    );
    assert_eq!(
        a.placement(),
        Some(Placement {
            profile: Some(pb),
            move_id: mb
        })
    );
    assert_eq!(a.placement(), b.placement());
}

#[test]
fn a_staged_import_with_missing_ancestors_is_pending_and_applies_nothing() {
    let (mut a, workspace, _, _) = placed(1);
    let b = WorkspaceCatalogUnit::from_snapshot(&a.snapshot().unwrap(), workspace, 2).unwrap();
    b.set_placement(Some(ProfileId::new()), Uuid::new_v4())
        .unwrap();
    let middle = b.version();
    let (pb, mb) = (ProfileId::new(), Uuid::new_v4());
    b.set_placement(Some(pb), mb).unwrap();
    let before_placement = a.placement();
    let before_version = a.version();
    // Only the second move: its ancestor is not in A.
    let gap = b.updates_since(&middle).unwrap();
    assert!(
        a.staged_import(std::slice::from_ref(&gap))
            .unwrap()
            .is_pending()
    );
    assert!(a.import_frames(&[gap]).unwrap().is_pending());
    assert_eq!(a.placement(), before_placement);
    assert_eq!(a.version(), before_version);
    assert!(!a.version().includes_vv(&b.version()));
    // The full gap applies, and the fork becomes the unit only then.
    let complete = b.updates_since(&a.version()).unwrap();
    assert!(!a.import_frames(&[complete]).unwrap().is_pending());
    assert_eq!(
        a.placement(),
        Some(Placement {
            profile: Some(pb),
            move_id: mb
        })
    );
    assert!(a.version().includes_vv(&b.version()));
}

#[test]
fn a_staged_import_refuses_another_units_frames_and_undecodable_bytes() {
    let (mut a, _, _, _) = placed(1);
    let (other, _, _, _) = placed(2);
    let before = a.version();
    assert_eq!(
        a.import_frames(&[other.snapshot().unwrap()]).unwrap_err(),
        CatalogUnitRefusal::HeaderMismatch {
            key: "workspace_id"
        }
    );
    assert_eq!(
        a.import_frames(&[b"garbage".to_vec()]).unwrap_err(),
        CatalogUnitRefusal::UndecodableFrame
    );
    assert_eq!(a.version(), before);
}

#[test]
fn a_newer_format_opens_read_only_and_is_never_merged_into_a_writable_unit() {
    let (unit, workspace, _, _) = placed(1);
    let future = tampered(&unit, |doc| {
        doc.get_map("meta")
            .insert("schema_version", CATALOG_UNIT_FORMAT_VERSION + 1)
            .unwrap();
        doc.get_map("placement_history")
            .insert("field", true)
            .unwrap();
    })
    .unwrap();
    assert!(future.is_read_only());
    assert_eq!(
        future.compatibility(),
        Compatibility::TooNew {
            found: CATALOG_UNIT_FORMAT_VERSION + 1,
            understood: CATALOG_UNIT_FORMAT_VERSION
        }
    );
    let read_only = CatalogUnitRefusal::ReadOnly {
        found: CATALOG_UNIT_FORMAT_VERSION + 1,
        understood: CATALOG_UNIT_FORMAT_VERSION,
    };
    assert_eq!(
        future.set_placement(None, Uuid::new_v4()).unwrap_err(),
        read_only
    );
    assert_eq!(
        future
            .record_deletion(DeletionId::new(), &DeletionRecord::new(1, None))
            .unwrap_err(),
        read_only
    );
    assert_eq!(
        future.updates_since(&Default::default()).unwrap_err(),
        read_only
    );
    assert_eq!(future.staged_import(&[]).unwrap_err(), read_only);
    // Retained as it is, still read-only after a round trip.
    assert!(
        WorkspaceCatalogUnit::from_snapshot(&future.snapshot().unwrap(), workspace, 1)
            .unwrap()
            .is_read_only()
    );
    // A writable unit does not take the newer frame.
    let mut held = unit;
    assert_eq!(
        held.import_frames(&[future.snapshot().unwrap()])
            .unwrap_err(),
        CatalogUnitRefusal::IncomingTooNew {
            found: CATALOG_UNIT_FORMAT_VERSION + 1,
            understood: CATALOG_UNIT_FORMAT_VERSION
        }
    );
    assert!(!held.is_read_only());
}

#[test]
fn refusals_map_to_stable_error_codes() {
    use crate::errors::Code;
    let cases = [
        (CatalogUnitRefusal::UndecodableFrame, Code::CrdtRejected),
        (CatalogUnitRefusal::MissingAncestors, Code::CrdtRejected),
        (
            CatalogUnitRefusal::ReadOnly {
                found: 2,
                understood: 1,
            },
            Code::SchemaTooNew,
        ),
        (
            CatalogUnitRefusal::HeaderMismatch { key: "profile_id" },
            Code::ProfileUnitScopeMismatch,
        ),
        (
            CatalogUnitRefusal::UnknownRoot { root: "x".into() },
            Code::ProfileUnitScopeMismatch,
        ),
        (
            CatalogUnitRefusal::DuplicateLifecycleId { id: Uuid::nil() },
            Code::IdempotencyConflict,
        ),
        (CatalogUnitRefusal::PlacementRequired, Code::CorruptState),
        (
            CatalogUnitRefusal::MalformedUuid {
                path: "deletions.x".into(),
            },
            Code::CorruptState,
        ),
        (CatalogUnitRefusal::Crdt, Code::StorageFailed),
    ];
    for (refusal, code) in cases {
        let text = refusal.to_string();
        let error: crate::errors::Error = refusal.into();
        assert_eq!(error.code(), code, "{text}");
        assert!(error.to_string().contains(&text), "{text}");
    }
    assert_eq!(K_LOCAL_USER_SCOPE, "local_user_scope");
}

// ---- Q10 case 3: a placement write concurrent with a recorded operation

/// The operation a unit's last commit wrote, as the repair record stores it:
/// `(writer, version[writer] - 1)`, because a version-vector counter is an
/// exclusive end.
fn last_operation(unit: &WorkspaceCatalogUnit, writer: u64) -> loro::ID {
    loro::ID::new(writer, unit.version().get_last(writer).unwrap())
}

/// Q10's actual shape: the repairing device holds no unit, so its repair
/// starts a lineage of its own while the absent device has one of its own —
/// two disjoint histories, concurrent by definition. `(repairing unit, the
/// absent device's snapshot, its placement)`.
fn disjoint_lineages(
    repair_writer: u64,
    absent_writer: u64,
) -> (
    WorkspaceCatalogUnit,
    WorkspaceId,
    loro::ID,
    Vec<u8>,
    Placement,
) {
    let workspace = WorkspaceId::new();
    let repaired = WorkspaceCatalogUnit::create(workspace, repair_writer).unwrap();
    repaired
        .set_placement(Some(ProfileId::new()), Uuid::new_v4())
        .unwrap();
    let operation = last_operation(&repaired, repair_writer);
    let absent = WorkspaceCatalogUnit::create(workspace, absent_writer).unwrap();
    let rival = Placement {
        profile: Some(ProfileId::new()),
        move_id: Uuid::new_v4(),
    };
    absent.set_placement(rival.profile, rival.move_id).unwrap();
    (
        repaired,
        workspace,
        operation,
        absent.snapshot().unwrap(),
        rival,
    )
}

#[test]
fn a_concurrent_placement_write_is_detected_even_when_the_merged_value_does_not_move() {
    // The repairing device writes under the greater peer id, so on equal
    // Lamport its own hint wins the merge: the dangerous case, where gating
    // on "the placement changed" would stay silent.
    let (mut repaired, _, operation, absent, rival) = disjoint_lineages(9, 2);
    let hint = repaired.placement().unwrap();
    let before = repaired.version();

    assert!(!repaired.import_frames(&[absent]).unwrap().is_pending());

    assert_eq!(
        repaired.placement(),
        Some(hint),
        "the merged placement did not move; the rival lost"
    );
    let PlacementConcurrency::Concurrent(rivals) =
        repaired.placement_writes_concurrent_with(&before, operation, &[])
    else {
        panic!("a disjoint lineage's placement write is concurrent with the repair");
    };
    assert_eq!(rivals.len(), 1);
    assert_eq!(
        rivals[0].asserted,
        Some(rival),
        "the losing value is read at the rival's own version, not from the merge"
    );
    assert_ne!(rivals[0].asserted.unwrap().profile, hint.profile);
}

#[test]
fn the_losing_value_is_read_whichever_side_won_the_merge() {
    // The same import with the peers swapped: now the rival wins the merge
    // and the detection is unchanged, because it never consults the winner.
    let (mut repaired, _, operation, absent, rival) = disjoint_lineages(2, 9);
    let before = repaired.version();

    assert!(!repaired.import_frames(&[absent]).unwrap().is_pending());

    assert_eq!(repaired.placement(), Some(rival), "the rival won here");
    let PlacementConcurrency::Concurrent(rivals) =
        repaired.placement_writes_concurrent_with(&before, operation, &[])
    else {
        panic!("the relation does not depend on who won");
    };
    assert_eq!(rivals[0].asserted, Some(rival));
}

#[test]
fn a_placement_write_that_already_saw_the_recorded_operation_is_not_concurrent() {
    let (mut repaired, workspace, operation, _, _) = disjoint_lineages(9, 2);
    // The other device imports the repaired unit whole, then moves: a causal
    // successor, which is clear and stays automatic (rule D).
    let seen =
        WorkspaceCatalogUnit::from_snapshot(&repaired.snapshot().unwrap(), workspace, 2).unwrap();
    let from = seen.version();
    seen.set_placement(Some(ProfileId::new()), Uuid::new_v4())
        .unwrap();
    let before = repaired.version();

    assert!(
        !repaired
            .import_frames(&[seen.updates_since(&from).unwrap()])
            .unwrap()
            .is_pending()
    );

    assert_eq!(
        repaired.placement_writes_concurrent_with(&before, operation, &[]),
        PlacementConcurrency::Ordered { settled: true },
        "it saw the repair, and with no rival on file it has seen everything there is"
    );
}

#[test]
fn an_identity_this_history_never_had_is_unknown_and_never_a_conflict() {
    let (mut repaired, _, _, absent, _) = disjoint_lineages(9, 2);
    let before = repaired.version();
    assert!(!repaired.import_frames(&[absent]).unwrap().is_pending());

    // A device's writer id is re-minted without being persisted when its
    // stored row cannot be parsed, so a recorded identity can be orphaned.
    for orphan in [loro::ID::new(4_242, 0), loro::ID::new(9, 9_999)] {
        assert_eq!(
            repaired.placement_writes_concurrent_with(&before, orphan, &[]),
            PlacementConcurrency::Unknown,
            "an orphaned identity is a do-not-know, never a conflict"
        );
    }
}

#[test]
fn a_concurrent_write_that_does_not_touch_the_placement_is_not_a_placement_conflict() {
    // One base, so the two lineages share a history; the second placement
    // write stands in for the repair and the other device never sees it.
    let (mut a, workspace, _, _) = placed(9);
    let b = WorkspaceCatalogUnit::from_snapshot(&a.snapshot().unwrap(), workspace, 2).unwrap();
    let from = b.version();
    a.set_placement(Some(ProfileId::new()), Uuid::new_v4())
        .unwrap();
    let operation = last_operation(&a, 9);
    // Concurrent with that write, and a lifecycle fact rather than a move.
    b.record_deletion(DeletionId::new(), &DeletionRecord::new(10, None))
        .unwrap();
    let before = a.version();

    assert!(
        !a.import_frames(&[b.updates_since(&from).unwrap()])
            .unwrap()
            .is_pending()
    );

    assert!(a.is_deleted(), "the concurrent fact did arrive");
    assert_eq!(
        a.placement_writes_concurrent_with(&before, operation, &[]),
        PlacementConcurrency::Ordered { settled: false },
        "the gate is a concurrent write to the placement, not any concurrent write"
    );
}

#[test]
fn a_write_that_saw_both_sides_settles_what_the_two_disjoint_lineages_left_open() {
    let (mut repaired, workspace, operation, absent, _) = disjoint_lineages(9, 2);
    let before = repaired.version();
    assert!(!repaired.import_frames(&[absent]).unwrap().is_pending());
    let PlacementConcurrency::Concurrent(rivals) =
        repaired.placement_writes_concurrent_with(&before, operation, &[])
    else {
        panic!("the rival is concurrent");
    };
    let recorded: Vec<loro::ID> = rivals.iter().map(|rival| rival.operation).collect();

    // A third device holding the merged unit moves the Workspace: it saw the
    // repair and the rival, so its write is an answer rather than a question.
    let both =
        WorkspaceCatalogUnit::from_snapshot(&repaired.snapshot().unwrap(), workspace, 11).unwrap();
    let from = both.version();
    both.set_placement(Some(ProfileId::new()), Uuid::new_v4())
        .unwrap();
    let before = repaired.version();
    assert!(
        !repaired
            .import_frames(&[both.updates_since(&from).unwrap()])
            .unwrap()
            .is_pending()
    );

    assert_eq!(
        repaired.placement_writes_concurrent_with(&before, operation, &recorded),
        PlacementConcurrency::Ordered { settled: true }
    );
    assert_eq!(
        repaired.placement_writes_concurrent_with(&before, operation, &[loro::ID::new(2, 9_999)]),
        PlacementConcurrency::Ordered { settled: false },
        "a rival it cannot be shown to have seen leaves the question open"
    );
}
