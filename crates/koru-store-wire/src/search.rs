// SPDX-License-Identifier: MPL-2.0

//! What a search asks for, and what comes back.
//!
//! Here rather than in `koru-search` because three crates need to name these
//! and only one of them should link an FTS engine: the daemon builds them, the
//! HTTP layer serialises them, the blocking client parses them, and at the
//! search cutover `apps/koru` renders them. `koru-search` re-exports the lot,
//! so nothing inside that crate says anything new.
//!
//! ## Ids, not names
//!
//! Nothing here carries a profile. **A profile is an authorization and query
//! scope over workspaces**, resolved through the store's catalog before a query
//! reaches the index; an index that stored one would be storing a fact about
//! who is allowed to look rather than a fact about what is written.
//!
//! ## Tokens, not labels
//!
//! [`field`] is `heading-2`, not `"Heading 2"`. The old index stored the English
//! and ranked on it, which made every result label untranslatable and froze the
//! vocabulary of the surface into the schema. *Persist semantic values, never
//! translated labels* — the same rule onboarding follows for a purpose and an
//! avatar ([ADR-82](../../../docs/decisions/ADR-076-100.md#adr-82-i18n--internationalization-constrains-what-is-stored-not-how-it-is-shown)). Whoever draws a
//! result resolves the label.
//!
//! ## Which ids are typed, and why not all of them
//!
//! **A request's workspace ids are [`WorkspaceId`]**, so one that will not parse
//! is refused with a code before it reaches the index. That is not tidiness: an
//! unparseable id used to be *dropped*, which emptied the scope, and because
//! "no workspaces named" had already been decided an empty scope means *no
//! results* rather than *everywhere*. The caller got a successful, empty page
//! and no hint that it had asked for something impossible.
//!
//! **A response's ids stay `String`.** They are the index's own columns, the
//! store minted them, and a client's next move is to hand them back. Typing
//! them would buy a parse of a value that cannot be wrong. `node` could not be
//! typed in any case — a `NodeId` belongs to `koru-core`, and naming it here
//! would drag the node model into the crate every client links.

use koru_store_core::ids::WorkspaceId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What part of a node a fragment is.
///
/// A token the surface translates, never a label. `attr:<role>` is the one
/// composed form — a labelled property carries the person's own word for it.
pub mod field {
    /// The document's own name, from the workspace listing.
    pub const TITLE: &str = "title";
    /// Ordinary prose.
    pub const BODY: &str = "body";
    pub const TASK: &str = "task";
    pub const QUOTE: &str = "quote";
    pub const CODE: &str = "code";
    pub const TABLE: &str = "table";
    pub const CONTACT: &str = "contact";
    /// An authored display name for content with no text of its own.
    pub const NAME: &str = "name";
    pub const LIST_NAME: &str = "list-name";
    /// A person's own name for a level of the hierarchy.
    pub const LEVEL_NAME: &str = "level-name";
    /// The workspace itself, so its name is matchable.
    pub const WORKSPACE: &str = "workspace";

    /// `heading-1` … `heading-6`.
    pub fn heading(level: u8) -> String {
        format!("heading-{}", level.clamp(1, 6))
    }

    /// `attr:<role>` — `attr:due`, `attr:owner`.
    pub fn attr(role: &str) -> String {
        format!("attr:{role}")
    }
}

/// Whether a hit's document is in the trash.
///
/// **Not a property of a fragment.** Trash is a flag on the manifest entry
/// ([ADR-95](../../../docs/decisions/ADR-076-100.md#adr-95-trash--a-document-in-the-trash-is-a-flag-on-its-manifest-entry)), so it is a property
/// of the *document*, resolved by joining the mirrored listing. Storing it on
/// every fragment is what used to let a hit and its own document title disagree
/// about whether they were binned.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    #[default]
    Live,
    Trashed,
}

/// Which of them a query wants.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleFilter {
    #[default]
    Live,
    Trash,
    All,
}

/// UTF-8 byte offsets in the string they explain.
///
/// **Bytes, not characters.** The text they index into is UTF-8, and a surface
/// slicing it needs the same units it will slice with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MatchSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchSort {
    #[default]
    Relevance,
    ObservedModified,
    LastUsedWorkspace,
}

/// One result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchHit {
    pub workspace: String,
    pub workspace_name: String,
    /// Absent when the hit is the workspace itself.
    pub document: Option<String>,
    pub node: Option<String>,
    /// Kind of the containing manifest entry. Unlike `kind`, this remains
    /// `file` when the matching fragment is its mirrored title.
    #[serde(default)]
    pub document_kind: String,
    /// MIME type of an opaque file, when the manifest recorded one.
    #[serde(default)]
    pub mime: Option<String>,
    pub kind: String,
    /// A [`field`] token.
    pub field: String,
    /// The document's name, from the listing.
    pub title: String,
    /// Workspace and the document's title path, assembled at query time —
    /// **never stored**, so renaming a document is one row and re-indexes
    /// nothing.
    pub breadcrumb: String,
    /// Offsets in [`Self::breadcrumb`] explaining path matches.
    pub breadcrumb_spans: Vec<MatchSpan>,
    pub snippet: String,
    pub spans: Vec<MatchSpan>,
    pub lifecycle: Lifecycle,
    pub trashed_at: Option<i64>,
    pub trashed_by: Option<String>,
    pub score: f64,
    /// When this store last observed **different** searchable content for the
    /// containing document. Derived local metadata, not an authored timestamp,
    /// and deliberately not *when we last looked*.
    pub observed_modified_at_ms: Option<i64>,
    /// Most recent navigation into this workspace on this machine.
    pub workspace_last_used_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SearchFacet {
    pub workspace: String,
    pub workspace_name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SearchFacets {
    pub workspaces: Vec<SearchFacet>,
}

/// A query that led somewhere, and when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RecentQuery {
    pub text: String,
    pub used_at_ms: i64,
}

/// How an [`AttrFilter`] compares.
///
/// Ranges are **half-open**, matching the windows the attribute service
/// already uses: `Before` is `< value`, `After` is `>= value`, and `Between`
/// is `[first, second)`. A person asking for "before the 10th" does not mean
/// the 10th, and one asking for "after the 10th" does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttrOp {
    /// Equal to the single value.
    Is,
    /// Equal to any of the values.
    OneOf,
    /// Strictly less than the single value.
    Before,
    /// Greater than or equal to the single value.
    After,
    /// `[values[0], values[1])`.
    Between,
    /// Carries this key at all, whatever the value.
    Exists,
    /// Does not carry this key.
    Missing,
}

/// One typed attribute filter.
///
/// **`kinds` is not optional and not a hint.** A stored value is compared only
/// against a filter that names its kind, which is what keeps the three
/// temporal types three: a `date` due and a `datetime_zoned` due are different
/// questions, and a window naming one does not answer the other
/// ([ADR-206](../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone)). Name every kind
/// the question covers — a due window usually means `["date",
/// "datetime_zoned"]` — or the answer silently omits the rest.
///
/// Comparison follows the kind: `number` and `datetime_zoned` compare as
/// numbers, everything else as its canonical text, which is ISO for the
/// remaining temporal kinds and therefore ordered correctly within one kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttrFilter {
    /// The attribute key: `koru.due`, `koru.priority`, or a workspace's own.
    pub key: String,
    pub op: AttrOp,
    /// The operand(s). Empty for `Exists`/`Missing`, one for `Is`/`Before`/
    /// `After`, two for `Between`, any number for `OneOf`.
    #[serde(default)]
    pub values: Vec<String>,
    /// The **same operands again, for the kinds that compare as numbers** —
    /// epoch milliseconds for a `datetime_zoned`, the plain number for a
    /// `number`. Positionally identical to [`Self::values`]: same count, same
    /// order, same meaning.
    ///
    /// **This exists because one filter names kinds that do not share a
    /// column.** A due window is `["date", "datetime_zoned"]`, and there is
    /// deliberately no column those two could be compared through
    /// ([ADR-217](../../../docs/decisions/ADR-201-225.md#adr-217-typedsearch--search-keeps-every-attribute-in-its-declared-type-and-a-filter-names-the-kinds-it-compares-against)): the date
    /// half needs `2026-09-10` and the instant half needs `1789...`. Without a
    /// second operand list the instant half would silently drop out — an ISO
    /// date does not parse as a number — and the filter would answer only for
    /// all-day deadlines while looking as though it answered for both.
    ///
    /// Empty means [`Self::values`] applies to every named kind, which is
    /// right whenever they all compare the same way (`/effort >3`).
    ///
    /// **The caller computes it, because the caller knows the zone.** Turning
    /// a calendar day into an instant is a zone question, and the store has no
    /// answer to it — the same rule `DueFilter` already states.
    #[serde(default)]
    pub numeric_values: Vec<String>,
    /// Which stored kinds this applies to. See the type docs — an empty list
    /// matches nothing, deliberately, rather than guessing.
    #[serde(default)]
    pub kinds: Vec<String>,
}

/// What to search for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SearchQuery {
    pub text: String,
    /// Restrict results to document/file titles and live task nodes for an @ picker.
    /// This is a query shape over the existing projection, not a stored index.
    #[serde(default)]
    pub entity_picker: bool,
    /// Empty means every workspace the index knows. **The caller resolves a
    /// profile to this**; the index has never heard of one.
    #[serde(default)]
    pub workspaces: Vec<WorkspaceId>,
    #[serde(default)]
    pub excluded_workspaces: Vec<WorkspaceId>,
    /// `Kind::as_str()` values, as documents store them.
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Tag **names**, not `TagId`s — this is a text-search surface and a
    /// person types the word they read. A hit's node must carry every one of
    /// them, and a name here matches its own descendants too (`fun` matches
    /// `fun/home`), so this is *node-scoped filtering* rather than a term:
    /// nothing here is searched for, ranked, or spelled back in
    /// [`SearchPage::corrected_query`]
    /// ([ADR-207](../../../docs/decisions/ADR-201-225.md#adr-207-tagfilter--a-tag-in-a-query-is-a-node-scoped-filter-not-a-term-and-it-lives-in-candidates-alone)).
    ///
    /// A surface may fill this directly; a person fills it by typing `#tag`
    /// into [`Self::text`], which the store lifts out before it searches.
    ///
    #[serde(default)]
    pub tags: Vec<String>,
    /// Typed attribute filters. A hit's node must satisfy **every** one.
    ///
    /// One general filter rather than a field per attribute, because a
    /// workspace defines its own attributes and declares their types: a wire
    /// naming `priority` and `due` explicitly would need a new field for every
    /// attribute anybody invents, while `key` + `kinds` serves a custom one on
    /// the day it exists.
    #[serde(default)]
    pub attributes: Vec<AttrFilter>,
    #[serde(default)]
    pub sort: SearchSort,
    #[serde(default)]
    pub lifecycle: LifecycleFilter,
    /// Clamped by the store; see [`MAX_PAGE_SIZE`].
    #[serde(default = "default_page_size")]
    pub page_size: usize,
    #[serde(default)]
    pub offset: usize,
    /// Copied to the response, so a surface typing quickly can drop an answer
    /// to a keystroke it has already moved past.
    #[serde(default)]
    pub request_seq: u64,
}

/// What a page holds when a caller says nothing.
pub const DEFAULT_PAGE_SIZE: usize = 50;

/// The most a caller may ask for in one page.
///
/// **Clamped rather than refused.** A number too large is a client being
/// hopeful, not a client being wrong, and a page is built in memory before it
/// is written — so the ceiling protects the store rather than scolding the
/// caller.
pub const MAX_PAGE_SIZE: usize = 200;

fn default_page_size() -> usize {
    DEFAULT_PAGE_SIZE
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            entity_picker: false,
            workspaces: Vec::new(),
            excluded_workspaces: Vec::new(),
            kinds: Vec::new(),
            tags: Vec::new(),
            attributes: Vec::new(),
            sort: SearchSort::Relevance,
            lifecycle: LifecycleFilter::Live,
            page_size: DEFAULT_PAGE_SIZE,
            offset: 0,
            request_seq: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchPage {
    pub hits: Vec<SearchHit>,
    pub next_offset: Option<usize>,
    /// Workspaces the answer may be behind on — not yet compared against the
    /// store since it came up, or with indexing still queued. **Reported rather
    /// than waited on**: a query that blocked until every workspace was current
    /// would be correct and occasionally unbounded.
    pub updating_workspaces: Vec<String>,
    pub request_seq: u64,
    /// A corrected spelling inferred from the highest-ranked result. Absent
    /// when the authored query itself supplied the evidence.
    pub corrected_query: Option<String>,
}

// ------------------------------------------------------------------ the route

/// The body of `POST /v1/search`.
///
/// **A body rather than a query string.** The access log records the path and
/// its query, and the store's logs rotate daily for a week —
/// `GLOBAL_SEARCH.md` is explicit that query text never enters a document, the
/// index, settings, telemetry or the recent-query list, and a URL would put it
/// in the middle of all of them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SearchRequest {
    #[serde(flatten)]
    pub query: SearchQuery,
    /// Also return the workspaces the index knows, for a scope picker.
    #[serde(default)]
    pub include_facets: bool,
    /// Also return this many recent queries. Absent means none.
    #[serde(default)]
    pub recent_queries: Option<usize>,
}

/// What `POST /v1/search` answers.
///
/// **One round trip, not three.** A search surface wants the page, the scope
/// picker's list and the recent queries on the same keystroke; the two extras
/// are optional and off by default, so a caller that only wants hits pays for
/// hits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchResponse {
    pub page: SearchPage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facets: Option<SearchFacets>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_queries: Option<Vec<RecentQuery>>,
}

/// The body of `POST /v1/search/navigated`.
///
/// **Machine-local, derived, and never written into a shared profile or a
/// collaborative document**: it describes this person's activity on this
/// desktop. A query earns a place in the history only by leading somewhere,
/// which is why the text arrives here rather than on the search itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Navigation {
    pub workspace: WorkspaceId,
    #[serde(default)]
    pub document: Option<String>,
    #[serde(default)]
    pub node: Option<String>,
    /// What was typed to get here. Empty is allowed and records nothing.
    #[serde(default)]
    pub query: String,
}
