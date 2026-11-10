// SPDX-License-Identifier: MPL-2.0
use super::*;
#[test]
fn live_ancestry_refuses_cycles_missing_parents_and_excess_depth() {
    let a = DocumentId::new();
    let b = DocumentId::new();
    assert_eq!(
        live_ancestor_chain(a, |id| Ok(Some((Some(if id == a { b } else { a }), false))))
            .unwrap_err()
            .code(),
        Code::CorruptState
    );
    assert_eq!(
        live_ancestor_chain(a, |id| Ok(if id == a {
            Some((Some(b), false))
        } else {
            None
        }))
        .unwrap_err()
        .code(),
        Code::CorruptState
    );
    let mut calls = 0;
    assert_eq!(
        live_ancestor_chain(a, |_| {
            calls += 1;
            Ok(Some((Some(DocumentId::new()), false)))
        })
        .unwrap_err()
        .code(),
        Code::VersionUnavailable
    );
    assert_eq!(calls, 256);
    assert!(!live_ancestor_chain(a, |id| Ok(Some((Some(b), id == b)))).unwrap());
    assert!(live_ancestor_chain(a, |_| Ok(Some((None, false)))).unwrap());
}
