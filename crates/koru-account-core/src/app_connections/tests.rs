// SPDX-License-Identifier: MPL-2.0

use super::*;

// The resolved-set rule — Profile grants following placement, Workspace
// grants pinned to a UUID, the later flag, lag and the cache key — is
// tested beside it in `grants/tests.rs`.

#[test]
fn an_entitlement_ends_at_its_boundary() {
    assert!(CloudEntitlement::Active { until_millis: None }.is_active_at(i64::MAX));
    let until = CloudEntitlement::Active {
        until_millis: Some(100),
    };
    assert!(until.is_active_at(99));
    assert!(!until.is_active_at(100));
    assert!(!CloudEntitlement::None.is_active_at(0));
}
