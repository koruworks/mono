// SPDX-License-Identifier: MPL-2.0

//! **An update AccessKit will accept, whatever the views handed over.**
//!
//! `accesskit_consumer` validates every update it is given, and a failed check
//! is a `panic!` on the loop thread rather than an error — the window, and any
//! unsaved state in it, goes with it. The check that has struck in the field is
//! *"TreeUpdate includes duplicate child"*: one update in which a node id is
//! named as a child twice, by two parents or twice by one. It is how a control
//! published twice arrives, because its parent's child list and the control's
//! own children both repeat. [`Projection::duplicate_ids`](super::Projection)
//! already counted the first half of that, and the update went out anyway.
//!
//! So [`repair`] runs on every update [`build`](super::projection::build)
//! produces, and does the cheapest thing that keeps the tree coherent: **the
//! first copy of a node and the first mention of a child win**, which is also
//! what a client already holding the tree has. The repair is not the fix for
//! whichever view published twice — that view is still wrong — but the
//! difference between a screen reader missing one control and the window
//! closing under the person using it. Each [`Repair`] carries the locators a
//! bug report needs to find that view; see `PaneAccessibility::publish`.
//!
//! ## What it must not break while it repairs
//!
//! Two more of the consumer's checks are one step away from this one, and a
//! repair that tripped either would trade one panic for another:
//!
//! - **Every node in an update must have a parent**, in the update or in the
//!   tree the client holds. Dropping a later copy or a later mention can leave
//!   a node nothing names any more, so a node the update *did* name before the
//!   repair and does not name after it is dropped too — and only those: in a
//!   delta, a node no parent in the update names is the ordinary case, because
//!   its parent did not change.
//! - **The focus must name a node that exists.** A focus on a node dropped
//!   above goes back to the window root, which always exists.
//!
//! A text run's `next_on_line` link naming a dropped run is not scrubbed. It
//! can only happen when two *different* controls hash to one id, which
//! `identity` makes vanishingly unlikely; a control published twice, the case
//! that has happened, publishes identical runs twice and drops none.

use std::collections::{HashMap, HashSet};
use std::fmt;

use accesskit::{NodeId, Role, TreeUpdate};

/// A node a repair is about, as a bug report needs to see it.
///
/// Ids, roles and locators only — never a label or a value, which can be
/// something a person wrote. The same rule `PaneAccessibility::note` keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named {
    pub id: NodeId,
    /// `None` when the node is not in this update — in a delta, the client
    /// holds it from an earlier one.
    pub role: Option<Role>,
    /// The node's `author_id`, which is its occurrence's locator. `None` for a
    /// node with none, which is every text run.
    pub locator: Option<String>,
}

impl fmt::Display for Named {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.id.0)?;
        match (&self.role, &self.locator) {
            (_, Some(locator)) => write!(f, " ({locator})"),
            (Some(role), None) => write!(f, " (a {role:?} with no locator)"),
            (None, None) => write!(f, " (not in this update)"),
        }
    }
}

/// One thing [`repair`] changed so the consumer would accept the update.
/// Every one of them is a bug in whoever built the semantics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Repair {
    /// The same node id was published twice. The first copy was kept.
    NodeTwice(Named),
    /// A child was named twice — by two parents, or twice by one. The first
    /// mention was kept, under `kept_under`.
    ChildTwice {
        child: Named,
        kept_under: Named,
        dropped_from: Named,
    },
    /// A node the repairs above left with no parent, dropped with them.
    Orphaned(Named),
    /// The focus named a node dropped above, and went back to the root.
    FocusReset(Named),
}

impl fmt::Display for Repair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NodeTwice(node) => {
                write!(
                    f,
                    "node {node} was published twice; the later copy was dropped"
                )
            }
            Self::ChildTwice {
                child,
                kept_under,
                dropped_from,
            } => write!(
                f,
                "child {child} was named twice; kept under {kept_under}, dropped from \
                 {dropped_from}"
            ),
            Self::Orphaned(node) => {
                write!(
                    f,
                    "node {node} lost its only parent to a repair and was dropped"
                )
            }
            Self::FocusReset(node) => write!(
                f,
                "focus named {node}, which a repair dropped; it is on the window root instead"
            ),
        }
    }
}

/// Make `update` acceptable to `accesskit_consumer`, and say what that took.
///
/// `root` is the window root, where a focus on a dropped node goes. Empty in
/// every healthy update, which costs one pass and two sets and copies nothing.
pub fn repair(update: &mut TreeUpdate, root: NodeId) -> Vec<Repair> {
    if is_clean(update) {
        return Vec::new();
    }
    let mut repairs = Vec::new();
    let names = Names::before(update);
    let named_before = named_children(update);

    // The first copy of every node wins.
    let mut kept = HashSet::new();
    update.nodes.retain(|(id, _)| {
        let first = kept.insert(*id);
        if !first {
            repairs.push(Repair::NodeTwice(names.get(*id)));
        }
        first
    });

    // The first mention of every child wins, in the order the consumer reads
    // them — which is the order of `nodes`.
    let mut parent_of: HashMap<NodeId, NodeId> = HashMap::new();
    for (parent, node) in &mut update.nodes {
        let before = node.children().to_vec();
        let after: Vec<NodeId> = before
            .iter()
            .copied()
            .filter(|child| match parent_of.get(child) {
                Some(first) => {
                    repairs.push(Repair::ChildTwice {
                        child: names.get(*child),
                        kept_under: names.get(*first),
                        dropped_from: names.get(*parent),
                    });
                    false
                }
                None => {
                    parent_of.insert(*child, *parent);
                    true
                }
            })
            .collect();
        if after.len() != before.len() {
            if after.is_empty() {
                node.clear_children();
            } else {
                node.set_children(after);
            }
        }
    }

    // What the repairs above orphaned, and what *that* orphaned in turn.
    let mut dropped = HashSet::new();
    loop {
        let named_now = named_children(update);
        let orphans: HashSet<NodeId> = update
            .nodes
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| *id != root && named_before.contains(id) && !named_now.contains(id))
            .collect();
        if orphans.is_empty() {
            break;
        }
        update.nodes.retain(|(id, _)| !orphans.contains(id));
        for id in orphans {
            repairs.push(Repair::Orphaned(names.get(id)));
            dropped.insert(id);
        }
    }
    if dropped.contains(&update.focus) {
        repairs.push(Repair::FocusReset(names.get(update.focus)));
        update.focus = root;
    }
    repairs
}

/// The consumer's own duplicate-child check, plus the same for nodes. The only
/// work a healthy update pays for.
fn is_clean(update: &TreeUpdate) -> bool {
    let mut nodes = HashSet::with_capacity(update.nodes.len());
    let mut children = HashSet::with_capacity(update.nodes.len());
    update.nodes.iter().all(|(id, node)| {
        nodes.insert(*id) && node.children().iter().all(|child| children.insert(*child))
    })
}

fn named_children(update: &TreeUpdate) -> HashSet<NodeId> {
    update
        .nodes
        .iter()
        .flat_map(|(_, node)| node.children().iter().copied())
        .collect()
}

/// Each node's role and locator as the update had it before anything was
/// dropped, so a repair can still name what it dropped.
struct Names(HashMap<NodeId, (Role, Option<String>)>);

impl Names {
    fn before(update: &TreeUpdate) -> Self {
        let mut names = HashMap::with_capacity(update.nodes.len());
        for (id, node) in &update.nodes {
            names
                .entry(*id)
                .or_insert_with(|| (node.role(), node.author_id().map(str::to_string)));
        }
        Self(names)
    }

    fn get(&self, id: NodeId) -> Named {
        let found = self.0.get(&id);
        Named {
            id,
            role: found.map(|(role, _)| *role),
            locator: found.and_then(|(_, locator)| locator.clone()),
        }
    }
}

#[cfg(test)]
mod tests;
