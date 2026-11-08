// SPDX-License-Identifier: MPL-2.0

//! `POST /v1/tasks/query` — the tasks query service's wire bodies.
//!
//! **A second question asked of the one read model**, never a second index: the
//! rows come from the same `projection.db` the attribute query reads, and no
//! document is opened to build a list ([ADR-287]).
//!
//! ## Why the six filters are spelled twice
//!
//! [`TaskQuery`] declares `has`, `missing`, `tags`, `priority`, `min_priority`
//! and `due` with the same names, the same types and the same meanings as
//! [`AttributeQuery`](crate::attributes::AttributeQuery). Extracting a shared
//! sub-object out of that body would have been tidier and would also have been
//! a breaking change to a published route with generated schemas, so the
//! duplication is deliberate and
//! `the_shared_filters_serialize_identically_on_both_queries` is what holds the
//! two declarations equal. [`DueFilter`](crate::attributes::DueFilter) itself is
//! shared rather than copied, because it is a nested object whose two windows
//! must mean the same thing to both queries.
//!
//! The sharing that actually matters is one layer down, where a filter turns
//! into SQL: `Projections::task_rows` appends its conditions to the ones
//! `attribute_rows` and `attribute_groups` already use, so "priority at least
//! high" has one definition and three callers.
//!
//! ## A bucket is asked for, never known
//!
//! SPEC §7's agenda buckets are a pure function of the reader's display zone
//! and are **never persisted**. So the store neither stores one nor computes
//! one from a clock of its own: a caller that wants to page one bucket, or to
//! draw the rail's counts, resolves its zone into [`BucketWindows`] and sends
//! the edges, and the store does zero-timezone arithmetic against them. That is
//! [`TaskQuery::overdue`]'s pattern, extended — the same reason, the same
//! shape, and the same refusal to guess.
//!
//! The semantics are `koru_core::tasks::bucket`'s and are held identical by
//! a parity test over the fixture corpus, not by two prose descriptions that
//! agree today.
//!
//! ## Two trash flags, and they answer different questions
//!
//! [`TaskRow::document_trashed`] is the **document**-level answer — the
//! manifest stamp, inherited down the document tree — and it is *flagged, never
//! hidden*, because restore has to be able to find those rows.
//! [`TaskRow::in_trash`] is the **node**-level answer: the task sits in its own
//! document's hidden trash root, and rows like that are returned only when
//! [`TaskQuery::include_trashed`] asks for them. Both true means both things
//! happened.
//!
//! [ADR-287]: ../../../docs/decisions/ADR-276-300.md#adr-287-taskread--the-tasks-query-is-a-second-question-asked-of-the-one-read-model

use koru_store_core::ids::{DocumentId, WorkspaceId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::attributes::{AttributeDefinition, AttributeRowValue, AttributeTag, DueFilter};

/// `POST /v1/tasks/query`.
///
/// Every filter is optional and absent means "no constraint". Filters compose
/// with AND; the values inside `tags` compose with OR (any of). The base set is
/// **derived, never marked in a second table**: a node is a task when it
/// carries the `koru.task` marker or a due date, which is the whole derivation
/// ([ADR-285]).
///
/// [ADR-285]: ../../../docs/decisions/ADR-276-300.md#adr-285-taskmodel--a-task-is-a-marked-node-and-the-marker-or-a-due-date-is-the-whole-derivation
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskQuery {
    /// Which workspaces to search. Empty means every workspace this store
    /// holds.
    #[serde(default)]
    pub workspaces: Vec<WorkspaceId>,

    // ------------------------------------------- mirrored from AttributeQuery
    /// Only tasks carrying this attribute key (`koru.due`, or a custom
    /// definition's key).
    #[serde(default)]
    pub has: Option<String>,
    /// Only tasks **not** carrying this key.
    #[serde(default)]
    pub missing: Option<String>,
    /// Only tasks carrying any of these tags, each a `TagId` in storage form.
    /// Ids, never names — the tag table owns names and rename must not change
    /// what a saved query matches.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Only tasks whose `koru.priority` equals this token
    /// (`low | medium | high | urgent`).
    #[serde(default)]
    pub priority: Option<String>,
    /// Only tasks whose `koru.priority` is this token or more important — the
    /// semantic order `low < medium < high < urgent`, never the alphabetic one.
    #[serde(default)]
    pub min_priority: Option<String>,
    /// Which `koru.due` values match, **stated per temporal type**.
    #[serde(default)]
    pub due: Option<DueFilter>,

    // ---------------------------------------------------------- its own six
    /// Only tasks in **one agenda bucket** — `today`, `next`, `upcoming`,
    /// `later` or `completed` (SPEC §7).
    ///
    /// Usable **only with [`bucket_windows`](Self::bucket_windows)**, which is
    /// where the reader's zone was resolved; naming one without the other is a
    /// 400, as is a token that is not one of the five. A bucket is never
    /// persisted and never guessed — a store that invented today's date would
    /// be wrong by morning and wrong in another country.
    ///
    /// **A bucket names a lifecycle, so it supersedes
    /// [`lifecycle`](Self::lifecycle).** `Completed` is one of the five and the
    /// other four are open by rule 1 of the bucket function, so ANDing the two
    /// fields would answer `bucket: "completed"` with an empty page under the
    /// *default* lifecycle — a silent trap on the most obvious request. When
    /// this field is present, the query's `lifecycle` is not applied.
    #[serde(default)]
    pub bucket: Option<String>,
    /// The reader's zone, already resolved into days and instants — required by
    /// [`bucket`](Self::bucket) and by `group_by: "bucket"`.
    #[serde(default)]
    pub bucket_windows: Option<BucketWindows>,
    /// **Hide tasks completed before this day** — a canonical `YYYY-MM-DD`,
    /// inclusive, which the caller computed from its own today and
    /// [`COMPLETED_WINDOW_DAYS`].
    ///
    /// It applies to completed rows and to nothing else, so it narrows the
    /// Completed queue and leaves the four open ones exactly as they were.
    ///
    /// **A task the store cannot date is kept.** A completion recorded before
    /// this build carries no day, and a store that cannot tell how old
    /// something is must not call it old: hiding those would take work out of
    /// sight that nobody could ask for back. They age out as they are
    /// reopened, finished again, or taken off Tasks.
    #[serde(default)]
    pub completed_on_or_after: Option<String>,
    /// **Keep tasks completed on or after this day where they were**, rather
    /// than only in the Completed queue: a canonical `YYYY-MM-DD` the caller
    /// computed from its own today.
    ///
    /// What it loosens is *where a completed task is listed*, not what a task
    /// is. A queue places a row by its due when this covers it, so something
    /// ticked this morning stays on the page it was ticked on, struck
    /// through, instead of vanishing; a list shows it beside the open work in
    /// it. Nothing else about the row changes, and the row is still in the
    /// Completed queue as well — the two pages are two questions.
    ///
    /// **The counts do not carry it**, deliberately: a queue's number is how
    /// much open work is in it, and ticking something should make that number
    /// go down rather than leave it where it was.
    #[serde(default)]
    pub recently_completed_since: Option<String>,
    /// Only tasks in this list — a `ListId` in storage form.
    ///
    /// **A list is a filter, not a place** (SPEC §8): membership is one
    /// `koru.list` value on the task, so this is one more predicate over one
    /// more attribute and ANDs with every filter above it. Ids, never names —
    /// the workspace's list table owns names, and renaming a list must not
    /// change what a saved query matches. An id no table names matches nothing.
    #[serde(default)]
    pub list: Option<String>,
    /// Ask for **corpus-wide counts** beside the page. `list` or `bucket`;
    /// absent means no groups, and the response's `groups` is empty. An unknown
    /// token is a 400, never a silently ungrouped answer.
    ///
    /// The attribute query's rule, applied here: a client can count a page
    /// itself, and what it cannot do is count the rows it did not fetch — so a
    /// sidebar's "14" comes from its own statement over the same filter rather
    /// than from the page above it.
    ///
    /// `bucket` requires [`bucket_windows`](Self::bucket_windows) and answers
    /// **five rows per workspace**, one per bucket — the rail's
    /// *Today 5 · 3 overdue, Next 12, Upcoming 40, Later 9, Completed 118*. It
    /// partitions by lifecycle (`Completed` is one of the five), so it does not
    /// apply [`lifecycle`](Self::lifecycle) either, for
    /// [`bucket`](Self::bucket)'s reason.
    #[serde(default)]
    pub group_by: Option<String>,
    /// **Which dues count as overdue**, stated per temporal type — used only to
    /// split `groups`, never to filter rows.
    ///
    /// Overdue is a fact about the reader's display zone (SPEC §7) and the
    /// store carries no timezone database, so the caller computes the windows
    /// and sends them: typically `dates.before = today` and
    /// `instants.before_ms = now`. Absent means every group's `overdue` is
    /// zero, which is the honest answer to a question nobody asked.
    #[serde(default)]
    pub overdue: Option<DueFilter>,

    // -------------------------------------------------------- and its own three
    /// Open, completed, or both. Absent means open — a task list answers *what
    /// should I do next*, and absence of `koru.task_state` is itself `open`.
    #[serde(default)]
    pub lifecycle: LifecycleFilter,
    /// Also return tasks sitting in their document's own bin, each flagged with
    /// [`TaskRow::in_trash`]. Off by default; on is how a binned task stays
    /// restorable from a Tasks surface.
    ///
    /// It does **not** govern binned *documents*: those rows are always
    /// returned, flagged with [`TaskRow::document_trashed`].
    #[serde(default)]
    pub include_trashed: bool,
    /// `due`, `priority` or `modified`. Absent means `due` — soonest first,
    /// undated last.
    #[serde(default)]
    pub sort: TaskSort,

    /// Rows per page, clamped by the store; see [`MAX_LIMIT`].
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// The `task:` reference the previous page ended on, exclusive.
    ///
    /// **Keyset, never an offset.** A Tasks view re-queries at every local
    /// midnight and on every resume, and an offset page silently repeats or
    /// skips a row when anything inserts above it. A reference that no longer
    /// matches the filter is not an error: the answer comes from the top with
    /// [`TaskQueryResponse::restarted`] set.
    #[serde(default)]
    pub after: Option<String>,

    /// **Writes this answer must include** for the caller to trust it.
    ///
    /// A surface that has just written a document and re-asks the read model
    /// can be answered from a projection that has not seen the write yet — the
    /// store projects asynchronously and coalesces. Naming the write lets the
    /// answer say so through [`TaskQueryResponse::covered`], and the caller can
    /// ask again rather than draw a number it knows is behind.
    ///
    /// **Optional and additive.** Absent means the caller is not waiting on
    /// anything; an older client never sends it. At most
    /// [`MAX_COVERING`] entries; a malformed one refuses the request rather
    /// than answering *uncovered*, because a malformed requirement is a client
    /// bug and a quiet answer would hide it. Pagination is untouched.
    #[serde(default)]
    pub covering: Vec<RevisionRequirement>,
}

/// One write a query wants included — see [`TaskQuery::covering`].
///
/// The workspace rides with it: the query's scope is workspaces, and a
/// document the projector has never seen (the first native task's own
/// `task-list` document) has no projection row to place it by. Scope is
/// answered against the workspace, and an unprojected document is simply
/// *uncovered*.
/// **A scoped receipt: the one operation the caller is waiting for.**
///
/// This used to be the document's whole version vector, and that was a bug
/// with a delay fuse on it. A version vector holds one entry per writer the
/// document has ever had, and Loro mints a fresh writer id per session — so
/// the requirement grew by ~21 hex characters every time any process opened
/// the document, and once it crossed [`MAX_COVERING`]'s sibling length limit
/// the store refused **every** task query naming a write. Measured: 100
/// writers encode to 2 126 hex characters, 200 to 4 220. The surfaces then had
/// no way to wait, so every optimistic row snapped back — the tick that
/// un-ticks itself.
///
/// What a surface actually needs to know is *has the projection seen my
/// write*, and that is one operation, not a census of everybody who ever
/// touched the document. `peer` is the writing session and `counter` is the
/// **exclusive** end of its own operations at the moment the mutation
/// committed, which is exactly `VersionVector::get(peer)`.
///
/// **Bounded by construction**: two integers, whatever the document's history.
/// That is the property the old shape could not have at any limit.
///
/// **Multi-operation writes need nothing extra.** A command that writes five
/// operations still ends at one counter, and one peer's operations are
/// contiguous — so covering the end covers all of them. A command that wrote
/// nothing produces no receipt at all; see [`TaskQuery::covering`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RevisionRequirement {
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    /// The writing session's Loro peer id. A `String` because it is a `u64`
    /// and JSON numbers are `f64` — the id would lose its low bits above 2^53
    /// in any JavaScript client, and these ids are random across the range.
    pub peer: String,
    /// The exclusive end of that peer's own operations when the write
    /// committed. Covered when the projection has applied at least this many
    /// of them.
    pub counter: i64,
}

/// The most writes one query may wait on.
pub const MAX_COVERING: usize = 16;

/// **How long the store may hold a document before its rows are readable** —
/// the contract behind [`TaskQueryResponse::covered`], published here so a
/// client's retry policy and the projector's debounce are one number rather
/// than two that agree until one is tuned.
///
/// The projector waits for a document to be quiet this long after its last
/// commit before projecting it, so a query sent inside that window is answered
/// from the previous projection and nothing a client does can make it sooner.
pub const PROJECTION_QUIET_MILLIS: i64 = 500;

/// **How far back the Completed queue looks**, in days.
///
/// Completed is a log of what was finished, and a log nobody can reach the
/// end of is not one anybody reads. So it is a *window* rather than an
/// archive: what was finished recently is listed, and what was finished
/// before that simply is not — nothing is flagged, nothing is destroyed, and
/// every one of those tasks is still in its document, still ticked, and still
/// findable with `/task_state completed` in search.
///
/// **The caller computes the boundary**, because *thirty days ago* starts
/// from a day, and which day it is now is a zone question this crate has no
/// answer to — the rule [`BucketWindows`] states from the other side.
pub const COMPLETED_WINDOW_DAYS: i64 = 30;

/// **How far back *show recently completed* reaches**, in days.
///
/// One, which is *today and yesterday* — the day-precision answer to "the
/// last day or so". [`KEY_COMPLETED_AT`] records a day rather than an
/// instant, so a literal twenty-four hours is not a question this store can
/// answer; a window that always covers the last day and sometimes a little
/// more is the honest one, and it is what a person ticking things off this
/// morning and looking again tomorrow morning actually wants.
///
/// [`KEY_COMPLETED_AT`]: koru_core::attributes::KEY_COMPLETED_AT
pub const RECENTLY_COMPLETED_DAYS: i64 = 1;

/// …and holds it at most this long while commits keep arriving. A write is
/// therefore readable within this bound plus the transport, whatever else is
/// happening to the document.
pub const PROJECTION_HOLD_CEILING_MILLIS: i64 = 5_000;

/// A revision as the wire spells it.
pub fn revision_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The bytes behind a wire revision, or `None` when it is not hex.
pub fn revision_bytes(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 || !hex.is_ascii() {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

/// What a page holds when a caller names no limit.
pub const DEFAULT_LIMIT: usize = 100;

/// The most a caller may ask for in one page.
///
/// **Clamped rather than refused**, the search rule: a number too large is a
/// client being hopeful, not a client being wrong, and a page is built in
/// memory before it is written.
pub const MAX_LIMIT: usize = 500;

fn default_limit() -> usize {
    DEFAULT_LIMIT
}

impl Default for TaskQuery {
    fn default() -> Self {
        Self {
            workspaces: Vec::new(),
            has: None,
            missing: None,
            tags: Vec::new(),
            priority: None,
            min_priority: None,
            due: None,
            bucket: None,
            completed_on_or_after: None,
            recently_completed_since: None,
            bucket_windows: None,
            list: None,
            group_by: None,
            overdue: None,
            lifecycle: LifecycleFilter::default(),
            include_trashed: false,
            sort: TaskSort::default(),
            limit: DEFAULT_LIMIT,
            after: None,
            covering: Vec::new(),
        }
    }
}

/// **The reader's zone, resolved into days and instants by the caller.**
///
/// SPEC §7 makes a bucket a pure function of `(due, lifecycle, priority, today,
/// zone)`, and this store carries no timezone database — so a caller that wants
/// buckets computes the edges and sends them, exactly as
/// [`TaskQuery::overdue`] already does for the overdue split. Nothing here is
/// guessed, defaulted or derived from a server clock: a store that decided what
/// *today* meant would be wrong by morning and wrong in another country, which
/// is the whole reason [ADR-287] refused to persist a bucket.
///
/// Six edges rather than the two a first sketch has, because the bucket
/// function has **two** horizons and each of them has to be stated twice:
///
/// - a **day** edge, for dues stored as a calendar date or a floating wall
///   clock — those carry a day that needs no zone to read;
/// - an **instant** edge, for dues stored as a zoned instant, whose day is
///   `ZonedDateTime::date_in(zone)` and therefore the caller's to compute.
///
/// The two are compared each in its own terms and never folded together
/// ([ADR-183] rule 5). Calendar days here are inclusive last days — the day
/// `Next` ends on, and the day `Upcoming` ends on — because that is how the
/// domain function reads (*within seven days*, *within thirty*); the instants
/// are the moments those days **end**, which is the same edge said the other
/// way.
///
/// [ADR-183]: ../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
/// [ADR-287]: ../../../docs/decisions/ADR-276-300.md#adr-287-taskread--the-tasks-query-is-a-second-question-asked-of-the-one-read-model
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BucketWindows {
    /// Today in the reader's zone, strictly `YYYY-MM-DD`. A due on or before it
    /// is `Today` — **overdue lands in Today**, because an overdue task is the
    /// answer to *what should I do next*.
    pub today: String,
    /// The last day of `Next`, strictly `YYYY-MM-DD` — `today + 7` under the
    /// shipped rule, but the *caller's* arithmetic, not this store's.
    pub horizon: String,
    /// The last day of `Upcoming`, strictly `YYYY-MM-DD` — `today + 30` under
    /// the shipped rule. Beyond it is `Later`, which is why one horizon is not
    /// enough to place a row.
    pub upcoming_horizon: String,
    /// The instant [`today`](Self::today) ends at in the reader's zone,
    /// epoch-ms UTC: the edge a **zoned** due is compared against for `Today`.
    pub today_ends_ms: i64,
    /// The instant [`horizon`](Self::horizon) ends at, epoch-ms UTC.
    pub horizon_ends_ms: i64,
    /// The instant [`upcoming_horizon`](Self::upcoming_horizon) ends at,
    /// epoch-ms UTC.
    pub upcoming_ends_ms: i64,
    /// Now, epoch-ms UTC — what makes a *zoned* due overdue, beside
    /// [`today`](Self::today), which is what makes a dated one overdue.
    ///
    /// It splits the counts and never selects a row: an overdue task is in
    /// `Today` either way, and this is what tints it.
    pub now_ms: i64,
    /// The priority an **undated** task must reach to be `Next` rather than
    /// `Later` — a priority token, `high` under SPEC §7's rule that priority
    /// ≥ High replaces the original proposal's *Important*.
    ///
    /// On the wire rather than compiled in, for the reason every other window
    /// here is: it is a product rule the caller states, and a store that knew
    /// it would be a second place to change it.
    pub min_next_priority: String,
}

/// The five bucket tokens, in agenda order. The vocabulary
/// [`TaskQuery::bucket`] accepts and `group_by: "bucket"` answers with —
/// spelled from the domain's own enum wherever a server can reach it, and here
/// so a client has the list without linking the node model.
pub const BUCKETS: [&str; 5] = ["today", "next", "upcoming", "later", "completed"];

/// Which lifecycle a query wants.
///
/// **Absence is `open`**, both here and in the data: a task carrying no
/// `koru.task_state` is open, so a due-derived task is open without anybody
/// having written a value.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleFilter {
    #[default]
    Open,
    Completed,
    All,
}

/// What orders the page.
///
/// `Due` is the default because a task list is asked *what should I do next*.
/// `Due` and `Priority` sort set values first and unset last; `Modified` is the
/// source document's stamp. `Manual` is the order somebody dragged the tasks
/// into (`koru.rank`, smaller first); a task never ranked follows the ranked
/// ones by its due, and an undated one of those comes last.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskSort {
    #[default]
    Due,
    Priority,
    Modified,
    Manual,
}

/// What the query answers with.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskQueryResponse {
    /// The matching tasks, one row each, in the requested order.
    pub rows: Vec<TaskRow>,
    /// Every attribute definition in play: the system registry always, plus the
    /// scoped workspaces' custom definitions — so a surface draws a value from
    /// its type rather than guessing.
    pub definitions: Vec<AttributeDefinition>,
    /// The scoped workspaces' tag tables (live tags only, merge tombstones
    /// resolved away), so a `koru.tag` value resolves to a name and a chip
    /// color without another request.
    pub tags: Vec<AttributeTag>,
    /// Corpus-wide counts per group — per list, or per bucket — when the query
    /// asked for them with `group_by`. Empty otherwise.
    ///
    /// **Names are not repeated here.** A group carries its key — a `ListId`,
    /// or a bucket token — with the count and the overdue share; the name, the
    /// swatch and the rail order live in the workspace's list table, which a
    /// client reads from its own workspace rather than from this response. Two
    /// spellings of one name in one answer is a drift waiting to happen — the
    /// same rule `tags` follows above.
    #[serde(default)]
    pub groups: Vec<TaskGroup>,
    /// Whether every workspace in scope has reached the read model at all.
    /// `false` is "still indexing" — the projector is debounced, so a task
    /// created a moment ago may not be a row yet.
    pub projected: bool,
    /// The workspaces the answer may be behind on, by the same rule search and
    /// the attribute query use: **named rather than waited for**.
    #[serde(default)]
    pub updating_workspaces: Vec<WorkspaceId>,
    /// The page size actually applied, after the store's cap.
    pub limit: usize,
    /// The `task:` reference to pass as the next request's
    /// [`TaskQuery::after`], or absent when this page is the end of the list.
    #[serde(default)]
    pub next_after: Option<String>,
    /// The `after` this request carried named a row the filter no longer
    /// matches — completed, removed or binned between pages — so the answer is
    /// page one again. A client can tell a fresh list from a continuation
    /// instead of silently drawing page one twice.
    #[serde(default)]
    pub restarted: bool,
    /// Whether the rows and counts include every write in
    /// [`TaskQuery::covering`].
    ///
    /// **`None` means this server cannot tell** — an older store that never
    /// read the requirement. A client must read `None` as *unknown*, never as
    /// coverage: it records the capability as absent, falls back to its
    /// ordinary schedule and stops sending requirements, so an old server is
    /// never asked again quickly for an answer it cannot give. A server that
    /// understands the field always answers `Some`, `true` when nothing was
    /// required.
    ///
    /// A different question from [`projected`](Self::projected), which says
    /// whether the workspace has been indexed at all; both are worth having.
    #[serde(default)]
    pub covered: Option<bool>,
}

/// One group's share of the whole matching corpus, when a query asked for
/// counts with `group_by`.
///
/// **Keyed by `(workspace, key)`**, because a `ListId` belongs to one
/// workspace's list table and folding two workspaces' rails together would
/// name a group neither of them has. A *bucket* key is universal rather than
/// workspace-scoped, and the rows stay per workspace anyway: a task belongs to
/// exactly one workspace, so a cross-workspace rail sums them and loses
/// nothing, while folding here would throw away an answer a client may want.
///
/// Under `group_by: "list"` these do not overlap: `koru.list` is
/// cardinality-one, so a task is in exactly one list or in none, and the counts
/// sum to at most the number of matching tasks. A list nothing matches has **no
/// row** rather than a zero — a client draws its rail from the list table and
/// reads a missing group as an empty bucket. Buckets partition the corpus for
/// the same reason and completely: every task is in exactly one of the five,
/// so those counts sum to the whole matching set.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskGroup {
    /// Which workspace this group counts in.
    pub workspace: WorkspaceId,
    /// **The group's key**, in the vocabulary the query's `group_by` named: a
    /// `ListId` in storage form under `"list"`, one of [`BUCKETS`] under
    /// `"bucket"`.
    ///
    /// A list id the table no longer names still counts — a deleted list's
    /// memberships clear document by document, and a stale value is not a
    /// missing task.
    ///
    /// The field is spelled `list` because lists were the only grouping when it
    /// was published, and this route's schemas are generated and consumed:
    /// widening the name would break every caller and every literal built
    /// against it for a rename. It wants to be `key`, and that is a rename to
    /// make in one change with its call sites rather than a second field
    /// carrying a second meaning.
    pub list: String,
    /// Matching tasks in this group across the whole corpus — not the page.
    pub count: usize,
    /// How many of `count` are overdue: by the windows the query's `overdue`
    /// named when grouping by list, and by `bucket_windows`' `today` and
    /// `now_ms` when grouping by bucket — one notion of overdue through one SQL
    /// builder, stated once by whichever object the grouping already required.
    /// Zero when nothing named it; dateless tasks are never overdue, and a
    /// completed task is not overdue whatever its due says.
    pub overdue: usize,
    /// How many of `count` sit in a binned document. Counted and flagged
    /// rather than dropped, exactly as those rows are.
    pub trashed: usize,
}

/// One task. The identity is `(workspace, document, node)`, carried together —
/// never a node id on its own — and [`task`](Self::task) is the same triple as
/// the one string that is also its pagination key and its activity subject.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskRow {
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    /// The node id, meaningful only inside `document`.
    pub node: String,
    /// `task:<workspace>:<document>:<node>` — the row's identity, the cursor a
    /// page ends on, and the subject its activity is filed under.
    pub task: String,
    /// The node's kind token — one of the task-capable kinds `paragraph`,
    /// `task`, `document`, `image`, `file`.
    pub node_kind: String,
    /// `open` or `completed`. Absence of the stored state reads as `open`.
    pub lifecycle: String,
    /// Whether the **document** sits in the trash, inherited down the document
    /// tree. Flagged, never hidden.
    pub document_trashed: bool,
    /// Whether the **node** sits in its document's own hidden trash root. Rows
    /// like this come back only under [`TaskQuery::include_trashed`].
    pub in_trash: bool,
    /// Where the task lives: the workspace, then the document path, then the
    /// ancestor rows, joined by `" / "` — one rule, computed the same way by
    /// the desktop from an open document and by the store from the projection.
    pub breadcrumb: String,
    /// The task's own title: its text, else its authored name, else
    /// "Untitled" — `koru-title`'s one chain.
    pub title: String,
    /// Every attribute on the node — not only the ones the filter named, so a
    /// row can be drawn without a second request.
    pub attributes: Vec<AttributeRowValue>,
}

#[cfg(test)]
mod tests;
