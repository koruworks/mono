// SPDX-License-Identifier: MPL-2.0

//! The P0b protocol model: two or more replicas, each a [`DeletionRecord`],
//! exchanging merges in every order the plan names (§6.2, §7 P0b). Every
//! assertion is on resolved state and content — the [`Resolution`], what
//! coverage contains, what is recovered — never on a single scalar (§8).

use super::*;

/// Replica keys and, by the same numbers, the peer ids they write under.
const A: Replica = 1;
const B: Replica = 2;
const C: Replica = 3;

fn vv(entries: &[(u64, i32)]) -> VersionVector {
    let mut out = VersionVector::new();
    for &(peer, counter) in entries {
        out.insert(peer, counter);
    }
    out
}

fn keep(replica: Replica, covers: VersionVector) -> Keep {
    Keep { replica, covers }
}

fn merged(a: &DeletionRecord, b: &DeletionRecord) -> DeletionRecord {
    let mut out = a.clone();
    out.merge(b);
    out
}

/// Merges in both orders, asserts they agree, and hands back the result —
/// so every scenario below is checked in both merge orders by construction.
fn both_orders(a: &DeletionRecord, b: &DeletionRecord) -> DeletionRecord {
    let ab = merged(a, b);
    let ba = merged(b, a);
    assert_eq!(ab, ba, "merge order must not matter");
    assert_eq!(ab.resolve(), ba.resolve());
    ab
}

fn coverage_contains(record: &DeletionRecord, event: EventId, version: &VersionVector) -> bool {
    covers(&record.coverage(event).expect("known event"), version)
}

/// A record that nothing has happened to is live, and so says it was never
/// deleted.
#[test]
fn a_record_with_no_events_is_live() {
    let record = DeletionRecord::new();
    assert_eq!(record.resolve(), Resolution::Live);
    assert!(!record.is_deleted());
    assert!(record.events_in_force().is_empty());
}

/// §6.3: "No conflict, no prompt — an event whose coverage contains every
/// version anyone reported is simply a deletion."
#[test]
fn an_event_whose_coverage_contains_every_report_is_simply_a_deletion() {
    let e = EventId::new();
    let mut a = DeletionRecord::new();
    a.delete(e, vv(&[(A, 5), (B, 3)]), 1_000);

    // B's local copy is exactly what A saw when it deleted: inside coverage.
    let mut b = DeletionRecord::new();
    b.report_uncovered(B, e, &vv(&[(B, 3)]));
    // And an older copy on C is inside it too.
    b.report_uncovered(C, e, &vv(&[(A, 2)]));

    let record = both_orders(&a, &b);
    assert_eq!(record.resolve(), Resolution::Deleted);
    assert!(record.is_deleted());
    assert!(record.conflicts().is_empty());
    assert_eq!(record.events_in_force(), vec![e]);
}

/// §6.2: "A conflict on e: not recovered, and some uncovered.*.<e> not
/// inside e's coverage; shown on both devices."
#[test]
fn writing_outside_coverage_is_a_conflict_shown_to_both() {
    let e = EventId::new();
    let v1 = vv(&[(B, 6)]);

    let mut a = DeletionRecord::new();
    a.delete(e, vv(&[(A, 5), (B, 3)]), 1_000);
    let mut b = DeletionRecord::new();
    b.report_uncovered(B, e, &v1);

    // Alone, A sees a plain deletion and B has an event it has not seen yet.
    assert_eq!(a.resolve(), Resolution::Deleted);
    assert_eq!(
        b.resolve(),
        Resolution::Live,
        "a report about an unseen event waits"
    );
    assert_eq!(b.coverage(e), None);

    let on_a = merged(&a, &b);
    let on_b = merged(&b, &a);
    assert_eq!(on_a.resolve(), Resolution::Conflict(vec![e]));
    assert_eq!(on_b.resolve(), Resolution::Conflict(vec![e]));
    assert_eq!(on_a, on_b);
    assert!(!coverage_contains(&on_a, e, &v1));
    assert!(on_a.is_deleted(), "a conflict is still an event in force");
}

/// §6.3: "Keep deleted on event e: mint decision d, write keeps.<e>.<d>
/// covering the join of the uncovered values the conflict showed; the
/// conflict closes."
#[test]
fn a_keep_covering_the_writing_closes_the_conflict() {
    let e = EventId::new();
    let v1 = vv(&[(B, 6)]);

    let mut a = DeletionRecord::new();
    a.delete(e, vv(&[(A, 5), (B, 3)]), 1_000);
    let mut b = DeletionRecord::new();
    b.report_uncovered(B, e, &v1);
    let before = both_orders(&a, &b);
    assert_eq!(before.resolve(), Resolution::Conflict(vec![e]));

    // The press happens on A, from the conflict it was shown.
    let shown = before.uncovered(B, e).expect("the report").clone();
    a.keep(e, DecisionId::new(), keep(A, shown));

    let after = both_orders(&a, &b);
    assert_eq!(after.resolve(), Resolution::Deleted);
    assert!(coverage_contains(&after, e, &v1));
    assert!(after.conflicts().is_empty());
    assert!(after.is_deleted());
}

/// §6.2: "Keep after V1 writes d1 covering V1; writing V2 reopens the
/// conflict; Keep again writes d2 covering V2 and closes it; a late d1
/// changes nothing (V1 ⊆ V2)." The decision set is what makes the second
/// Keep possible: re-delivering d1 cannot close a conflict that reopened
/// after it (plan Part 2, rev7 F3).
#[test]
fn keep_then_write_then_keep_again_needs_a_second_decision() {
    let e = EventId::new();
    let v1 = vv(&[(B, 6)]);
    let v2 = vv(&[(B, 9)]);
    let d1 = DecisionId::new();
    let d2 = DecisionId::new();

    let mut a = DeletionRecord::new();
    a.delete(e, vv(&[(A, 5), (B, 3)]), 1_000);
    let mut b = DeletionRecord::new();
    b.report_uncovered(B, e, &v1);

    // Keep d1 covering V1 closes the first conflict.
    a.keep(e, d1, keep(A, v1.clone()));
    let closed = both_orders(&a, &b);
    assert_eq!(closed.resolve(), Resolution::Deleted);

    // B keeps writing; V2 is outside what d1 covered, so it reopens.
    b.report_uncovered(B, e, &v2);
    let reopened = both_orders(&a, &b);
    assert_eq!(reopened.resolve(), Resolution::Conflict(vec![e]));
    assert!(coverage_contains(&reopened, e, &v1));
    assert!(!coverage_contains(&reopened, e, &v2));

    // Delivering d1 again — the only Keep a write-once value could offer —
    // changes nothing: the record is already what it would be.
    let mut again = reopened.clone();
    again.keep(e, d1, keep(A, v1.clone()));
    again.merge(&a);
    assert_eq!(again, reopened);
    assert_eq!(again.resolve(), Resolution::Conflict(vec![e]));

    // A second decision covering V2 closes it.
    a.keep(e, d2, keep(A, v2.clone()));
    let closed_again = both_orders(&a, &b);
    assert_eq!(closed_again.resolve(), Resolution::Deleted);
    assert!(coverage_contains(&closed_again, e, &v2));

    // A late delivery of d1 after d2 — merged again, on either side — is a
    // no-op on state and on resolution.
    let mut late_on_b = merged(&b, &a);
    let mut only_d1 = DeletionRecord::new();
    only_d1.keep(e, d1, keep(A, v1.clone()));
    late_on_b.merge(&only_d1);
    assert_eq!(late_on_b, closed_again);
    assert_eq!(late_on_b.resolve(), Resolution::Deleted);
    assert_eq!(closed_again.keeps(e).count(), 2, "both decisions stay");
}

/// §7 P0b: "delayed first decision" — the first Keep arriving after the
/// second resolves the same, and a replica that never sees the first still
/// resolves the same, because d2 alone covers everything reported.
#[test]
fn the_first_decision_arriving_last_resolves_the_same() {
    let e = EventId::new();
    let v1 = vv(&[(B, 6)]);
    let v2 = vv(&[(B, 9)]);
    let d1 = DecisionId::new();
    let d2 = DecisionId::new();

    let mut base = DeletionRecord::new();
    base.delete(e, vv(&[(A, 5), (B, 3)]), 1_000);
    base.report_uncovered(B, e, &v2);

    let mut first_then_second = base.clone();
    first_then_second.keep(e, d1, keep(A, v1.clone()));
    first_then_second.keep(e, d2, keep(A, v2.clone()));

    let mut second_then_first = base.clone();
    second_then_first.keep(e, d2, keep(A, v2.clone()));
    assert_eq!(
        second_then_first.resolve(),
        Resolution::Deleted,
        "d2 alone closes it"
    );
    second_then_first.keep(e, d1, keep(A, v1.clone()));

    assert_eq!(first_then_second, second_then_first);
    assert_eq!(second_then_first.resolve(), Resolution::Deleted);
    assert_eq!(
        first_then_second.coverage(e),
        second_then_first.coverage(e),
        "coverage is the same join whichever decision came first"
    );
}

/// §4, rev6: "Recover wins over a concurrent Keep deleted about the same
/// deletion event." Nothing is removed to express it: the Keep stays.
#[test]
fn a_concurrent_recover_beats_a_keep_about_the_same_event() {
    let e = EventId::new();
    let v1 = vv(&[(B, 6)]);
    let d1 = DecisionId::new();

    let mut base = DeletionRecord::new();
    base.delete(e, vv(&[(A, 5), (B, 3)]), 1_000);
    base.report_uncovered(B, e, &v1);

    // Both see the conflict; A presses Keep, B presses Recover, offline.
    let mut a = base.clone();
    a.keep(e, d1, keep(A, v1.clone()));
    let mut b = base.clone();
    b.recover(B, [e]);
    assert_eq!(a.resolve(), Resolution::Deleted);
    assert_eq!(b.resolve(), Resolution::Live);

    let record = both_orders(&a, &b);
    assert_eq!(record.resolve(), Resolution::Live);
    assert!(record.is_recovered(e));
    assert!(!record.is_deleted());
    assert!(record.events_in_force().is_empty());
    assert_eq!(
        record.keeps(e).map(|(id, _)| id).collect::<Vec<_>>(),
        vec![d1],
        "the Keep is still in the record"
    );
    assert!(
        coverage_contains(&record, e, &v1),
        "and still counts toward coverage"
    );
}

/// §6.2: "Deleting a recovered document again writes a new event; the old
/// recovery does not name it, so it stays deleted. A delayed Keep or Recover
/// about the first event changes nothing about the second."
#[test]
fn deleting_a_recovered_document_again_is_a_new_event() {
    let e1 = EventId::new();
    let e2 = EventId::new();

    let mut a = DeletionRecord::new();
    a.delete(e1, vv(&[(A, 5)]), 1_000);
    let mut b = DeletionRecord::new();
    b.merge(&a);
    b.recover(B, [e1]);
    assert_eq!(both_orders(&a, &b).resolve(), Resolution::Live);

    // A deliberate second deletion, later, of the recovered document.
    a.merge(&b);
    a.delete(e2, vv(&[(A, 9), (B, 4)]), 2_000);
    let record = both_orders(&a, &b);
    assert_eq!(record.resolve(), Resolution::Deleted);
    assert_eq!(record.events_in_force(), vec![e2]);
    assert!(record.is_recovered(e1));
    assert!(!record.is_recovered(e2));

    // Late decisions about e1 — a Keep from C and a Recover from A — arrive.
    let mut late = DeletionRecord::new();
    late.keep(e1, DecisionId::new(), keep(C, vv(&[(C, 2)])));
    late.recover(A, [e1]);
    let after = both_orders(&record, &late);
    assert_eq!(after.resolve(), Resolution::Deleted);
    assert_eq!(after.events_in_force(), vec![e2]);

    // Only a Recover that observed e2 ends it.
    let mut recover_e2 = DeletionRecord::new();
    recover_e2.recover(B, [e1, e2]);
    assert_eq!(both_orders(&after, &recover_e2).resolve(), Resolution::Live);
}

/// §6.2: "A Recover writes one key per event it has observed and no others,
/// so an event it never saw stays in force: two concurrent deletions need
/// two recoveries, as in the manifest."
#[test]
fn two_concurrent_deletions_need_two_recoveries() {
    let e1 = EventId::new();
    let e2 = EventId::new();

    let mut a = DeletionRecord::new();
    a.delete(e1, vv(&[(A, 5)]), 1_000);
    let mut b = DeletionRecord::new();
    b.delete(e2, vv(&[(B, 7)]), 1_001);
    let record = both_orders(&a, &b);
    assert_eq!(record.resolve(), Resolution::Deleted);
    assert_eq!(record.events_in_force().len(), 2);

    // C had only seen A's deletion when it pressed Recover.
    let mut c = a.clone();
    c.recover(C, c.events().collect::<Vec<_>>());
    let one_recovered = both_orders(&record, &c);
    assert_eq!(one_recovered.resolve(), Resolution::Deleted);
    assert_eq!(one_recovered.events_in_force(), vec![e2]);
    assert!(one_recovered.is_recovered(e1));

    // A Recover taken with both events in view ends both.
    let mut both = one_recovered.clone();
    both.recover(C, both.events().collect::<Vec<_>>());
    assert_eq!(both.resolve(), Resolution::Live);
    assert!(!both.is_deleted());
}

/// §6.2: "Because the set only grows and the join is monotone, coverage never
/// shrinks, whatever order decisions arrive in."
#[test]
fn coverage_never_shrinks() {
    let e = EventId::new();
    let mut record = DeletionRecord::new();
    record.delete(e, vv(&[(A, 5)]), 1_000);

    type Step = Box<dyn Fn(&mut DeletionRecord)>;
    let steps: Vec<Step> = vec![
        // ahead on another peer
        Box::new(move |r| r.keep(e, DecisionId::new(), keep(B, vv(&[(B, 3)])))),
        // contained in what is already covered
        Box::new(move |r| r.keep(e, DecisionId::new(), keep(A, vv(&[(A, 2)])))),
        // concurrent: behind on A, ahead on C
        Box::new(move |r| r.keep(e, DecisionId::new(), keep(C, vv(&[(A, 1), (C, 4)])))),
        // equal to the current coverage
        Box::new(move |r| r.keep(e, DecisionId::new(), keep(A, vv(&[(A, 5), (B, 3), (C, 4)])))),
        // a merge carrying a decision from elsewhere
        Box::new(move |r| {
            let mut other = DeletionRecord::new();
            other.keep(e, DecisionId::new(), keep(B, vv(&[(B, 8)])));
            r.merge(&other);
        }),
        // a Recover and an uncovered report do not touch coverage
        Box::new(move |r| r.recover(B, [e])),
        Box::new(move |r| r.report_uncovered(C, e, &vv(&[(C, 9)]))),
    ];

    let mut previous = record.coverage(e).expect("known");
    for (index, step) in steps.iter().enumerate() {
        step(&mut record);
        let now = record.coverage(e).expect("known");
        assert!(
            covers(&now, &previous),
            "step {index}: coverage {now:?} does not contain the previous {previous:?}"
        );
        previous = now;
    }
    assert_eq!(previous, vv(&[(A, 5), (B, 8), (C, 4)]));
}

/// §6.2: "Evidence only grows. A replica never writes a smaller or
/// incomparable value over a larger one; P0 tests the join over real Loro
/// vectors (contained, equal, concurrent, ahead)."
#[test]
fn an_uncovered_report_only_grows() {
    let e = EventId::new();
    let mut record = DeletionRecord::new();

    let first = vv(&[(B, 5), (C, 2)]);
    record.report_uncovered(B, e, &first);
    assert_eq!(record.uncovered(B, e), Some(&first));

    let cases: [(&str, VersionVector); 4] = [
        ("contained", vv(&[(B, 3)])),
        ("equal", first.clone()),
        ("concurrent", vv(&[(B, 1), (C, 7)])),
        ("ahead", vv(&[(B, 9), (C, 7), (A, 1)])),
    ];
    let mut expected = first.clone();
    for (name, version) in &cases {
        let before = record.uncovered(B, e).expect("present").clone();
        record.report_uncovered(B, e, version);
        expected = join(&expected, version);
        let now = record.uncovered(B, e).expect("present");
        assert_eq!(now, &expected, "{name}: the stored value is the join");
        assert!(covers(now, &before), "{name}: never smaller than before");
        assert!(covers(now, version), "{name}: never loses the report");
    }
    assert_eq!(expected, vv(&[(A, 1), (B, 9), (C, 7)]));

    // Another replica's report is its own key and does not mix in.
    record.report_uncovered(C, e, &vv(&[(C, 1)]));
    assert_eq!(record.uncovered(C, e), Some(&vv(&[(C, 1)])));
    assert_eq!(record.uncovered(B, e), Some(&expected));
}

/// One write of the protocol, so a scenario can be replayed in any order and
/// split across replicas.
#[derive(Clone)]
enum Op {
    Delete(EventId, VersionVector, i64),
    Uncovered(Replica, EventId, VersionVector),
    Keep(EventId, DecisionId, Keep),
    Recover(Replica, Vec<EventId>),
}

fn apply(record: &mut DeletionRecord, op: &Op) {
    match op {
        Op::Delete(event, covers, ms) => record.delete(*event, covers.clone(), *ms),
        Op::Uncovered(replica, event, version) => {
            record.report_uncovered(*replica, *event, version)
        }
        Op::Keep(event, decision, keep) => record.keep(*event, *decision, keep.clone()),
        Op::Recover(replica, observed) => record.recover(*replica, observed.iter().copied()),
    }
}

fn replay(ops: &[&Op]) -> DeletionRecord {
    let mut record = DeletionRecord::new();
    for op in ops {
        apply(&mut record, op);
    }
    record
}

/// Calls `visit` with every permutation of `0..n`. Heap's algorithm, written
/// out rather than pulled in: seven ops is 5040 orders.
fn for_each_permutation(n: usize, mut visit: impl FnMut(&[usize])) {
    fn go(k: usize, items: &mut [usize], visit: &mut impl FnMut(&[usize])) {
        if k <= 1 {
            visit(items);
            return;
        }
        go(k - 1, items, visit);
        for i in 0..k - 1 {
            if k.is_multiple_of(2) {
                items.swap(i, k - 1);
            } else {
                items.swap(0, k - 1);
            }
            go(k - 1, items, visit);
        }
    }
    let mut items: Vec<usize> = (0..n).collect();
    go(n, &mut items, &mut visit);
}

/// A scenario touching every table: a deletion, writing outside it, two
/// Keeps, writing inside the second, a Recover, and a second deletion the
/// Recover does not name. Its reference outcome is Deleted with only the
/// second event in force.
fn scenario() -> (Vec<Op>, EventId, EventId) {
    let e1 = EventId::new();
    let e2 = EventId::new();
    let ops = vec![
        Op::Delete(e1, vv(&[(A, 5)]), 1_000),
        Op::Uncovered(B, e1, vv(&[(B, 4)])),
        Op::Keep(e1, DecisionId::new(), keep(A, vv(&[(B, 4)]))),
        Op::Uncovered(B, e1, vv(&[(B, 7)])),
        Op::Keep(e1, DecisionId::new(), keep(A, vv(&[(B, 7)]))),
        Op::Recover(B, vec![e1]),
        Op::Delete(e2, vv(&[(A, 9), (B, 7)]), 2_000),
    ];
    (ops, e1, e2)
}

/// §7 P0b: "resolution under every merge order." Every permutation of the
/// scenario's writes ends in the same record and the same resolution.
#[test]
fn every_order_of_the_same_writes_ends_in_the_same_record() {
    let (ops, e1, e2) = scenario();
    let reference = replay(&ops.iter().collect::<Vec<_>>());
    assert_eq!(reference.resolve(), Resolution::Deleted);
    assert_eq!(reference.events_in_force(), vec![e2]);
    assert!(reference.is_recovered(e1));
    assert!(coverage_contains(&reference, e1, &vv(&[(B, 7)])));

    let mut orders = 0;
    for_each_permutation(ops.len(), |order| {
        let ordered: Vec<&Op> = order.iter().map(|&i| &ops[i]).collect();
        let record = replay(&ordered);
        assert_eq!(record, reference, "order {order:?}");
        assert_eq!(record.resolve(), reference.resolve(), "order {order:?}");
        orders += 1;
    });
    assert_eq!(orders, 5040);
}

/// §7 P0b, the two-replica form: however the scenario's writes are split
/// between two replicas, merging them in either order gives the record a
/// single replica would have reached.
#[test]
fn any_split_of_the_writes_between_two_replicas_merges_to_the_same_record() {
    let (ops, _, _) = scenario();
    let reference = replay(&ops.iter().collect::<Vec<_>>());

    for mask in 0..(1u32 << ops.len()) {
        let goes_to_a = |i: &usize| mask & (1 << i) != 0;
        let to_a: Vec<&Op> = (0..ops.len()).filter(goes_to_a).map(|i| &ops[i]).collect();
        let to_b: Vec<&Op> = (0..ops.len())
            .filter(|i| !goes_to_a(i))
            .map(|i| &ops[i])
            .collect();
        let a = replay(&to_a);
        let b = replay(&to_b);
        let record = both_orders(&a, &b);
        assert_eq!(record, reference, "split {mask:#b}");
        assert_eq!(record.resolve(), reference.resolve(), "split {mask:#b}");
    }
}

/// Merge is the union: commutative, idempotent, and the empty record is its
/// identity. A duplicated deletion or decision keeps its first value.
#[test]
fn merge_is_idempotent_and_commutative() {
    let e = EventId::new();
    let d = DecisionId::new();

    let mut a = DeletionRecord::new();
    a.delete(e, vv(&[(A, 5)]), 1_000);
    a.keep(e, d, keep(A, vv(&[(B, 2)])));
    a.report_uncovered(B, e, &vv(&[(B, 2)]));

    let mut b = DeletionRecord::new();
    b.delete(e, vv(&[(A, 5)]), 1_000);
    b.keep(e, DecisionId::new(), keep(C, vv(&[(C, 3)])));
    b.report_uncovered(B, e, &vv(&[(B, 1), (C, 1)]));
    b.recover(C, [e]);

    let once = both_orders(&a, &b);
    let mut twice = once.clone();
    twice.merge(&b);
    twice.merge(&a);
    assert_eq!(twice, once, "merging again changes nothing");

    let mut with_self = a.clone();
    with_self.merge(&a);
    assert_eq!(with_self, a);

    let mut with_empty = a.clone();
    with_empty.merge(&DeletionRecord::new());
    assert_eq!(with_empty, a);

    // A repeat under an existing id is ignored; the first value stands.
    let mut repeat = a.clone();
    repeat.delete(e, vv(&[(A, 99)]), 9_999);
    repeat.keep(e, d, keep(C, vv(&[(C, 99)])));
    assert_eq!(repeat, a);
    assert_eq!(
        repeat.deletion(e).map(|deletion| deletion.deleted_ms),
        Some(1_000)
    );
}
