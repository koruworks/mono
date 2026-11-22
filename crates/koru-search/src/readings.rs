// SPDX-License-Identifier: MPL-2.0

//! Resolving a blob hash to the words read out of its bytes.
//!
//! **A picture or a file is indexed by its name only**, because the node holds
//! nothing else: its content is `Blob { hash, … }` and its bytes are in the
//! blob store. The words read out of those bytes live in a *reading document*,
//! a synced child document of the file or the page (TECHNICAL D9), which the
//! projector never walks (D14). The Store daemon mirrors what those documents
//! say into a table beside `projection.db`, and supplies it here through
//! [`Readings`] — the [`TagNames`](crate::TagNames) shape, for the same reason:
//! text the indexer fetches from outside the node.
//!
//! What the indexer does with it is TECHNICAL §7 "The index": for each live
//! `image` or `file` node whose `blob` has a reading, one fragment per
//! non-empty page, `field` [`field::READ`](crate::field::READ), and a page
//! number on the hit only for a file of more than one page.
//!
//! ## Strings and primitives, and nothing from `koru-read`
//!
//! **This crate links no node model and no reading document.** A hash arrives
//! here as the `b3:…` string the projection already carries on
//! `ProjectedNode::blob`, and a page as a number and its words. Opening a
//! reading document means a `LoroDoc`, the store's lock and `koru-read`'s
//! shape — three things the daemon holds and this crate must keep no way of
//! naming. The daemon implements this; the index only asks.

use koru_store_core::ids::WorkspaceId;

/// One blob's winning reading in one workspace, flattened to what an index
/// wants.
///
/// "Winning" is decided on the daemon's side of the seam: the highest
/// generation, then the best status, across every reading document in the
/// workspace that holds this hash (TECHNICAL §7, the mirror's
/// `reading_pages`). What arrives here is one answer per hash.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct ReadingText {
    /// `b3:<hex>`, as `ProjectedNode::blob` spells it.
    pub hash: String,
    /// How many pages the file has, as far as its reading knows — the highest
    /// page reached. **The index numbers a hit's page only when this is above
    /// 1**, so a picture, a one-page PDF and a plain-text file carry no page.
    pub page_count: u32,
    /// `(page from 1, its words)`, ascending, each non-empty. Lines separated
    /// by `"\n"`.
    pub pages: Vec<(u32, String)>,
}

/// Where the indexer gets the words read out of a workspace's blobs.
///
/// **A seam rather than a dependency**, as [`TagNames`](crate::TagNames) is.
/// The daemon's readings mirror implements it.
///
/// `None` means *this could not be answered now*: the mirror is behind or
/// unavailable, or a host wired no source at all. It is never an error, and it
/// is **not** the same as "none of these hashes has a reading" — that is
/// `Some` with no entry for them. A hash with no reading is simply absent from
/// the answer.
pub trait Readings: Send {
    /// The winning reading of each of `hashes` in `workspace`, in any order,
    /// one per hash at most.
    fn readings(&self, workspace: WorkspaceId, hashes: &[String]) -> Option<Vec<ReadingText>>;
}

/// The source a host without a readings mirror has: phones, tests, and every
/// build until the daemon's mirror lands. It answers nothing, so the index
/// stays exactly what it was — a picture and a file found by their names.
#[derive(Copy, Clone, Default, Debug)]
pub struct NoReadings;

impl Readings for NoReadings {
    fn readings(&self, _workspace: WorkspaceId, _hashes: &[String]) -> Option<Vec<ReadingText>> {
        None
    }
}

#[cfg(test)]
mod tests;
