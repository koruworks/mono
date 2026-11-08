// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn reordered_version_entries_preserve_proof_but_edits_do_not() {
    // Postcard map: two (peer, counter) entries, in opposite orders.
    let original = [2, 1, 1, 2, 1];
    let reordered = [2, 2, 1, 1, 1];
    let edited = [2, 2, 2, 1, 1];
    assert_ne!(original, reordered);
    assert!(same_revision(&original, &reordered));
    assert!(!same_revision(&original, &edited));
    assert!(!same_revision(&original, &[255]));
}
