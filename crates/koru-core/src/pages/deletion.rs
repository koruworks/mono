// SPDX-License-Identifier: MPL-2.0

//! The deletion protocol of one page-document generation, as a value.
//!
//! When a page document is deleted on one device and written on another, the
//! two devices have to agree on what happened without a server and without a
//! clock — and then show the same answer to the person who made both moves.
//! The system document carries that agreement as four small tables under the
//! page record (`PLAN.final.md` §6.2), and this module is their executable
//! statement: [`DeletionRecord`] is the in-memory shape of those tables, its
//! writes are the only writes the protocol makes, and [`DeletionRecord::resolve`]
//! is the one reading of them every surface shares — the Tasks page, the
//! library entry beside Trash, and the reconciler that removes or re-lists.
//!
//! Nothing here is in Loro, on disk, or in the store. P5 reads the persisted
//! keys into a record and asks it; the tests beside this module are the proof
//! that the protocol behaves under every merge order, which is what P0b owes.
//!
//! Four rules carry the whole design:
//!
//! - **Every write is a set insertion or a join.** Deletion events, Keep
//!   decisions and Recovers are grow-only sets; the writing a replica found
//!   outside an event's coverage is a monotone join. So every write commutes
//!   with every other and applying one twice changes nothing — the CRDT union
//!   in [`DeletionRecord::merge`] is the same arithmetic.
//! - **Coverage only grows.** An event's coverage is the join of its own
//!   `covers` and the `covers` of every Keep decision about it. A decision is
//!   a fresh id written once, never a value overwritten in place, because a
//!   write-once Keep could not close a conflict that reopened after it (plan
//!   Part 2, rev7 F3): a second Keep is a second decision, and a late first
//!   decision then lands inside what the second already covers.
//! - **Recover wins.** One Recover naming an event ends that event for good,
//!   whatever Keeps exist or arrive later (owner, rev6). Nothing is removed to
//!   express that — the Keeps stay in the record — so the merge stays a union.
//! - **A Recover names the events it saw and no others.** An event it never
//!   observed stays in force: two concurrent deletions need two recoveries,
//!   and deleting a recovered document again is a new event the old Recover
//!   does not name.

use std::collections::{BTreeMap, BTreeSet};

use loro::VersionVector;

pub use crate::ids::{DecisionId, EventId};

use super::{covers, join};

/// The key a replica writes under. Opaque here: the real key (a device id,
/// a store peer) is chosen in P5 when the persisted layout is written, and
/// nothing in the protocol depends on what it is beyond equality.
pub type Replica = u64;

/// What a deletion event records: the version of the document the deleting
/// device could see, which is what the deletion is understood to cover, and
/// when it happened (for display; nothing is decided on the clock).
#[derive(Clone, PartialEq, Debug)]
pub struct Deletion {
    pub covers: VersionVector,
    pub deleted_ms: i64,
}

/// One *Keep deleted* decision: who took it and what it covers — the join of
/// the uncovered writing the conflict showed at the press.
#[derive(Clone, PartialEq, Debug)]
pub struct Keep {
    pub replica: Replica,
    pub covers: VersionVector,
}

/// The resolved state of a generation, the one reading every surface shares.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Resolution {
    /// No event is in force: either nothing was ever deleted or every event
    /// has been recovered. Callers that must tell the two apart — to decide
    /// whether a replacement is seeded — ask [`DeletionRecord::is_deleted`]
    /// before the events were recovered, or look at the events themselves.
    Live,
    /// Some event is in force and its coverage contains everything anyone
    /// reported: a deletion, with nothing to ask.
    Deleted,
    /// Some event in force has writing outside its coverage. Shown on every
    /// device with *Keep deleted* and *Recover*; the events are listed so the
    /// decision can name the one it is about.
    Conflict(Vec<EventId>),
}

/// The deletion tables of one page-document generation.
///
/// Every field is a grow-only structure, which is what makes the record a
/// CRDT: two records of the same generation can always be merged, in any
/// order, any number of times, and arrive at the same value. A superseded
/// generation's record is a separate value and never moves the page.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct DeletionRecord {
    /// Grow-only; an event is never removed, which is the tombstone rule.
    deletions: BTreeMap<EventId, Deletion>,
    /// A Recover by `replica` that had observed `event`. Grow-only.
    recovers: BTreeSet<(Replica, EventId)>,
    /// The Keep decisions about each event. Grow-only; a decision is written
    /// once under its own id and never overwritten.
    keeps: BTreeMap<EventId, BTreeMap<DecisionId, Keep>>,
    /// The version of writing `replica` found outside `event`'s coverage —
    /// evidence, joined monotonically, never replaced by something smaller.
    uncovered: BTreeMap<(Replica, EventId), VersionVector>,
}

impl DeletionRecord {
    /// A generation that has never been deleted.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a deletion event. The first value under an id wins and a
    /// repeat is ignored; in practice a repeat is the same deletion arriving
    /// twice, so there is nothing to choose.
    pub fn delete(&mut self, event: EventId, covers: VersionVector, deleted_ms: i64) {
        self.deletions
            .entry(event)
            .or_insert(Deletion { covers, deleted_ms });
    }

    /// Records a Recover by `replica` of every event it had observed — one
    /// key per event and no others, so an event it never saw stays in force.
    pub fn recover(&mut self, replica: Replica, observed: impl IntoIterator<Item = EventId>) {
        self.recovers
            .extend(observed.into_iter().map(|event| (replica, event)));
    }

    /// Records a Keep decision under its own id. A decision id already present
    /// is left as it is: that is the same press re-sent until durable.
    pub fn keep(&mut self, event: EventId, decision: DecisionId, keep: Keep) {
        self.keeps
            .entry(event)
            .or_default()
            .entry(decision)
            .or_insert(keep);
    }

    /// Records the version of writing `replica` found outside `event`'s
    /// coverage, as the join of what it had reported before and `version`.
    /// A smaller or concurrent report therefore never replaces a larger one;
    /// evidence only grows. The event need not be known yet — a report that
    /// arrives before its event is held until the event does.
    pub fn report_uncovered(&mut self, replica: Replica, event: EventId, version: &VersionVector) {
        self.uncovered
            .entry((replica, event))
            .and_modify(|previous| *previous = join(previous, version))
            .or_insert_with(|| version.clone());
    }

    /// The CRDT union: every set unioned, every uncovered value joined. The
    /// same arithmetic as the writes above applied one by one, so merging is
    /// commutative, associative and idempotent — the tests prove it under
    /// every order.
    pub fn merge(&mut self, other: &DeletionRecord) {
        for (event, deletion) in &other.deletions {
            self.delete(*event, deletion.covers.clone(), deletion.deleted_ms);
        }
        self.recovers.extend(other.recovers.iter().copied());
        for (event, decisions) in &other.keeps {
            for (decision, keep) in decisions {
                self.keep(*event, *decision, keep.clone());
            }
        }
        for ((replica, event), version) in &other.uncovered {
            self.report_uncovered(*replica, *event, version);
        }
    }

    /// Every deletion event this record knows, in id order.
    pub fn events(&self) -> impl Iterator<Item = EventId> + '_ {
        self.deletions.keys().copied()
    }

    /// The deletion event itself, if known.
    pub fn deletion(&self, event: EventId) -> Option<&Deletion> {
        self.deletions.get(&event)
    }

    /// The Keep decisions about an event, in decision-id order.
    pub fn keeps(&self, event: EventId) -> impl Iterator<Item = (DecisionId, &Keep)> + '_ {
        self.keeps
            .get(&event)
            .into_iter()
            .flat_map(|decisions| decisions.iter().map(|(id, keep)| (*id, keep)))
    }

    /// What `replica` has reported as written outside `event`'s coverage.
    pub fn uncovered(&self, replica: Replica, event: EventId) -> Option<&VersionVector> {
        self.uncovered.get(&(replica, event))
    }

    /// The coverage of an event: the join of its own `covers` and the
    /// `covers` of every Keep decision about it. Because the decision set
    /// only grows and the join is monotone, this never shrinks, whatever
    /// order the decisions arrive in. `None` for an event this record has
    /// not seen — Keeps about an unknown event are held, not counted.
    pub fn coverage(&self, event: EventId) -> Option<VersionVector> {
        let deletion = self.deletions.get(&event)?;
        Some(
            self.keeps(event)
                .fold(deletion.covers.clone(), |acc, (_, keep)| {
                    join(&acc, &keep.covers)
                }),
        )
    }

    /// Whether some replica has recovered this event. Recover beats every
    /// Keep of the same event, so this is the only question that ends one.
    pub fn is_recovered(&self, event: EventId) -> bool {
        self.recovers
            .iter()
            .any(|(_, recovered)| *recovered == event)
    }

    /// The events not recovered, in id order.
    pub fn events_in_force(&self) -> Vec<EventId> {
        self.events()
            .filter(|event| !self.is_recovered(*event))
            .collect()
    }

    /// Whether some event is in force. True for a plain deletion and for a
    /// conflict alike: both mean the page has no live document of this
    /// generation and may seed a replacement.
    pub fn is_deleted(&self) -> bool {
        self.events().any(|event| !self.is_recovered(event))
    }

    /// The events in force that some replica has written outside — the ones
    /// a person is asked about. An event whose coverage contains every
    /// version anyone reported is not here: no conflict, no prompt.
    pub fn conflicts(&self) -> Vec<EventId> {
        self.events_in_force()
            .into_iter()
            .filter(|event| {
                let coverage = self
                    .coverage(*event)
                    .expect("an event in force is a known deletion");
                self.uncovered.iter().any(|((_, reported), version)| {
                    reported == event && !covers(&coverage, version)
                })
            })
            .collect()
    }

    /// The one reading of the record every surface shares.
    pub fn resolve(&self) -> Resolution {
        let conflicts = self.conflicts();
        if !conflicts.is_empty() {
            Resolution::Conflict(conflicts)
        } else if self.is_deleted() {
            Resolution::Deleted
        } else {
            Resolution::Live
        }
    }
}

#[cfg(test)]
mod tests;
