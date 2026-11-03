// SPDX-License-Identifier: MPL-2.0

//! The per-document bookkeeping, without a thread or a Store.

use super::*;

fn node(n: u8) -> uuid::Uuid {
    uuid::Uuid::from_bytes([n; 16])
}

fn state(closed: &[(uuid::Uuid, bool)]) -> PrivateState {
    PrivateState {
        revision: 1,
        nodes: closed
            .iter()
            .map(|(node, value)| {
                (
                    node.to_string(),
                    std::collections::BTreeMap::from([(COLLAPSED.to_owned(), *value)]),
                )
            })
            .collect(),
    }
}

#[test]
fn only_true_values_read_as_closed() {
    let closed = closed_in(&state(&[(node(1), true), (node(2), false)]));
    assert_eq!(closed, HashSet::from([node(1)]));
}

#[test]
fn a_pending_change_wins_over_what_the_store_said() {
    let mut doc = DocFolds::default();
    doc.adopt(&state(&[(node(1), true)]));
    doc.pending.insert(node(1), (false, 7));
    doc.pending.insert(node(2), (true, 7));
    assert_eq!(doc.closed(), HashSet::from([node(2)]));
}

#[test]
fn an_older_acknowledgement_keeps_a_newer_click() {
    let mut doc = DocFolds::default();
    // Closed at seq 1, opened again at seq 2; the answer to 1 arrives.
    doc.pending.insert(node(1), (false, 2));
    doc.adopt(&state(&[(node(1), true)]));
    doc.pending.retain(|_, (_, pending)| *pending > 1);
    assert!(doc.closed().is_empty(), "the newer open still shows");
}
