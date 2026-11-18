// SPDX-License-Identifier: MPL-2.0

//! Task lists, as the index is given them.
//!
//! A list is not a node of any document: it is a row of a registry in the
//! workspace's system document, which this crate cannot read — it links no
//! node model. So the host supplies the rows, as it supplies tag names
//! ([`TagNames`](crate::tags::TagNames)), and the index stores what it is
//! given.
//!
//! **The index has no repair rule of its own.** Each row's parent is the list
//! it is *drawn* under, after the host has skipped dead parents and broken
//! cycles. A path in a search result is therefore the path in the rail, and
//! there is one place — the host's tree — where "where does this list sit"
//! is decided.

use koru_store_core::ids::WorkspaceId;

/// One live task list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListRow {
    /// The list's id, in storage form.
    pub id: String,
    /// The live list it is drawn under, after repair; `None` is the top level.
    pub parent: Option<String>,
    /// Its position among its siblings, from zero.
    pub ordinal: usize,
    /// Its name, as typed. This is what is searched.
    pub name: String,
    /// Its swatch, as an accent token.
    pub color: Option<String>,
}

/// Where the indexer gets a workspace's lists.
///
/// `None` means *not now* — no system document yet, or one that could not be
/// read. **A read that fails is not an empty table**: the indexer writes and
/// caches nothing, the rows it already holds stay, and the next reconcile asks
/// again. `Some(vec![])` is a workspace that has no lists.
pub trait ListNames: Send {
    fn list_names(&self, workspace: WorkspaceId) -> Option<Vec<ListRow>>;
}
