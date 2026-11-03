// SPDX-License-Identifier: MPL-2.0

//! Which toggles this reader has closed, as a body sees it.
//!
//! **Not document content.** A toggle's open/closed state is the reader's
//! private, device-local preference ([`koru_core::toggle`]); the editor holds
//! the current answer and hands each change it makes to the host as an
//! explicit [`FoldIntent`], which the host saves through the Store. Nothing
//! here writes to the document, enters the undo stack or marks it dirty.
//!
//! A missing entry is open. The host replaces the whole set when the
//! reader's saved state arrives ([`Body::adopt_folds`](crate::body::Body::adopt_folds)),
//! so a slow read never closes a toggle the reader has since opened: the host
//! lays its own pending intents over what it read before handing it here.

use std::collections::HashSet;

use koru_core::NodeId;

/// One fold the reader asked for: the value, never "invert".
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FoldIntent {
    pub node: NodeId,
    pub closed: bool,
}

#[derive(Default)]
pub struct Folds {
    closed: HashSet<NodeId>,
    intents: Vec<FoldIntent>,
}

impl Folds {
    pub fn is_closed(&self, id: NodeId) -> bool {
        self.closed.contains(&id)
    }

    /// Set one fold and owe the host the intent. Returns whether anything
    /// changed; asking for the value already shown owes nothing.
    pub(crate) fn set(&mut self, node: NodeId, closed: bool) -> bool {
        let changed = if closed {
            self.closed.insert(node)
        } else {
            self.closed.remove(&node)
        };
        if changed {
            self.intents.push(FoldIntent { node, closed });
        }
        changed
    }

    /// Replace the whole set with what the host knows. Owes nothing: this is
    /// the host's own answer coming back.
    pub(crate) fn adopt(&mut self, closed: HashSet<NodeId>) -> bool {
        if self.closed == closed {
            return false;
        }
        self.closed = closed;
        true
    }

    pub(crate) fn take_intents(&mut self) -> Vec<FoldIntent> {
        std::mem::take(&mut self.intents)
    }
}
