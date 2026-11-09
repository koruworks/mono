// SPDX-License-Identifier: MPL-2.0

//! Root-catalog Plan 4.1 (A2) between real Stores in memory, through the
//! daemon's adapter as a sync pass drives it
//! (`docs/features/root-catalog/VERIFICATION.md`): source-device absence,
//! stale replicas after a purge and after a restore, a privacy
//! enable/disable cycle with lifecycle facts pending in between, and the
//! protocol-5 refusal through the real frame handler. The fixture is the
//! parent module's: `Device`, `sync`, `exchange_root`, `Capture`.

use super::*;
use koru_replication::protocol::{PeerFrame, ROOT_CATALOG_REQUIRED, handle_frame};
use koru_store_core::TRASH_RETENTION_MS;
use koru_store_core::root_catalog::{Staged, WorkspaceCatalogUnit};

/// A moment past the Trash retention of a deletion written at `offset_ms`.
fn expired(offset_ms: i64) -> Timestamp {
    Timestamp::from_millis(AT + offset_ms + TRASH_RETENTION_MS + 1)
}

// ------------------------------------------------- source device absence

/// §6.4 "No original device is required if another authorized replica
/// holds the needed state": A and B converge, then C joins B while A is
/// offline and gets W's placement, the Trash state of another Workspace,
/// and W's content, all from B. A is never touched again.
#[test]
fn a_third_device_joins_the_survivor_with_the_origin_offline_and_gets_placement_lifecycle_and_content()
 {
    let a = Device::new("A");
    let b = Device::new("B");
    let c = Device::new("C");
    let p = a.profile("P", TransportMode::P2p);
    let w = a.workspace(p.id, "W", TransportMode::P2p);
    let gone = a.workspace(p.id, "Gone", TransportMode::P2p);
    let (document, _) = a
        .store()
        .create_document(w, "Plan", "page", None, None, at())
        .unwrap();
    a.type_into(w, document, 1, "the plan");
    a.store().delete_workspace(gone, later(5)).unwrap();
    let ab = all_data_peer(relationship());
    sync(&a, &b, &ab);
    b.adapter.provision_document(&ab, w, document).unwrap();
    let bytes = a.adapter.snapshot(&ab, w, document).unwrap();
    b.adapter
        .apply_remote_updates(&ab, w, document, vec![bytes])
        .unwrap();
    assert_eq!(b.placement(w), Some(p.id));
    assert!(b.deleted_at(gone).is_some());
    let a_rows = a.workspace_count();

    // A is offline from here. C joins B.
    let bc = all_data_peer(relationship());
    sync(&b, &c, &bc);
    assert_eq!(c.placement(w), Some(p.id), "placement from the survivor");
    assert_eq!(c.owner_row(w), p.id);
    assert!(
        c.deleted_at(gone).is_some(),
        "lifecycle from the survivor: the Trash state travels"
    );
    assert_eq!(
        c.workspace_count(),
        2,
        "exactly the two Workspaces, nothing beside them"
    );
    let receipt = c
        .store()
        .join_receipt(
            bc.relationship_id.as_uuid(),
            JoinUnit::WorkspaceCatalog { workspace: w },
        )
        .unwrap()
        .expect("a receipt for W's unit");
    assert_eq!(receipt.phase, JoinPhase::Reconciled);
    // Content from the survivor too.
    c.adapter.provision_document(&bc, w, document).unwrap();
    let before = c.adapter.version(&bc, w, document).unwrap();
    let bytes = b.adapter.snapshot(&bc, w, document).unwrap();
    c.adapter
        .apply_remote_updates(&bc, w, document, vec![bytes])
        .unwrap();
    assert_ne!(c.adapter.version(&bc, w, document).unwrap(), before);
    assert_eq!(
        decode(&c.adapter.root_unit_version(&bc, unit_of(w)).unwrap()),
        decode(&b.adapter.root_unit_version(&bc, unit_of(w)).unwrap()),
        "one lineage on the survivor and the newcomer"
    );
    assert_eq!(a.workspace_count(), a_rows, "the origin was not consulted");
}

/// With no surviving copy the state is reported unavailable, never
/// invented: a device that never held W answers "not found" for its unit
/// and its content, and a device that asks it learns nothing about W —
/// no placement, no row, no unassigned entry, no deletion.
#[test]
fn with_no_surviving_copy_the_state_is_unavailable_and_nothing_is_invented() {
    let d = Device::new("D");
    d.profile("Own", TransportMode::P2p);
    let e = Device::new("E");
    let w = WorkspaceId::new();
    let peer = all_data_peer(relationship());
    assert!(e.adapter.bind_root_scope(&peer).unwrap());

    assert!(
        d.adapter
            .root_catalog(&peer)
            .unwrap()
            .units
            .iter()
            .all(|unit| unit.unit != unit_of(w))
    );
    assert_eq!(
        d.adapter
            .root_unit_version(&peer, unit_of(w))
            .unwrap_err()
            .code,
        ROOT_UNIT_NOT_FOUND
    );
    assert_eq!(
        d.adapter
            .root_unit_updates_since(&peer, unit_of(w), &VersionVector::new())
            .unwrap_err()
            .code,
        ROOT_UNIT_NOT_FOUND,
        "nothing is derived from nothing"
    );
    assert!(
        d.adapter.inventory(&peer, w).is_err(),
        "no content to serve"
    );

    for (_, outcome) in exchange_root(&d, &e, &peer) {
        outcome.unwrap();
    }
    assert!(
        e.store().root_placement(w).unwrap().is_none(),
        "unavailable, not unassigned"
    );
    assert_eq!(e.workspace_count(), 0);
    assert!(e.store().unassigned_workspaces().unwrap().is_empty());
    assert!(e.store().profile_deletions(at()).unwrap().is_empty());
    assert!(
        e.store()
            .join_receipt(
                peer.relationship_id.as_uuid(),
                JoinUnit::WorkspaceCatalog { workspace: w }
            )
            .unwrap()
            .is_none(),
        "no receipt for a unit nobody framed"
    );
}

// ------------------------------------------------------- stale devices

/// §6.4 "enough causality to refuse resurrection by a stale replica": a
/// replica that last synced before W was trashed and purged comes back
/// with a move of W. The move lands on the evidence-holding unit and makes
/// nothing live: no row returns, nothing is listed, nothing is served.
/// The stale replica then learns the deletion and the evidence and its own
/// pass purges. And a replica that only ever saw a deletion cannot
/// re-delete a Workspace restored since: its frames add nothing.
#[test]
fn a_stale_replica_cannot_resurrect_a_purged_workspace_or_re_delete_a_restored_one() {
    let a = Device::new("A");
    let b = Device::new("B");
    let p1 = a.profile("P1", TransportMode::P2p);
    let p2 = a.profile("P2", TransportMode::P2p);
    let w = a.workspace(p1.id, "W", TransportMode::P2p);
    let ab = all_data_peer(relationship());
    sync(&a, &b, &ab);
    assert_eq!(b.placement(w), Some(p1.id));

    // A trashes and purges W while B is away.
    a.store().delete_workspace(w, later(10)).unwrap();
    let purged = a.store().purge_deleted(expired(10)).unwrap();
    assert_eq!(purged.purged, vec![w]);
    assert_eq!(a.workspace_count(), 0);

    // B, stale, moves W and comes back.
    b.store().move_workspace(w, p2.id).unwrap();
    assert!(a.adapter.bind_root_scope(&ab).unwrap());
    for (unit, outcome) in exchange_root(&b, &a, &ab) {
        outcome.unwrap_or_else(|error| panic!("{unit:?}: {}", error.code));
    }
    assert_eq!(a.workspace_count(), 0, "no row came back");
    assert!(
        a.store()
            .workspaces_including_deleted()
            .unwrap()
            .iter()
            .all(|row| row.id != w)
    );
    assert!(a.store().unassigned_workspaces().unwrap().is_empty());
    assert!(a.adapter.inventory(&ab, w).is_err(), "nothing to serve");
    assert!(
        !a.adapter
            .catalog(&ab)
            .unwrap()
            .iter()
            .any(|row| row.workspace_id == w)
    );
    // The stale replica learns the deletion and the evidence; its pass purges.
    for (unit, outcome) in exchange_root(&a, &b, &ab) {
        outcome.unwrap_or_else(|error| panic!("{unit:?}: {}", error.code));
    }
    assert!(
        b.deleted_at(w).is_some(),
        "the active deletion wins visibility"
    );
    let b_purge = b.store().purge_deleted(expired(10)).unwrap();
    assert_eq!(b_purge.purged, vec![w]);
    assert_eq!(b.workspace_count(), 0);
    assert_eq!(
        decode(&a.adapter.root_unit_version(&ab, unit_of(w)).unwrap()),
        decode(&b.adapter.root_unit_version(&ab, unit_of(w)).unwrap()),
        "both hold the same evidence"
    );

    // A replica that saw only the deletion cannot re-delete a restored V.
    let v = a.workspace(p1.id, "V", TransportMode::P2p);
    sync(&a, &b, &ab);
    a.store().delete_workspace(v, later(20)).unwrap();
    let c = Device::new("C");
    let ac = all_data_peer(relationship());
    sync(&a, &c, &ac);
    assert!(c.deleted_at(v).is_some(), "C holds the deletion");
    a.store().restore_workspace(v).unwrap();
    for (_, outcome) in exchange_root(&a, &b, &ab) {
        outcome.unwrap();
    }
    assert!(a.deleted_at(v).is_none());
    assert!(b.deleted_at(v).is_none());
    // C re-sends everything it has about V to A.
    assert!(a.adapter.bind_root_scope(&ac).unwrap());
    let frames = c
        .adapter
        .root_unit_updates_since(&ac, unit_of(v), &VersionVector::new())
        .unwrap();
    let outcome = a
        .adapter
        .apply_remote_root_updates(&ac, unit_of(v), vec![frames], None)
        .unwrap();
    assert!(matches!(outcome, RootJoin::Applied { .. }));
    assert!(
        a.deleted_at(v).is_none(),
        "an observed deletion is not re-deleted by a replica that re-sends it"
    );
    for (_, outcome) in exchange_root(&a, &c, &ac) {
        outcome.unwrap();
    }
    assert!(
        c.deleted_at(v).is_none(),
        "and the stale replica catches up"
    );
}

// ------------------------------------------------------- privacy cycle

/// LocalOnly → P2P → LocalOnly → P2P over a real peer with a trash pending
/// through the first disable and a restore through the second: each
/// enable folds the overlay once and the peer sees one deletion and then
/// one restore; the frontiers end equal; no frame, digest, inventory row
/// or log line of the whole cycle names the hidden Profile W was born
/// under, by UUID or by its sentinel name.
#[test]
fn a_privacy_cycle_over_a_peer_with_pending_lifecycle_facts_folds_once_and_leaks_nothing() {
    const HIDDEN: &str = "SENTINEL-HIDDEN-CYCLE";
    let a = Device::new("A");
    let b = Device::new("B");
    let hidden = a.profile(HIDDEN, TransportMode::P2p);
    let w = a.workspace(hidden.id, "W", TransportMode::P2p);
    let open = a.profile("Open", TransportMode::P2p);
    a.store().move_workspace(w, open.id).unwrap();
    a.arm_all_profile(hidden.id, TransportMode::LocalOnly);
    let peer = all_data_peer(relationship());
    sync(&a, &b, &peer);
    assert_eq!(b.placement(w), Some(open.id));
    assert_eq!(
        b.store().profiles().unwrap().len(),
        1,
        "the hidden Profile never arrived"
    );

    let capture = Arc::new(Capture(Mutex::new(String::new())));
    let mut framed = String::new();
    tracing::subscriber::with_default(Arc::clone(&capture), || {
        let mut frame_everything = |a: &Device| {
            let inventory = a.adapter.root_catalog(&peer).unwrap();
            for unit in &inventory.units {
                framed.push_str(&unit.unit.subject_id());
            }
            let digest = koru_replication::protocol::root_catalog_digest(
                &peer,
                &inventory.units,
                &inventory.personal,
            )
            .unwrap();
            framed.push_str(&format!("{digest:?}"));
            let bytes = a
                .adapter
                .root_unit_updates_since(&peer, unit_of(w), &VersionVector::new())
                .unwrap();
            framed.push_str(&history_text(&bytes));
            framed.push_str(&String::from_utf8_lossy(&bytes));
            for row in a.adapter.catalog(&peer).unwrap() {
                framed.push_str(&row.profile_id.to_storage());
            }
        };
        // Cycle 1: disable, trash, enable.
        a.arm_all_profile(open.id, TransportMode::LocalOnly);
        a.store().delete_workspace(w, later(10)).unwrap();
        assert!(
            !a.adapter
                .root_catalog(&peer)
                .unwrap()
                .units
                .iter()
                .any(|unit| unit.unit == unit_of(w)),
            "while Open is Local-only W is listed nowhere"
        );
        a.arm_all_profile(open.id, TransportMode::P2p);
        let _ = a.adapter.publish_pending_root_overlays().unwrap();
        assert_eq!(
            a.adapter.publish_pending_root_overlays().unwrap(),
            0,
            "folded once"
        );
        frame_everything(&a);
        for (unit, outcome) in exchange_root(&a, &b, &peer) {
            outcome.unwrap_or_else(|error| panic!("{unit:?}: {}", error.code));
        }
        assert!(b.deleted_at(w).is_some(), "the peer sees the trash");

        // Cycle 2: disable, restore, enable.
        a.arm_all_profile(open.id, TransportMode::LocalOnly);
        a.store().restore_workspace(w).unwrap();
        assert!(b.deleted_at(w).is_some(), "nothing moved while disabled");
        a.arm_all_profile(open.id, TransportMode::P2p);
        let _ = a.adapter.publish_pending_root_overlays().unwrap();
        assert_eq!(a.adapter.publish_pending_root_overlays().unwrap(), 0);
        frame_everything(&a);
        for (unit, outcome) in exchange_root(&a, &b, &peer) {
            outcome.unwrap_or_else(|error| panic!("{unit:?}: {}", error.code));
        }
        assert!(b.deleted_at(w).is_none(), "the peer sees the restore");
    });

    let logs = capture.0.lock().unwrap().clone();
    for needle in [hidden.id.to_storage(), HIDDEN.to_owned()] {
        assert!(
            !framed.contains(&needle),
            "framed bytes name the hidden Profile: {needle}"
        );
        assert!(
            !logs.contains(&needle),
            "a log line names the hidden Profile: {needle}"
        );
    }
    assert!(
        logs.contains("root_sync"),
        "the exchange did log its phases"
    );
    // One lineage, and the facts folded once each.
    let a_version = decode(&a.adapter.root_unit_version(&peer, unit_of(w)).unwrap());
    let b_version = decode(&b.adapter.root_unit_version(&peer, unit_of(w)).unwrap());
    assert_eq!(a_version, b_version);
    let bytes = a
        .adapter
        .root_unit_updates_since(&peer, unit_of(w), &VersionVector::new())
        .unwrap();
    let Staged::Applied(unit) = WorkspaceCatalogUnit::from_frames(&[bytes], w, 9).unwrap() else {
        panic!("a snapshot is never pending");
    };
    assert_eq!(unit.deletions().len(), 1);
    assert_eq!(unit.restores().len(), 1);
    assert!(unit.is_live());
    assert_eq!(unit.placement().unwrap().profile, Some(open.id));
}

// ------------------------------------------------------ old protocols

/// A protocol-5 peer — the unit addresses without the root exchange — is
/// refused by name on the data plane through the real frame handler over
/// a real Store, for the root request and the legacy catalog request
/// alike; the same relationship on protocol 6 is answered.
#[test]
fn a_protocol_five_peer_is_refused_by_name_on_the_real_adapter() {
    let a = Device::new("A");
    let p = a.profile("P", TransportMode::P2p);
    a.workspace(p.id, "W", TransportMode::P2p);
    let mut peer = all_data_peer(relationship());
    peer.peer_protocol = Some(5);
    assert_eq!(peer.protocol_refusal(), Some(ROOT_CATALOG_REQUIRED));
    for request in [PeerFrame::RootCatalogRequest, PeerFrame::CatalogRequest] {
        assert_eq!(
            handle_frame(&a.adapter, &peer, request),
            vec![PeerFrame::Error {
                code: ROOT_CATALOG_REQUIRED.into()
            }]
        );
    }
    peer.peer_protocol = Some(koru_sync_control::PEER_PROTOCOL_VERSION);
    let answer = handle_frame(&a.adapter, &peer, PeerFrame::RootCatalogRequest);
    assert_eq!(answer.len(), 1);
    assert!(
        !matches!(answer[0], PeerFrame::Error { .. }),
        "the same relationship on 6 is answered: {answer:?}"
    );
}
