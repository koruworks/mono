// SPDX-License-Identifier: MPL-2.0

use super::*;

fn ids() -> (DocumentId, ProfileId) {
    (DocumentId::new(), ProfileId::new())
}

fn change(node: &str, value: Option<bool>) -> PrivateChange {
    PrivateChange {
        node_id: node.to_owned(),
        key: COLLAPSED.to_owned(),
        value,
    }
}

const NODE_A: &str = "0190f1d2-0000-7000-8000-00000000000a";
const NODE_B: &str = "0190f1d2-0000-7000-8000-00000000000b";

#[test]
fn absent_state_is_empty_and_open() {
    let (document, owner) = ids();
    let unit = DocumentPrivate::fresh(document, owner).unwrap();
    assert!(unit.state().nodes.is_empty());
}

#[test]
fn a_repeated_value_is_a_no_op_and_clearing_removes_the_node() {
    let (document, owner) = ids();
    let mut unit = DocumentPrivate::fresh(document, owner).unwrap();
    assert!(unit.apply(&[change(NODE_A, Some(true))]).unwrap());
    let revision = unit.revision();
    assert!(!unit.apply(&[change(NODE_A, Some(true))]).unwrap());
    assert_eq!(unit.revision(), revision, "a no-op commits nothing");
    assert!(unit.state().nodes[NODE_A][COLLAPSED]);
    assert!(unit.apply(&[change(NODE_A, None)]).unwrap());
    assert!(unit.state().nodes.is_empty());
    assert!(unit.revision() > revision);
}

#[test]
fn an_invalid_batch_applies_nothing() {
    let (document, owner) = ids();
    let mut unit = DocumentPrivate::fresh(document, owner).unwrap();
    let bad_key = PrivateChange {
        node_id: NODE_B.to_owned(),
        key: "koru.other".to_owned(),
        value: Some(true),
    };
    assert!(unit.apply(&[change(NODE_A, Some(true)), bad_key]).is_err());
    assert!(unit.apply(&[change("not-a-uuid", Some(true))]).is_err());
    assert!(unit.apply(&[]).is_err());
    let too_many: Vec<_> = (0..=MAX_BATCH)
        .map(|_| change(NODE_A, Some(true)))
        .collect();
    assert!(unit.apply(&too_many).is_err());
    assert!(unit.state().nodes.is_empty());
}

#[test]
fn a_snapshot_round_trips_only_under_its_own_identity() {
    let (document, owner) = ids();
    let mut unit = DocumentPrivate::fresh(document, owner).unwrap();
    unit.apply(&[change(NODE_A, Some(true)), change(NODE_B, Some(false))])
        .unwrap();
    let bytes = unit.snapshot().unwrap();
    let back = DocumentPrivate::load(&bytes, document, owner).unwrap();
    assert_eq!(back.state(), unit.state());
    assert!(DocumentPrivate::load(&bytes, DocumentId::new(), owner).is_err());
    assert!(DocumentPrivate::load(&bytes, document, ProfileId::new()).is_err());
    assert!(DocumentPrivate::load(b"garbage", document, owner).is_err());
}

#[test]
fn two_writers_from_one_stored_state_both_keep_their_change() {
    // Two windows each apply their own node against what the Store holds;
    // the Store serializes them, so the second loads the first's result.
    let (document, owner) = ids();
    let mut first = DocumentPrivate::fresh(document, owner).unwrap();
    first.apply(&[change(NODE_A, Some(true))]).unwrap();
    let stored = first.snapshot().unwrap();
    let mut second = DocumentPrivate::load(&stored, document, owner).unwrap();
    second.apply(&[change(NODE_B, Some(true))]).unwrap();
    assert_eq!(second.state().nodes.len(), 2);
}
