// SPDX-License-Identifier: MPL-2.0

//! What the index itself holds, and the wire vocabulary it re-exports.
//!
//! ## Ids, not names
//!
//! Nothing here carries a profile. **A profile is an authorization and query
//! scope over workspaces**, resolved through the store's catalog before a query
//! reaches this crate; an index that stored one would be storing a fact about
//! who is allowed to look rather than a fact about what is written. Scope
//! arrives as a set of workspace ids.
//!
//! ## Tokens, not labels
//!
//! [`Field`] is `heading-2`, not `"Heading 2"`. The old index stored the
//! English and ranked on it, which made every result label untranslatable and
//! froze the vocabulary of the surface into the schema. *Persist semantic
//! values, never translated labels* — the same rule onboarding follows for a
//! purpose and an avatar ([ADR-82](../../../docs/decisions/ADR-076-100.md#adr-82-i18n--internationalization-constrains-what-is-stored-not-how-it-is-shown)).
//! Whoever draws a result resolves the label.

use std::fmt;

// **The wire vocabulary lives in `koru-store-wire`**, and is re-exported here so
// nothing inside this crate says anything new. It moved because three crates
// need to name a hit — the daemon that builds one, the HTTP layer that
// serialises it, the blocking client that parses it — and only this one should
// link an FTS engine to do so.
pub use koru_store_wire::search::{
    AttrFilter, AttrOp, DEFAULT_PAGE_SIZE, Lifecycle, LifecycleFilter, MAX_PAGE_SIZE, MatchSpan,
    Navigation, ReadingCounts, RecentQuery, SearchFacet, SearchFacets, SearchHit, SearchPage,
    SearchQuery, SearchRequest, SearchResponse, SearchSort, field,
};

/// One independently explainable piece of matchable text.
///
/// A fragment carries **no breadcrumb, no workspace name and no document
/// title**. Those are assembled at query time from the mirrored listing, which
/// is what makes a rename one row rather than a rewrite of every descendant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fragment {
    /// `node:<document>:<node>:primary`, and never `node:<node>:…` — **a
    /// `NodeId` means nothing outside the document that minted it.**
    pub key: String,
    pub workspace: String,
    /// `None` for the one fragment that *is* a workspace.
    pub document: Option<String>,
    pub node: Option<String>,
    /// Document order, so a preview can read a window around a hit.
    pub seq: i64,
    /// `Kind::as_str()`, as the document stores it.
    pub kind: String,
    /// A [`field`] token.
    pub field: String,
    pub text: String,
}

/// One attribute of one node, **kept in the type it was declared with**.
///
/// A [`Fragment`] is text, and text answers *contains this word*. It cannot
/// answer *before this date* or *above this number*, and flattening a typed
/// value into it is exactly what `ProjectedAttr.kind` exists to prevent — "so
/// a consumer can ask for dates without parsing every string and hoping."
/// Search is that consumer, so it keeps the type.
///
/// **Every attribute produces one of these, system and custom alike**, because
/// every attribute has a declared type: the ones Koru ships are known, and a
/// workspace-defined one must declare its type before it can be used. Nothing
/// here is per-attribute code.
///
/// The attribute is still written as a fragment as well. The two answer
/// different questions — `high` typed into the box finds the block by text,
/// while a `koru.priority is high` filter uses this row — and neither replaces
/// the other.
#[derive(Clone, Debug, PartialEq)]
pub struct AttrRow {
    pub workspace: String,
    pub document: String,
    pub node: String,
    /// The attribute key: `koru.due`, `koru.priority`, or a workspace's own.
    pub role: String,
    /// `ProjectedAttr.kind`, carried through unchanged: `date`,
    /// `datetime_floating`, `datetime_zoned`, `number`, `bool`, `ref`, `url`,
    /// `text`.
    ///
    /// **A filter names the kinds it applies to and a row of any other kind
    /// does not match** — which is how the three temporal types stay three
    /// (ADR-189). There is no column here that a `date` and a
    /// `datetime_zoned` could be compared through, deliberately.
    pub kind: String,
    /// The canonical text: an ISO date, a select token, a reference id.
    pub value: String,
    /// The comparable number, for the two kinds that have one: `number`, and
    /// `datetime_zoned` as epoch milliseconds. `None` for every other kind.
    pub num: Option<f64>,
    /// The zone a `datetime_zoned` was meant in. Display, never comparison —
    /// the instant in [`num`](Self::num) is what orders.
    pub zone: Option<String>,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum FuzzyIndexState {
    #[default]
    Warming,
    Ready,
    Rebuilding,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FuzzyIndexStats {
    pub state: FuzzyIndexState,
    pub active_terms: u64,
    pub tree_nodes: u64,
    pub inactive_nodes: u64,
    pub last_build_ms: Option<u64>,
    pub last_built_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewBlock {
    pub node: Option<String>,
    pub kind: String,
    pub field: String,
    pub text: String,
    pub matched: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchPreview {
    pub title: String,
    pub blocks: Vec<PreviewBlock>,
}

/// What the index is doing, for `GET /v1/debug/search`.
///
/// **Counters, never content.** That route's rule is metadata only, and a
/// search index is the component most tempted to break it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexStats {
    pub schema_version: i64,
    /// Documents waiting to be indexed.
    pub pending: usize,
    pub indexed: u64,
    pub removed: u64,
    /// Listings consumed — a rename, a trash, a new document.
    pub listings: u64,
    pub failed: u64,
    /// Reconciles that found work the events had not delivered.
    pub caught_up: u64,
    pub documents: usize,
    pub fragments: usize,
    /// **How far the index trails the read model**, as of the last reconcile.
    ///
    /// The number that says search is stale, and nothing else reported it:
    /// `pending` is how much work is *queued*, which is zero both when the
    /// index is current and when it has not yet noticed it is behind.
    ///
    /// Free to publish — the reconcile already compares every cursor against
    /// the projection's revisions, so this is a value that was being computed
    /// and thrown away.
    pub behind: usize,
    /// **Documents the read model holds that no workspace listing names**, as
    /// of the last reconcile.
    ///
    /// These cannot be indexed at all: a fragment for a document the listing
    /// does not name would be a result nobody can open, so `index_one` requires
    /// a listing row. The count is here because the alternative is silence -
    /// they are not `behind` (nothing is going to catch up) and not `failed`
    /// (nothing went wrong here). Non-zero means that many documents exist in
    /// the store and cannot be found by searching, which is a fact about the
    /// *store*, not about the index.
    pub unlisted: usize,
    /// Milliseconds the last document took, read, build and write together.
    pub last_millis: u64,
    /// Every document since start-up, added up.
    pub total_millis: u64,
    /// The slowest single document. **An average hides the one worth looking
    /// at** — four hundred at two milliseconds and one at nine hundred is a
    /// healthy mean and an unhealthy store.
    pub max_millis: u64,
    /// The last document, split by stage: reading the read model, turning it
    /// into fragments, writing them. `last_millis` covered all three at once.
    pub read_millis: u64,
    pub build_millis: u64,
    pub write_millis: u64,
    pub last_error: Option<String>,
}

#[derive(Debug)]
pub struct SearchError(pub String);

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SearchError {}

impl From<rusqlite::Error> for SearchError {
    fn from(value: rusqlite::Error) -> Self {
        Self(value.to_string())
    }
}

impl From<std::io::Error> for SearchError {
    fn from(value: std::io::Error) -> Self {
        Self(value.to_string())
    }
}

impl From<koru_projection::ProjectionError> for SearchError {
    fn from(value: koru_projection::ProjectionError) -> Self {
        Self(value.0)
    }
}

pub type SearchResult<T> = Result<T, SearchError>;
