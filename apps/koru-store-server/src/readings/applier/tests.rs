// SPDX-License-Identifier: MPL-2.0

use super::*;

/// Two encodings of one state are one revision; another state is not.
#[test]
fn a_revision_is_compared_decoded() {
    let doc = loro::LoroDoc::new();
    doc.set_peer_id(7).unwrap();
    doc.get_map("readings").insert("k", 1).unwrap();
    doc.commit();
    let one = doc.oplog_vv().encode();
    assert!(same_revision(&one, &one.clone()));
    doc.get_map("readings").insert("k", 2).unwrap();
    doc.commit();
    assert!(!same_revision(&one, &doc.oplog_vv().encode()));
    assert!(!same_revision(&one, b"not a vector"));
}
