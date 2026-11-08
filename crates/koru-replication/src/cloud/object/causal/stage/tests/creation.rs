// SPDX-License-Identifier: MPL-2.0

//! What a creation looks like on the wire: the blob shapes the Cloud worker's
//! first-creation rule (Cloud MCP C23) tells apart.

use super::*;

fn seeded(peer: u64, body: &str) -> LoroDoc {
    let doc = LoroDoc::new();
    doc.set_peer_id(peer).unwrap();
    doc.get_text("body").insert(0, body).unwrap();
    doc.commit();
    doc
}

#[test]
fn a_creation_is_any_blob_rooted_at_the_empty_document() {
    let winner = seeded(7001, "the winner's seed");
    // The client's own creation: a snapshot of a unit Cloud does not hold,
    // and the same history as updates.
    for mode in [ExportMode::snapshot(), ExportMode::all_updates()] {
        assert!(starts_from_empty(&winner.export(mode).unwrap()).unwrap());
    }
    // A second attempt whose join answered the winner's version: its fresh
    // history is every change it sends, still rooted at the empty document.
    let loser = seeded(7002, "the loser's seed");
    let late = loser
        .export(ExportMode::updates(&winner.oplog_vv()))
        .unwrap();
    assert!(starts_from_empty(&late).unwrap());
    // The same, after the loser has also downloaded the winner: what it
    // sends is still only its own fresh history.
    loser
        .import(&winner.export(ExportMode::snapshot()).unwrap())
        .unwrap();
    let merged = loser
        .export(ExportMode::updates(&winner.oplog_vv()))
        .unwrap();
    assert!(starts_from_empty(&merged).unwrap());
}

#[test]
fn an_edit_and_a_shallow_snapshot_are_not_creations() {
    let cloud = seeded(7101, "in Cloud");
    let edited = LoroDoc::new();
    edited
        .import(&cloud.export(ExportMode::snapshot()).unwrap())
        .unwrap();
    // A new peer's first edit on a unit it loaded depends on Cloud's changes.
    edited.set_peer_id(7102).unwrap();
    edited.get_text("body").insert(0, "edited: ").unwrap();
    edited.commit();
    let edit = edited
        .export(ExportMode::updates(&cloud.oplog_vv()))
        .unwrap();
    assert!(!starts_from_empty(&edit).unwrap());
    // An existing peer's later edit, too.
    let before = edited.oplog_vv();
    edited.get_text("body").insert(0, "again: ").unwrap();
    edited.commit();
    let again = edited.export(ExportMode::updates(&before)).unwrap();
    assert!(!starts_from_empty(&again).unwrap());
    // A shallow snapshot starts at its shallow root, not at the empty
    // document.
    let shallow = edited
        .export(ExportMode::shallow_snapshot(&edited.state_frontiers()))
        .unwrap();
    assert!(!starts_from_empty(&shallow).unwrap());
    // Nothing in it creates nothing.
    let nothing = edited
        .export(ExportMode::updates(&edited.oplog_vv()))
        .unwrap();
    assert!(!starts_from_empty(&nothing).unwrap());
    // Not a Loro blob at all is an error, never a guess.
    assert_eq!(starts_from_empty(&[255; 32]), Err(Error::Loro));
}
