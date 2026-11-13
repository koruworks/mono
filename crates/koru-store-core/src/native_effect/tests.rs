// SPDX-License-Identifier: MPL-2.0
use super::*;
use loro::VersionVector;

#[test]
fn edit_revision_comparison_is_semantic_and_bounded() {
    // Real Loro/postcard map encodings with reversed entry order. Validate
    // their decoded values so these bytes cannot silently become a fake VV.
    let left = vec![2, 1, 2, 2, 4];
    let right = vec![2, 2, 4, 1, 2];
    let expected = VersionVector::from_iter([(1, 1), (2, 2)]);
    assert_ne!(left, right);
    assert_eq!(VersionVector::decode(&left).unwrap(), expected);
    assert_eq!(VersionVector::decode(&right).unwrap(), expected);
    assert!(same_native_edit_revision(&left, &right).unwrap());
    let changed = VersionVector::from_iter([(1, 3), (2, 2)]).encode();
    assert!(!same_native_edit_revision(&left, &changed).unwrap());
    for invalid in [Vec::new(), vec![0xff; 32], vec![0; 65_537]] {
        assert_eq!(
            same_native_edit_revision(&invalid, &left).unwrap_err().code,
            crate::Code::InvalidRequest
        );
        assert_eq!(
            same_native_edit_revision(&left, &invalid).unwrap_err().code,
            crate::Code::InvalidRequest
        );
    }
}
