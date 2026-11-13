// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn history_checksum_vv_frontiers_and_pending_must_agree() {
    let doc = LoroDoc::new();
    doc.get_text("body").insert(0, "one").unwrap();
    doc.commit();
    let first = doc.oplog_vv();
    let first_frontiers = doc.oplog_frontiers();
    doc.get_text("body").insert(3, "two").unwrap();
    doc.commit();
    let snapshot = doc.export(ExportMode::snapshot()).unwrap();
    let vv = doc.oplog_vv().encode();
    let frontiers = doc.oplog_frontiers().encode();
    let sum = checksum(&snapshot);
    for (bytes, version, frontier, hash) in [
        (
            snapshot.clone(),
            vv.clone(),
            frontiers.clone(),
            "0".repeat(64),
        ),
        (
            snapshot.clone(),
            first.encode(),
            frontiers.clone(),
            sum.clone(),
        ),
        (
            snapshot.clone(),
            vv.clone(),
            first_frontiers.encode(),
            sum.clone(),
        ),
        (snapshot.clone(), vec![255], frontiers.clone(), sum.clone()),
        (snapshot.clone(), vv.clone(), vec![255], sum.clone()),
        (
            snapshot.clone(),
            vv.clone(),
            frontiers.clone(),
            sum.to_uppercase(),
        ),
    ] {
        assert_eq!(
            validate_history_evidence(&bytes, &version, &frontier, &hash)
                .err()
                .unwrap()
                .code(),
            Code::CorruptState
        );
    }
    let pending = doc.export(ExportMode::updates(&first)).unwrap();
    assert_eq!(
        validate_history_evidence(&pending, &vv, &frontiers, &checksum(&pending))
            .err()
            .unwrap()
            .code(),
        Code::CorruptState
    );
}

#[test]
fn history_proof_preserves_original_vv_and_updates() {
    let doc = LoroDoc::new();
    doc.get_text("body").insert(0, "exact history").unwrap();
    doc.commit();
    let snapshot = doc.export(ExportMode::snapshot()).unwrap();
    let before = snapshot.clone();
    let original = doc.export(ExportMode::all_updates()).unwrap();
    let checked = validate_history_evidence(
        &snapshot,
        &doc.oplog_vv().encode(),
        &doc.oplog_frontiers().encode(),
        &checksum(&snapshot),
    )
    .unwrap();
    assert_eq!(checked.oplog_vv(), doc.oplog_vv());
    assert_eq!(checked.oplog_frontiers(), doc.oplog_frontiers());
    assert_eq!(checked.get_text("body").to_string(), "exact history");
    assert_eq!(snapshot, before);
    assert_eq!(doc.export(ExportMode::all_updates()).unwrap(), original);
}
