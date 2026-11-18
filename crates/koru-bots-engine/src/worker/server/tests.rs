// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn authority_requires_the_whole_secret() {
    assert!(matches_authority("abcd", "abcd"));
    assert!(!matches_authority("abcd", "abce"));
    assert!(!matches_authority("abc", "abcd"));
}

#[test]
fn bootstrap_debug_contains_neither_root_nor_authority() {
    let bootstrap = Bootstrap {
        launch_id: Uuid::new_v4(),
        segment: "dev".into(),
        root: "private-root".into(),
        authority: "private-secret".into(),
    };
    let debug = format!("{bootstrap:?}");
    assert!(!debug.contains("private-root"));
    assert!(!debug.contains("private-secret"));
}
