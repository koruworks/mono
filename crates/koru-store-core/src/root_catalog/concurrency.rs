// SPDX-License-Identifier: MPL-2.0

//! **Was an arriving placement write concurrent with one this device
//! recorded?** (`docs/features/root-catalog/QUESTIONS.md` Q10, case 3 of the
//! decision, and its "Case 3 is feasible" section.)
//!
//! One read of Loro's own causality graph, kept here because it is the only
//! code outside this module's own writers that needs to know which container
//! the placement register lives in. The caller — the inbound choke point in
//! `service::root_conflict` — owns what the answer *means*; this file owns
//! how it is obtained, and it answers in three values rather than a boolean
//! so that "don't know" cannot be mistaken for "no" or for "yes".
//!
//! ```text
//! after.sub_iter(before)        every operation this import added
//!   per change segment
//!     get_changed_containers_in does that segment touch `placement`?
//!     cmp_frontiers(op, repair) Ok(None)         -> concurrent: a rival
//!                               Ok(Some(Greater))-> it saw the repair, and
//!                                                   if it also saw every
//!                                                   recorded rival it is an
//!                                                   answer: `settled`
//!                               Err(..)          -> Unknown
//!     fork_at(op)               the placement that operation's own history
//!                               reads: the value it asserted, win or lose
//! ```
//!
//! Three things about it are load-bearing:
//!
//! - **`Err(FrontiersNotIncluded)` means *don't know*, never a conflict.**
//!   The peer id a device writes under is re-minted without being persisted
//!   when its stored row cannot be parsed, so a recorded identity can be
//!   orphaned. An orphan must produce silence, not a question about an
//!   operation that is not there.
//! - **The gate is that a concurrent operation touched the placement at
//!   all**, not that the merged value changed. In the case this exists for —
//!   the bootstrap hint winning over a move the person actually made — the
//!   merged placement does *not* change, so gating on a change would stay
//!   silent in exactly the case worth asking about.
//! - **Nothing here reads the JSON encoding.** `export_json_updates`
//!   compresses peer ids into an index into its own table, and a per-change
//!   dependency array may not survive a re-encode; every API used here reads
//!   Loro's graph instead, which is what makes the identity comparable at
//!   all.

use std::cmp::Ordering;

use loro::{ContainerID, ContainerType, Frontiers, ID, LoroDoc, VersionVector};

use super::R_PLACEMENT;
use super::lifecycle::{Facts, Placement};

/// What the operations one import added say about the placement writes this
/// device recorded earlier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlacementConcurrency {
    /// The recorded operation is not in this document's history, so the
    /// relation cannot be computed. **Never a conflict**: the caller records
    /// nothing and the next arrival asks again.
    Unknown,
    /// Nothing the import added is both concurrent with the recorded
    /// operation and a write to the placement register. A write whose causal
    /// history already contains the other is clear and stays automatic
    /// (rule D).
    Ordered {
        /// An added placement write saw the recorded operation **and** every
        /// recorded rival: it was authored where both sides were visible, so
        /// it is already the answer the question would have asked for.
        settled: bool,
    },
    /// At least one operation is concurrent with the recorded one *and*
    /// touched the placement register, whether or not the merged value
    /// moved. A new rival outranks `settled`: it has not been answered.
    Concurrent(Vec<RivalPlacementWrite>),
}

/// One concurrent placement write, as the record keeper needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RivalPlacementWrite {
    /// The last operation of the change segment that touched the placement:
    /// an identity in this document's history, and the version
    /// [`Self::asserted`] was read at.
    pub operation: ID,
    /// The placement that operation's **own** history reads — the value the
    /// write asserted, which is the losing value whenever the merge went the
    /// other way. `None` when the register could not be read at that
    /// version, which leaves the caller with a concurrency it cannot name a
    /// Profile for.
    pub asserted: Option<Placement>,
}

/// The placement register's container: a named root map, the only place a
/// placement operation can land, because `validation` refuses any other root
/// and any key but `current` under this one.
fn placement_container() -> ContainerID {
    ContainerID::new_root(R_PLACEMENT, ContainerType::Map)
}

/// The placement `operation`'s own history reads, by forking the merged
/// document at it. The fork holds that operation and its causal past and
/// nothing concurrent with it, so on a disjoint lineage this is the arriving
/// device's value even when the merge kept the other one.
fn asserted_at(doc: &LoroDoc, operation: ID) -> Option<Placement> {
    let forked = doc.fork_at(&Frontiers::from_id(operation)).ok()?;
    Facts { doc: &forked }.placement()
}

/// Does `operation`'s own history contain `earlier`?
fn saw(doc: &LoroDoc, operation: ID, earlier: ID) -> bool {
    matches!(
        doc.cmp_frontiers(&Frontiers::from_id(operation), &Frontiers::from_id(earlier)),
        Ok(Some(Ordering::Greater)) | Ok(Some(Ordering::Equal))
    )
}

/// The placement writes between `before` and `after`, judged against
/// `repair` — the operation whose concurrency is the question — and
/// `recorded`, the rivals already on file, which only a write that saw all
/// of them can answer. See the module header; `after` is the merged
/// document's own version and `before` the version the import merged into.
pub(super) fn placement_writes_concurrent_with(
    doc: &LoroDoc,
    before: &VersionVector,
    after: &VersionVector,
    repair: ID,
    recorded: &[ID],
) -> PlacementConcurrency {
    let known = Frontiers::from_id(repair);
    // Asked once, before any comparison: an identity this history does not
    // contain can never be compared against, and the answer is silence.
    if doc.frontiers_to_vv(&known).is_none() {
        return PlacementConcurrency::Unknown;
    }
    let placement = placement_container();
    let mut rivals = Vec::new();
    let mut settled = false;
    for span in after.sub_iter(before) {
        let mut counter = span.counter.start;
        while counter < span.counter.end {
            let first = ID::new(span.peer, counter);
            // A change is the unit of causality: every operation in it has
            // the same dependencies, so the relation is computed once per
            // change segment rather than once per operation.
            let Some(change) = doc.get_change(first) else {
                return PlacementConcurrency::Unknown;
            };
            let end = (change.id.counter + change.len as i32).min(span.counter.end);
            debug_assert!(end > counter, "a change segment covers its own span");
            if !doc
                .get_changed_containers_in(first, (end - counter) as usize)
                .contains(&placement)
            {
                counter = end;
                continue;
            }
            let last = ID::new(span.peer, end - 1);
            match doc.cmp_frontiers(&Frontiers::from_id(first), &known) {
                // Orphaned mid-walk: still "don't know", and nothing found
                // before it is reported, because the walk is the evidence.
                Err(_) => return PlacementConcurrency::Unknown,
                Ok(None) => rivals.push(RivalPlacementWrite {
                    operation: last,
                    asserted: asserted_at(doc, last),
                }),
                Ok(Some(Ordering::Greater)) => {
                    settled |= recorded.iter().all(|rival| saw(doc, last, *rival));
                }
                // In the recorded operation's own past: it is what the
                // repair was written on top of, and says nothing.
                Ok(Some(Ordering::Less | Ordering::Equal)) => {}
            }
            counter = end;
        }
    }
    if rivals.is_empty() {
        PlacementConcurrency::Ordered { settled }
    } else {
        PlacementConcurrency::Concurrent(rivals)
    }
}
