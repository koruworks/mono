// SPDX-License-Identifier: MPL-2.0

//! What the Tasks tab's rows come from.
//!
//! **The store answers, and this asks.** A task is a marked node anywhere in
//! the workspace, so nothing the window has open can enumerate one; the read
//! model can, and `POST /v1/tasks/query` is the only door. This module is the
//! adapter — one query per refresh, on a worker thread, never on the event
//! thread — on [`TaskCountsPanel`]'s shape, because it is the same shape: a
//! cached client, a generation, a one-shot thread per unit of work, and a
//! deadline that is `None` while the tab is not the one on screen.
//!
//! [`TaskCountsPanel`]: crate::mainview::taskcounts::TaskCountsPanel
//!
//! ## The bucket is asked for, not folded here
//!
//! SPEC §7 makes a bucket a pure function of `(due, lifecycle, priority,
//! today, zone)`, and the store carries no timezone database — so the reader
//! resolves its zone into [`BucketWindows`] and the store does zero-timezone
//! arithmetic against the edges. That is why there is no `bucket_of` in this
//! file: a second copy of the rule is a second answer, and the one that lived
//! in `taskcounts.rs` had already drifted (it had no thirty-day horizon, so
//! every dated task past next week counted as *Upcoming* and *Later* held only
//! undated work).
//!
//! [`windows_now`] is the one place the edges are computed, and both this
//! surface and the rail's counts go through it.

use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use crate::panes::explore::rail::{Bucket, RailTarget};
use chrono::{Datelike, NaiveDate, TimeZone};
use koru_core::ids::ListId;
use koru_core::schema::WorkspaceDoc;
use koru_core::{DocumentId, NodeId, WorkspaceId};
use koru_store_wire::attributes::{AttributeRowValue, DateWindow, DueFilter, InstantWindow};
use koru_store_wire::tasks::{
    BucketWindows, DEFAULT_LIMIT, LifecycleFilter, PROJECTION_HOLD_CEILING_MILLIS,
    PROJECTION_QUIET_MILLIS, RevisionRequirement, TaskQuery, TaskQueryResponse, TaskRow, TaskSort,
};

/// How often the page is asked for again while the tab is on screen.
///
/// The rail's interval, and for the rail's reason: a list is context, not a
/// live readout. A mutation from this window refreshes it immediately through
/// [`TaskListPanel::invalidate`].
const REFRESH_EVERY: Duration = Duration::from_secs(30);

/// What "due within the week" means for the Next bucket (SPEC §7), and what
/// "within the month" means for Upcoming.
///
/// **The caller's arithmetic, sent on the wire** — see [`BucketWindows`]. They
/// are spelled here rather than read from `koru_core::tasks` because those are
/// private to the domain function; `bucket_placement_agrees_with_the_domain_function`
/// in `koru-projection` is what holds the two equal.
const NEXT_DAYS: i64 = 7;
const UPCOMING_DAYS: i64 = 30;

/// The priority an **undated** task must reach to be Next rather than Later.
pub const MIN_NEXT_PRIORITY: &str = "high";

/// **One write a surface is waiting for the read model to include.**
///
/// The read model is fed asynchronously and coalesces, so a query sent the
/// turn after a write is answered from the previous projection. A surface
/// that holds the write's receipt names it to the query
/// (`TaskQuery::covering`) and asks again, for a bounded period, while the
/// answer says it is behind (ADR-288).
///
/// Two producers, two guarantees. A task command the shell ran hands back its
/// receipt **exactly** — the version read the moment the write landed. The
/// editor's own gesture — a checkbox, a `/due` — is noticed by the activity
/// sweep, which holds the written document and reads its version there: **at
/// least the write**, never less, because a remote import in the same turn
/// can only widen it. Coverage is causal inclusion, so a superset is slower
/// and never wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteRequirement {
    pub workspace: koru_store_core::ids::WorkspaceId,
    pub document: koru_store_core::ids::DocumentId,
    /// The writing session, and how far its own operations had got. See
    /// [`koru_workspace::tasks::WriteReceipt`]: this used to be the
    /// document's whole version vector, which grew one entry per writer the
    /// document had ever had until the store refused the query outright.
    pub peer: u64,
    pub counter: i64,
}

impl WriteRequirement {
    /// From a command's receipt. `None` three ways, and they are three
    /// different facts rather than one:
    ///
    /// - **The command wrote nothing** (`receipt: None`) — a guarded no-op.
    ///   There is no write for a read model to catch up to, so waiting would
    ///   be waiting for an event that will never happen.
    /// - **The store was not told** (`published: false`) — a stopped session,
    ///   an outbox that will carry it later. A wait on a store that has not
    ///   heard of the write burns its whole budget every time.
    /// - **The receipt could not say which workspace** — nothing to address.
    ///
    /// All three mean *invalidate and do not wait*, and the caller must also
    /// not leave an optimistic row standing on a promise nothing will keep.
    pub fn of_landed(landed: &koru_workspace_host::tasks::Landed) -> Option<Self> {
        let receipt = landed.receipt.as_ref()?;
        if !landed.published {
            return None;
        }
        Some(Self {
            workspace: koru_workspace::explore::store_workspace(landed.workspace?),
            document: koru_workspace::explore::store_document(landed.document),
            peer: receipt.peer,
            counter: receipt.counter,
        })
    }

    /// From the document an editor gesture wrote, read where it is borrowed:
    /// the conservative producer. `None` for a document that does not know
    /// which workspace or which document it is — nothing to address.
    ///
    /// **This session's own counter**, which is at least the gesture's write
    /// and never less: a remote import arriving in the same turn advances
    /// *another* peer, so it cannot move this one past the write and cannot
    /// make the requirement unsatisfiable.
    pub fn of_document(ws: &WorkspaceDoc) -> Option<Self> {
        let peer = ws.peer_id();
        Some(Self {
            workspace: koru_workspace::explore::store_workspace(ws.id()?),
            document: koru_workspace::explore::store_document(ws.own_document_id()?),
            peer,
            counter: i64::from(ws.version().get(&peer).copied().unwrap_or(0)),
        })
    }

    /// As the query carries it.
    pub fn to_wire(&self) -> RevisionRequirement {
        RevisionRequirement {
            workspace: self.workspace,
            document: self.document,
            peer: self.peer.to_string(),
            counter: self.counter,
        }
    }

    /// Whether an answer that carried `self` also covers `want`.
    ///
    /// The same peer, at or past the counter — coalescing is what makes `>=`
    /// the right comparison rather than equality: twenty commits become one
    /// projection, and a caller waiting on operation 5 against rows applied at
    /// 6 has its answer.
    pub fn covers(&self, want: &Self) -> bool {
        self.workspace == want.workspace
            && self.document == want.document
            && self.peer == want.peer
            && self.counter >= want.counter
    }
}

/// How long after an uncovered answer the next question is asked.
pub const RETRY_MILLIS: u64 = 250;

/// What the transport and the query add to the store's own hold before a
/// write is readable — the one term the store's contract does not state.
///
/// **Provisional.** The projector's two constants are known; this one is
/// measured, through the diagnostics `KORU_TASKS_DEBUG` prints on every
/// *covered* transition, and corrected here when the owner's pass has
/// numbers (FRESHNESS §3 O).
pub const TRANSPORT_ALLOWANCE_MILLIS: i64 = 2_000;

/// The first question after a write is asked no sooner than this: the store
/// holds a document at least this long after its last commit, so an earlier
/// answer would be the previous projection and nothing a client does can make
/// it sooner.
pub fn first_ask_after_write() -> Duration {
    Duration::from_millis(PROJECTION_QUIET_MILLIS as u64)
}

/// The longest a surface keeps re-asking for one write: the store's own
/// ceiling, plus the allowance. **Derived, not a literal** — tuning the
/// projector moves this with it, through the wire crate both read.
pub fn wait_ceiling() -> Duration {
    Duration::from_millis((PROJECTION_HOLD_CEILING_MILLIS + TRANSPORT_ALLOWANCE_MILLIS) as u64)
}

/// **What one surface is waiting on, and how it stops.**
///
/// One type for the page and the rail: the failure being fixed is two
/// readings of one workspace disagreeing, and two independent backoffs would
/// be that mistake one level up. A display policy about what a reader will
/// tolerate, not a store concern — which is why it lives here beside the
/// windows both surfaces share and not in the store.
///
/// The rules, each with the answer that would go wrong without it:
///
/// - **An answer acknowledges only what its own request carried.** A request
///   sent before edit B can come back covering A alone, or nothing, and must
///   not clear B's wait. So every request is stamped with the requirements it
///   carried, and only those are satisfied by a *covered* answer.
/// - **Two writes to one document are one requirement**, the union of their
///   vectors: a projection covers the union exactly when it covers both.
/// - **One deadline per write, and a retry never resets it.** A requirement
///   joining an empty wait starts the clock; a new write joining a live one
///   extends it to its own ceiling, because it is a new write with its own
///   bound; an answer never moves it.
/// - **The ceiling ends the retries.** The last answer is drawn, the wait is
///   cleared, the ordinary schedule resumes. Bounded convergence, not a promise
///   of simultaneity.
/// - **`None` is *cannot tell*, and it turns the mechanism off.** An older
///   store never read the requirement; asking it again quickly would be a
///   burst of futile retries, so a surface that sees `None` stops sending
///   requirements and takes its ordinary schedule.
#[derive(Debug, Default)]
pub struct Wait {
    wanted: Vec<WriteRequirement>,
    /// When the current batch's retries stop.
    deadline: Option<Instant>,
    /// When the current batch began — for the interval the diagnostics report.
    since: Option<Instant>,
    /// Questions asked carrying this batch.
    asks: u32,
    /// `false` once the server answered `covered: None`.
    unsupported: bool,
}

/// What an answer did to the wait.
#[derive(Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    /// Nothing was being waited on.
    Idle,
    /// Every write the surface was waiting on is in the answer.
    Covered { after: Duration, asks: u32 },
    /// Ask again, at this instant.
    Retry(Instant),
    /// The ceiling ended the retries; the answer drawn is the last one.
    GaveUp { after: Duration, asks: u32 },
    /// The server cannot tell. Requirements are off from here on.
    Unsupported,
}

impl Wait {
    /// A write to wait on. Answers when to ask, or `None` when the server has
    /// said it cannot tell and the caller should take its ordinary schedule.
    pub fn require(&mut self, requirement: WriteRequirement, now: Instant) -> Option<Instant> {
        if self.unsupported {
            return None;
        }
        // **Folded per (workspace, document, peer)**, taking the later
        // operation. Two writes this session made to one document are one
        // thing to wait for — the second includes the first, because one
        // peer's operations are contiguous. A *different* peer writing the
        // same document is a separate requirement: its counter says nothing
        // about ours, and merging them would be waiting on a number that
        // means nothing.
        match self.wanted.iter_mut().find(|have| {
            have.workspace == requirement.workspace
                && have.document == requirement.document
                && have.peer == requirement.peer
        }) {
            Some(have) => have.counter = have.counter.max(requirement.counter),
            None => self.wanted.push(requirement),
        }
        if self.since.is_none() {
            self.since = Some(now);
            self.asks = 0;
        }
        self.deadline = Some(now + wait_ceiling());
        Some(now + first_ask_after_write())
    }

    /// The requirements the next request carries — a copy, because the
    /// answer will be acknowledged against exactly this set and nothing that
    /// joined afterwards.
    pub fn carrying(&self) -> Vec<WriteRequirement> {
        self.wanted.clone()
    }

    /// One question left carrying the batch.
    pub fn asked(&mut self) {
        if !self.wanted.is_empty() {
            self.asks = self.asks.saturating_add(1);
        }
    }

    /// The outstanding set, for the tests that walk a Wait through its
    /// states — production reads it through `carrying` and `is_waiting`.
    #[cfg(test)]
    pub fn wanted(&self) -> &[WriteRequirement] {
        &self.wanted
    }

    pub fn is_waiting(&self) -> bool {
        !self.wanted.is_empty()
    }

    /// The answer to a request that carried `carried` came back, saying
    /// `covered`.
    pub fn acknowledge(
        &mut self,
        carried: &[WriteRequirement],
        covered: Option<bool>,
        now: Instant,
    ) -> WaitOutcome {
        if self.wanted.is_empty() {
            return WaitOutcome::Idle;
        }
        let Some(covered) = covered else {
            self.unsupported = true;
            self.clear();
            return WaitOutcome::Unsupported;
        };
        let after = self
            .since
            .map(|since| now.saturating_duration_since(since))
            .unwrap_or_default();
        let asks = self.asks;
        if covered {
            // Satisfied: what this request named, and only that. A
            // requirement that moved on since — a later write to the same
            // document — is not in the answer and stays.
            self.wanted
                .retain(|want| !carried.iter().any(|sent| sent.covers(want)));
            if self.wanted.is_empty() {
                self.clear();
                return WaitOutcome::Covered { after, asks };
            }
        }
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.clear();
            return WaitOutcome::GaveUp { after, asks };
        }
        WaitOutcome::Retry(now + Duration::from_millis(RETRY_MILLIS))
    }

    /// Forget the batch: a workspace change, a filter change, a hidden
    /// surface, a failed worker. The ceiling would catch all of these; the
    /// common ones are cleared where they happen so a wait never outlives
    /// its subject by even a turn.
    pub fn clear(&mut self) {
        self.wanted.clear();
        self.deadline = None;
        self.since = None;
        self.asks = 0;
    }

    /// Whether the server has said it cannot answer coverage. Read by the
    /// tests; production acts on the [`WaitOutcome::Unsupported`] arm instead.
    #[cfg(test)]
    pub fn unsupported(&self) -> bool {
        self.unsupported
    }

    /// A new store may answer; forget that the old one could not.
    pub fn reset_support(&mut self) {
        self.unsupported = false;
    }
}

/// One wait's transition, on the debug switch. Content-free: a surface name,
/// an outcome, milliseconds and counts.
pub fn note_wait(surface: &'static str, outcome: &WaitOutcome) {
    match outcome {
        // **`Idle` and `Retry` are logged now, and they are the two that
        // mattered.** They were skipped as noise — a wait still running says
        // nothing, and one that was never armed says less. But a write the
        // store was never told to wait for produces exactly `Idle`, and that
        // is the state two rounds of guessing at the tick-that-unticks-itself
        // turned on. A line that only prints when something went right cannot
        // show what went wrong.
        WaitOutcome::Idle => crate::tasks::debug_line(format_args!(
            "{surface}: answer arrived with nothing waiting on it"
        )),
        WaitOutcome::Retry(_) => crate::tasks::debug_line(format_args!(
            "{surface}: answer is behind the write; asking again"
        )),
        WaitOutcome::Covered { after, asks } => crate::tasks::debug_line(format_args!(
            "{surface}: covered after {} ms, {asks} asks",
            after.as_millis()
        )),
        WaitOutcome::GaveUp { after, asks } => crate::tasks::debug_line(format_args!(
            "{surface}: gave up after {} ms, {asks} asks",
            after.as_millis()
        )),
        WaitOutcome::Unsupported => crate::tasks::debug_line(format_args!(
            "{surface}: unsupported — the store cannot say what an answer covers"
        )),
    }
}

/// One task's due value, in the only two shapes a comparison needs.
///
/// A calendar day and a floating wall clock are both read **as days** in the
/// display zone; a zoned value is compared **as an instant**, and its day is
/// the day it falls on locally. That is ADR-183's rule — each in its own
/// terms, never folded — reduced to what a chip has to decide.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Due {
    Day(NaiveDate),
    Instant(i64),
}

impl Due {
    /// The value as it arrives on the wire, or `None` for a kind this build
    /// does not know — which reads as *no due date* rather than as an error,
    /// the same tolerance every stored token here has.
    pub fn parse(kind: &str, value: &str) -> Option<Self> {
        match kind {
            "date" => koru_core::time::DateOnly::parse(value).map(|d| Self::Day(d.date())),
            "datetime_floating" => {
                koru_core::time::FloatingDateTime::parse(value).map(|f| Self::Day(f.wall().date()))
            }
            "datetime_zoned" => value.parse::<i64>().ok().map(Self::Instant),
            _ => None,
        }
    }

    /// Which local day it falls on. `None` only for an instant the local zone
    /// cannot place, which is a value nothing can draw a date for.
    pub fn local_day(self) -> Option<NaiveDate> {
        match self {
            Self::Day(d) => Some(d),
            Self::Instant(ms) => chrono::Local
                .timestamp_millis_opt(ms)
                .single()
                .map(|at| at.date_naive()),
        }
    }
}

/// Whether a due value is already past. A day is late once the day has
/// **ended**; an instant is late the moment it passes.
pub fn is_overdue(due: Due, today: NaiveDate, now_ms: i64) -> bool {
    match due {
        Due::Day(d) => d < today,
        Due::Instant(ms) => ms < now_ms,
    }
}

/// The instant a local day ends, epoch-ms UTC — the start of the day after it.
///
/// `earliest()` rather than `unwrap`: a spring-forward midnight does not exist
/// in every zone, and the earliest instant that *does* is the honest edge. A
/// zone with no answer at all falls back to the naive instant, which is wrong
/// by at most an offset and never a panic on somebody's calendar.
fn day_ends_ms(day: NaiveDate) -> i64 {
    let next = day.succ_opt().unwrap_or(day);
    let midnight = next.and_hms_opt(0, 0, 0).unwrap_or_else(|| next.into());
    chrono::Local
        .from_local_datetime(&midnight)
        .earliest()
        .map(|at| at.timestamp_millis())
        .unwrap_or_else(|| midnight.and_utc().timestamp_millis())
}

/// **The reader's zone, resolved into the six edges the store compares
/// against.** One function, two callers — this surface's page and the rail's
/// counts — so *today* means one thing in the window.
pub fn windows(today: NaiveDate, now_ms: i64) -> BucketWindows {
    let horizon = today + chrono::Duration::days(NEXT_DAYS);
    let upcoming = today + chrono::Duration::days(UPCOMING_DAYS);
    BucketWindows {
        today: today.format("%Y-%m-%d").to_string(),
        horizon: horizon.format("%Y-%m-%d").to_string(),
        upcoming_horizon: upcoming.format("%Y-%m-%d").to_string(),
        today_ends_ms: day_ends_ms(today),
        horizon_ends_ms: day_ends_ms(horizon),
        upcoming_ends_ms: day_ends_ms(upcoming),
        now_ms,
        min_next_priority: MIN_NEXT_PRIORITY.to_owned(),
    }
}

/// **The first day the Completed queue still lists** — today, less the
/// window the wire crate names.
///
/// Sent on every page rather than only on Completed: the predicate can only
/// exclude a *completed* row, so a queue of open work is untouched by it, and
/// a list page showing all lifecycles gets the same window its Completed
/// queue does without asking a second question.
pub fn completed_since(today: NaiveDate) -> String {
    (today - chrono::Duration::days(koru_store_wire::tasks::COMPLETED_WINDOW_DAYS))
        .format("%Y-%m-%d")
        .to_string()
}

/// **The first day *show recently completed* reaches back to** — today, less
/// the day the wire crate names.
pub fn recently_completed_since(today: NaiveDate) -> String {
    (today - chrono::Duration::days(koru_store_wire::tasks::RECENTLY_COMPLETED_DAYS))
        .format("%Y-%m-%d")
        .to_string()
}

/// [`windows`] against this machine's clock.
pub fn windows_now() -> (NaiveDate, i64, BucketWindows) {
    let today = chrono::Local::now().date_naive();
    let now_ms = chrono::Utc::now().timestamp_millis();
    (today, now_ms, windows(today, now_ms))
}

/// **Midnight, and waking on the other side of one.**
///
/// Every bucket is a function of *today*, so the local date moving makes all
/// five wrong at the same instant — and nothing else does: no mutation arrives,
/// no answer changes, and a page left open overnight is simply a lie until
/// something else happens to refresh it.
///
/// This is the whole mechanism, and what it deliberately is *not* is a timer.
/// The container already wakes on a maintenance tick; this compares one cached
/// date against the one the caller read on that tick and says whether it moved.
/// So there is **no clock of its own, no wake of its own, and no clock read per
/// frame** — and [`rolled`](DayWatch::rolled) is a pure function of the date it
/// is handed, which is what lets the rollover be tested on an injected day
/// rather than at midnight.
///
/// It lives here, beside [`windows_now`], because this file is the one place
/// the bucket edges are computed and the rollover is the one event that
/// invalidates them.
#[derive(Default, Debug)]
pub struct DayWatch {
    day: Option<NaiveDate>,
}

impl DayWatch {
    /// Whether the local date has moved since the last call.
    ///
    /// **The first call never rolls.** There is nothing to be stale yet — a
    /// window that refreshed everything on its first tick would pay for a
    /// rollover on every launch.
    pub fn rolled(&mut self, today: NaiveDate) -> bool {
        let moved = self.day.is_some_and(|day| day != today);
        self.day = Some(today);
        moved
    }

    /// The day this watch last saw. Test-only: production asks
    /// [`rolled`](Self::rolled), which is the only question there is.
    #[cfg(test)]
    pub fn day(&self) -> Option<NaiveDate> {
        self.day
    }
}

/// The wire token for a bucket. The vocabulary is
/// [`koru_store_wire::tasks::BUCKETS`]; this is the rail enum's half of it.
pub fn bucket_token(bucket: Bucket) -> &'static str {
    match bucket {
        Bucket::Today => "today",
        Bucket::Next => "next",
        Bucket::Upcoming => "upcoming",
        Bucket::Later => "later",
        Bucket::Completed => "completed",
    }
}

// ---------------------------------------------------------------- the filters

/// The priority floor the *chip* asks for — SPEC §7's `≥ High`, which is the
/// same token the undated half of `Next` uses and deliberately not the same
/// constant: one is a bucket rule the store is handed, the other is a filter a
/// person switched on, and they are free to move apart.
pub const CHIP_MIN_PRIORITY: koru_core::attributes::Priority =
    koru_core::attributes::Priority::High;

/// Which due window the page is narrowed to.
///
/// **One dimension with three values.** *Overdue* and *This week* are two
/// answers to *when*, and ANDing them would ask for a due that is both before
/// today and after it — a page nobody meant. So the surface spells them as one
/// selector that steps between them, on the one page that offers the dimension
/// at all ([`Filters::due_selects`]).
///
/// **Idle while no page draws a filter row** (the owner, 2026-11-05): nothing
/// in the app sets a window now, and the query still honours one, so the row
/// can come back without the query being rebuilt. The suite drives it.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
#[allow(dead_code)]
pub enum DueWindow {
    #[default]
    Any,
    /// Due before today, or — for a zoned value — before now. **The store's own
    /// overdue**, spelled from the same six edges the rail's split count uses.
    Overdue,
    /// Due today through the end of this calendar week.
    ThisWeek,
}

impl DueWindow {
    /// The window as the wire states it, per temporal type.
    ///
    /// **Both halves or neither** ([ADR-183] rule 5): a calendar day and a zoned
    /// instant are compared each in its own terms, so a window that named only
    /// dates would quietly drop every timed task out of the answer.
    ///
    /// [ADR-183]: ../../../docs/decisions/ADR-201-225.md
    pub fn filter(self, windows: &BucketWindows, today: NaiveDate) -> Option<DueFilter> {
        match self {
            Self::Any => None,
            // The rail's own tint, as a filter: `windows.today` and
            // `windows.now_ms` are the two edges `TaskQuery::overdue` splits the
            // counts by, so the chip cannot mean a different *overdue* from the
            // number beside the queue.
            Self::Overdue => Some(DueFilter {
                dates: Some(DateWindow {
                    after: None,
                    before: Some(windows.today.clone()),
                }),
                instants: Some(InstantWindow {
                    after_ms: None,
                    before_ms: Some(windows.now_ms),
                }),
            }),
            // Today through Sunday, inclusive — `[after, before)` with the
            // exclusive end written as the day after, which is how every window
            // on this wire spells an inclusive last day.
            Self::ThisWeek => {
                let end = week_end(today);
                Some(DueFilter {
                    dates: Some(DateWindow {
                        after: Some(day_token(today)),
                        before: Some(day_token(end.succ_opt().unwrap_or(end))),
                    }),
                    instants: Some(InstantWindow {
                        // The instant today *began*, so a timed task earlier
                        // today is still due this week — it is late, which is
                        // the other chip's business and not this one's.
                        after_ms: Some(day_ends_ms(today.pred_opt().unwrap_or(today))),
                        before_ms: Some(day_ends_ms(end)),
                    }),
                })
            }
        }
    }
}

fn day_token(day: NaiveDate) -> String {
    day.format("%Y-%m-%d").to_string()
}

/// The Sunday that ends `day`'s week.
fn week_end(day: NaiveDate) -> NaiveDate {
    day + chrono::Duration::days(6 - day.weekday().num_days_from_monday() as i64)
}

/// What the chips row and the overflow menu move.
///
/// **Every field is a field of [`TaskQuery`]**, and that is the whole contract:
/// this surface has no filter of its own, so a control here can only ever change
/// what the store is asked. A chip that narrowed the rows *after* the answer
/// arrived would disagree with its own page as soon as the page was longer than
/// the limit.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Filters {
    /// `Priority ≥ High` — [`TaskQuery::min_priority`].
    pub min_high: bool,
    /// Open, Completed, or both.
    ///
    /// **The store applies this only where the query names no bucket.** A bucket
    /// names its own lifecycle ([`TaskQuery::bucket`]), so on a queue page the
    /// field is discarded — which is why the chip and the menu row that move it
    /// would be offered only where they bite.
    pub lifecycle: LifecycleFilter,
    pub window: DueWindow,
    /// **Keep what you just finished on the page you finished it on** —
    /// *Show recently completed*, the overflow's one switch.
    ///
    /// Off by default, so a queue is the work in front of you. On, a task
    /// completed today or yesterday is listed where its due puts it, struck
    /// through, instead of leaving the page the moment it is ticked. It says
    /// nothing about the Completed queue, which lists completed work by
    /// definition.
    pub recently_completed: bool,
    /// How a queue page is ordered. `Due` unless somebody chose otherwise.
    pub sort: TaskSort,
    /// How a **list** page is ordered: the order somebody dragged it into
    /// (`Manual`) unless they chose a sort. Its own field so a queue's choice
    /// and a list's do not overwrite each other when the tab moves between
    /// them — a queue has no manual order to offer.
    pub list_sort: TaskSort,
}

impl Default for Filters {
    fn default() -> Self {
        Self {
            min_high: false,
            lifecycle: LifecycleFilter::default(),
            window: DueWindow::default(),
            recently_completed: false,
            sort: TaskSort::Due,
            list_sort: TaskSort::Manual,
        }
    }
}

impl Filters {
    /// Whether a page may hold a due window at all: a list, which spans all
    /// time — SPEC §8's *a list is a filter, not a place*. A queue is already
    /// a due window, so a second one over it is redundant where they agree
    /// and empty where they do not; leaving a list clears it.
    pub fn due_selects(target: RailTarget) -> bool {
        matches!(target, RailTarget::List(_))
    }

    /// The order `target`'s page is asked for: a list's own, or a queue's.
    pub fn sort_for(&self, target: RailTarget) -> TaskSort {
        match target {
            RailTarget::List(_) => self.list_sort,
            _ => self.sort,
        }
    }

    /// Choose the order for `target`'s kind of page.
    pub fn set_sort_for(&mut self, target: RailTarget, sort: TaskSort) {
        match target {
            RailTarget::List(_) => self.list_sort = sort,
            _ => self.sort = sort,
        }
    }
}

/// The page this surface draws: one bucket, or one list, narrowed by whatever
/// the chips and the overflow menu are set to.
///
/// **A list is a filter, not a place** (SPEC §8), so the list arm is the same
/// query with one more predicate — and it is the arm whose lifecycle the reader
/// owns, because a bucket names its own.
pub fn page_query(
    workspace: koru_store_core::ids::WorkspaceId,
    target: RailTarget,
    windows: &BucketWindows,
    filters: Filters,
    today: NaiveDate,
) -> TaskQuery {
    let base = TaskQuery {
        workspaces: vec![workspace],
        bucket_windows: Some(windows.clone()),
        min_priority: filters
            .min_high
            .then(|| CHIP_MIN_PRIORITY.as_str().to_owned()),
        due: filters.window.filter(windows, today),
        sort: filters.sort_for(target),
        lifecycle: filters.lifecycle,
        completed_on_or_after: Some(completed_since(today)),
        recently_completed_since: filters
            .recently_completed
            .then(|| recently_completed_since(today)),
        limit: DEFAULT_LIMIT,
        ..TaskQuery::default()
    };
    match target {
        RailTarget::Reminders => unreachable!(
            "the rail's Reminders row swaps the tab's view; a Tasks page never targets it"
        ),
        RailTarget::Bucket(bucket) => TaskQuery {
            bucket: Some(bucket_token(bucket).to_owned()),
            // Said out loud rather than left to the default: on this arm the
            // store discards the field, and a page whose query carried a
            // lifecycle nobody could have set would read as a bug in the filter.
            lifecycle: LifecycleFilter::default(),
            ..base
        },
        RailTarget::List(id) => TaskQuery {
            list: Some(id.to_storage()),
            ..base
        },
    }
}

// ------------------------------------------------------------------- the row

/// What kind of thing the task is, as far as the icon lane cares.
///
/// SPEC §4's task-capable kinds, folded to the three that draw differently: a
/// paragraph or a task row wears no icon at all (the complete control is
/// already at its head), an image and a file wear their type's glyph, and a
/// document wears the page glyph.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum RowKind {
    Text,
    Document,
    Image,
    File,
}

impl RowKind {
    fn of(token: &str) -> Self {
        match token {
            "document" => Self::Document,
            "image" => Self::Image,
            "file" => Self::File,
            // `paragraph`, `task`, and anything a newer build knows and this
            // one does not: a row with no glyph is legible; a wrong glyph is a
            // claim.
            _ => Self::Text,
        }
    }

    /// The glyph, or `None` for a plain row.
    pub fn glyph(self, mime: Option<&str>, name: &str) -> Option<char> {
        match self {
            Self::Text => None,
            Self::Document => Some(koru_draw::icons::DOCUMENT),
            Self::Image => Some(koru_draw::icons::IMAGE),
            Self::File => Some(koru_ui::file_icon::FileIcon::new(mime, Some(name)).glyph()),
        }
    }
}

/// One row of the list, read from an answer rather than from a document.
#[derive(Clone, PartialEq, Debug)]
pub struct TaskEntry {
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    pub node: NodeId,
    /// `task:<workspace>:<document>:<node>` — the row's identity on the wire.
    pub task: String,
    pub kind: RowKind,
    pub mime: Option<String>,
    /// The title with any leading emoji taken off, so the emoji is not drawn
    /// twice — the rule the explore column, the tab strip and Trash share.
    pub title: String,
    pub icon: Option<String>,
    /// Where it lives, as a person reads it — the store's own breadcrumb, which
    /// is computed by the same `koru_core::tasks::breadcrumb` the desktop uses
    /// from an open document.
    pub breadcrumb: String,
    pub due: Option<Due>,
    pub overdue: bool,
    /// The stored priority token (`low | medium | high | urgent`).
    pub priority: Option<String>,
    pub list: Option<ListId>,
    /// The line's own colour (`koru.color`) — an accent token, drawn as the
    /// swatch the document draws after the same task's chips.
    pub color: Option<String>,
    /// Where the task sits in its list's dragged order (`koru.rank`), or
    /// `None` when nobody has placed it.
    pub rank: Option<i64>,
    pub completed: bool,
    /// **Put on Today by hand**, whatever the due says.
    ///
    /// Read off the row's own attributes, which the wire already carries in
    /// full — "not only the ones the filter named, so a row can be drawn
    /// without a second request". What it decides here is which way the menu's
    /// row reads, the way `completed` decides between *Complete* and *Reopen*.
    pub pinned_today: bool,
    /// The document is binned, or the node is in its document's own bin. Both
    /// draw the one *In trash* chip; which of them happened is not a
    /// distinction a list row can carry usefully.
    pub trashed: bool,
    /// Completed, and still holding unfinished structural work — SPEC §5's
    /// concurrent residue, and E-2's warning glyph.
    ///
    /// **`false` unless this window can see it.** The answer needs the task's
    /// own document, and the query carries no such column; a row whose document
    /// this window does not hold is drawn without the triangle rather than with
    /// a guess. See [`TasksView::catch_up`](crate::tasks_view::TasksView::catch_up).
    pub residue: bool,
}

fn attr<'a>(row: &'a TaskRow, key: &str) -> Option<&'a AttributeRowValue> {
    row.attributes.iter().find(|a| a.role == key)
}

fn value_of<'a>(row: &'a TaskRow, key: &str) -> Option<&'a str> {
    attr(row, key).map(|a| a.value.as_str())
}

impl TaskEntry {
    /// One wire row, in this view's terms.
    ///
    /// `None` when the node id will not parse — a refusal rather than a row
    /// that cannot be opened, which is `search::pane_request`'s rule applied to
    /// the same shape of input.
    pub fn of(row: &TaskRow, today: NaiveDate, now_ms: i64) -> Option<Self> {
        let node = NodeId::parse(&row.node)?;
        let due =
            attr(row, koru_core::attributes::KEY_DUE).and_then(|a| Due::parse(&a.kind, &a.value));
        let completed = row.lifecycle == "completed";
        let (icon, rest) = koru_shell_chrome::split_leading_emoji(&row.title);
        let title = if rest.trim().is_empty() {
            "Untitled".to_owned()
        } else {
            rest.trim().to_owned()
        };
        Some(Self {
            workspace: WorkspaceId::from_uuid(row.workspace.as_uuid()),
            document: DocumentId::from_uuid(row.document.as_uuid()),
            node,
            task: row.task.clone(),
            kind: RowKind::of(&row.node_kind),
            mime: attr(row, "koru.mime").map(|a| a.value.clone()),
            title,
            icon: icon.map(str::to_owned),
            breadcrumb: row.breadcrumb.clone(),
            due,
            // **A completed task is never overdue**, whatever its due says —
            // the store's own rule for the group counts, kept here so the tint
            // and the number cannot disagree.
            overdue: !completed && due.is_some_and(|due| is_overdue(due, today, now_ms)),
            priority: value_of(row, koru_core::attributes::KEY_PRIORITY).map(str::to_owned),
            list: value_of(row, koru_core::attributes::KEY_LIST).and_then(ListId::parse),
            color: value_of(row, koru_core::attributes::KEY_COLOR)
                .filter(|token| !token.is_empty())
                .map(str::to_owned),
            rank: value_of(row, koru_core::attributes::KEY_RANK)
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite())
                .map(|value| value as i64),
            completed,
            // Presence *and* truth, the same rule the bucket SQL applies:
            // unpinning removes the record, so a stored `false` is not
            // something this build writes — and where one arrives anyway, both
            // readers say not pinned.
            pinned_today: value_of(row, koru_core::attributes::KEY_TODAY) == Some("true"),
            trashed: row.document_trashed || row.in_trash,
            residue: false,
        })
    }

    /// The local day this row is due on. `None` for an undated row, which never
    /// earns a rule-label of its own.
    pub fn due_day(&self) -> Option<NaiveDate> {
        self.due?.local_day()
    }
}

// ---------------------------------------------------------------- the panel

/// One answered page, stamped so a stale one is discarded rather than drawn.
#[derive(Debug)]
struct Stamped {
    workspace: WorkspaceId,
    target: RailTarget,
    generation: u64,
    /// The requirements the request carried, so the answer acknowledges
    /// exactly those and nothing that joined the wait afterwards.
    carried: Vec<WriteRequirement>,
    answer: Result<Answer, String>,
}

#[derive(Debug)]
pub struct Answer {
    rows: Vec<TaskEntry>,
    /// Workspaces the answer may be behind on — named rather than waited for.
    /// V-3 draws the chip; V-2 keeps the field so the page it is drawn from is
    /// the page it describes.
    updating: bool,
    day: NaiveDate,
    /// Whether the rows include every write the request named; `None` when
    /// the store cannot say.
    covered: Option<bool>,
}

/// The background half of the Tasks tab.
#[derive(Default)]
pub struct TaskListPanel {
    client: Option<koru_api_client::Client>,
    workspace: Option<WorkspaceId>,
    target: Option<RailTarget>,
    /// What the chips and the overflow menu are set to. Pushed in by the view,
    /// because the controls are the view's and the query is this panel's.
    filters: Filters,
    visible: bool,
    generation: u64,
    receiver: Option<Receiver<Stamped>>,
    next_refresh: Option<Instant>,
    rows: Vec<TaskEntry>,
    updating: bool,
    /// The local day the page was bucketed for. Midnight makes every bucket
    /// wrong at once, which is why the rollover is a refresh trigger rather
    /// than something the next mutation happens to fix.
    day: Option<NaiveDate>,
    /// Whether an answer has ever arrived. Before it has, an empty list means
    /// *we have not looked yet* rather than *there is nothing* — the same
    /// distinction `TaskCounts::known` draws for the rail.
    known: bool,
    notice: Option<String>,
    /// The writes this page is waiting for the read model to include, and
    /// the policy that stops waiting.
    wait: Wait,
    /// An answer arrived and no wait is left standing — the echoes the view
    /// has been drawing over the store's rows are in the rows now, or the
    /// wait gave up. Taken by the view once.
    settled: bool,
}

impl TaskListPanel {
    /// Cache the store client supplied during drawing. **No request is made
    /// here** — a view with no client (every unit test) makes none at all.
    pub fn set_client(&mut self, client: Option<koru_api_client::Client>) {
        self.client = client;
    }

    pub fn rows(&self) -> &[TaskEntry] {
        &self.rows
    }

    /// The workspace the page is on — what a provisional row is stamped with.
    pub fn workspace(&self) -> Option<WorkspaceId> {
        self.workspace
    }

    /// Whether the last answer left no wait standing, once.
    pub fn take_settled(&mut self) -> bool {
        std::mem::take(&mut self.settled)
    }

    pub fn known(&self) -> bool {
        self.known
    }

    /// Whether the answer named a workspace it may be behind on.
    ///
    /// What the *Updating…* chip is drawn from, and it is kept beside the page
    /// it describes so the two cannot come from different answers: the chip says
    /// something about **these rows**, not about the store in general.
    pub fn updating(&self) -> bool {
        self.updating
    }

    /// What this panel is querying with. **Test-only**: the view owns the
    /// filters and pushes them down, so a second production reader would be a
    /// second answer to a question one side already holds.
    #[cfg(test)]
    pub fn filters(&self) -> Filters {
        self.filters
    }

    /// Point the query at another set of filters. Answers whether it moved.
    ///
    /// **A filter change is a new page, not a re-reading of this one.** It takes
    /// the same path a bucket change takes — a new generation, the rows dropped,
    /// `known` back to false — so a page in flight under the old filters cannot
    /// land under the new ones, and an empty list means *we have not looked yet*
    /// for the same half-second it does anywhere else.
    pub fn set_filters(&mut self, filters: Filters) -> bool {
        if self.filters == filters {
            return false;
        }
        self.filters = filters;
        self.restart(Instant::now());
        true
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn day(&self) -> Option<NaiveDate> {
        self.day
    }

    /// How many open tasks the page holds — the header's count.
    pub fn open_count(&self) -> usize {
        self.rows.iter().filter(|row| !row.completed).count()
    }

    /// Ask again on the next maintenance turn — what a mutation in this window
    /// calls, so completing something moves the row now rather than in half a
    /// minute.
    pub fn invalidate(&mut self) {
        self.invalidate_at(Instant::now());
    }

    fn invalidate_at(&mut self, now: Instant) {
        if self.visible && self.workspace.is_some() && self.target.is_some() {
            self.next_refresh = Some(now);
        }
    }

    /// Ask again, **and keep asking until the answer includes this write.**
    /// The invalidation a receipt causes, as opposed to the one a rollover
    /// or a stale navigation causes — those have no write to wait for.
    ///
    /// The first question waits the store's quiet window: an earlier one
    /// would be answered from the previous projection, whatever the store is
    /// doing. A store that cannot say what it covers gets the plain
    /// invalidation instead.
    pub fn require(&mut self, requirement: WriteRequirement) {
        self.require_at(requirement, Instant::now());
    }

    fn require_at(&mut self, requirement: WriteRequirement, now: Instant) {
        match self.wait.require(requirement, now) {
            Some(ask_at) => {
                if self.visible && self.workspace.is_some() && self.target.is_some() {
                    self.next_refresh = Some(ask_at);
                }
            }
            None => self.invalidate_at(now),
        }
    }

    #[cfg(test)]
    pub fn wanted_for_test(&self) -> &[WriteRequirement] {
        self.wait.wanted()
    }

    #[cfg(test)]
    pub fn require_for_test(&mut self, requirement: WriteRequirement, now: Instant) {
        self.require_at(requirement, now);
    }

    #[cfg(test)]
    pub fn next_refresh_for_test(&self) -> Option<Instant> {
        self.next_refresh
    }

    /// The only clock this panel contributes to the event loop. **A background
    /// Tasks tab has no deadline and therefore no traffic at all.**
    pub fn deadline(&self) -> Option<Instant> {
        (self.visible && self.workspace.is_some() && self.target.is_some())
            .then_some(self.next_refresh)
            .flatten()
    }

    /// Answers whether anything the list draws changed.
    pub fn maintain(
        &mut self,
        workspace: Option<WorkspaceId>,
        target: Option<RailTarget>,
        visible: bool,
        now: Instant,
    ) -> bool {
        let mut changed = false;

        if workspace != self.workspace || target != self.target {
            self.workspace = workspace;
            self.target = target;
            self.visible = visible;
            self.restart(now);
            changed = true;
        }

        if visible != self.visible {
            self.visible = visible;
            self.next_refresh =
                (visible && self.workspace.is_some() && self.target.is_some()).then_some(now);
            // A hidden page waits on nothing: it generates no traffic, and a
            // wait kept across the hide would outlive what it was about.
            if !visible {
                self.wait.clear();
            }
        }

        // **The rollover is not detected here.** It was, in this panel and in
        // the rail's, which is two clock reads and two answers to one question
        // — and the counts panel's could only fire after its first answer had
        // landed. [`DayWatch`] is the one place now, driven off the container's
        // maintenance tick, and it invalidates both panels through the door a
        // mutation already uses.

        if let Some(receiver) = &self.receiver
            && let Ok(stamped) = receiver.try_recv()
        {
            self.receiver = None;
            let current = Some(stamped.workspace) == self.workspace
                && Some(stamped.target) == self.target
                && stamped.generation == self.generation;
            if current {
                changed |= self.apply(stamped.answer, &stamped.carried, now);
            }
        }

        if self.visible
            && self.receiver.is_none()
            && self.next_refresh.is_some_and(|due| due <= now)
        {
            self.next_refresh = Some(now + REFRESH_EVERY);
            changed |= self.start();
        }
        changed
    }

    /// Throw this page away and ask for another.
    ///
    /// **One body, three callers** — a new workspace, a new bucket, a new
    /// filter. Each is a different question, and a page answered for the old one
    /// must not be drawn under the new one; the generation is what makes an
    /// answer already in flight land and be discarded.
    fn restart(&mut self, now: Instant) {
        self.generation = self.generation.wrapping_add(1);
        self.rows.clear();
        self.known = false;
        self.notice = None;
        // A different question waits on nothing the old one was owed — and a
        // different workspace may be a different store, so what the old one
        // could not answer is forgotten with it.
        self.wait.clear();
        self.wait.reset_support();
        self.next_refresh =
            (self.visible && self.workspace.is_some() && self.target.is_some()).then_some(now);
    }

    /// Take the answer, and let the wait decide whether to ask again.
    ///
    /// **The answer is drawn whatever the wait says.** Bounded convergence
    /// keeps the last answer on screen and asks again; it never blanks a
    /// number because it is known to be behind.
    fn apply(
        &mut self,
        answer: Result<Answer, String>,
        carried: &[WriteRequirement],
        now: Instant,
    ) -> bool {
        match answer {
            Ok(answer) => {
                let changed = answer.rows != self.rows
                    || answer.updating != self.updating
                    || !self.known
                    || self.notice.is_some();
                self.rows = answer.rows;
                self.updating = answer.updating;
                self.day = Some(answer.day);
                self.known = true;
                self.notice = None;
                let covered = answer.covered;
                let outcome = self.wait.acknowledge(carried, answer.covered, now);
                note_wait("page", &outcome);
                if let WaitOutcome::Retry(at) = outcome {
                    self.next_refresh = Some(at);
                }
                // The echoes the view draws over these rows are dropped when
                // the wait ends — this answer has the write, or nothing will
                // — never on an answer that arrived while one is standing.
                //
                // **A wait that *ended*, not the mere absence of one.** This
                // asked `!is_waiting()`, which is also true when there was
                // never anything to wait for — and that is an ordinary state,
                // not a rare one: `WriteRequirement::of_landed` answers `None`
                // for an unpublished write (a stopped session, an outbox that
                // will carry it later) and for a receipt naming no addressable
                // workspace, and the container then invalidates *without* a
                // requirement. The echo is armed all the same, so the very next
                // answer — necessarily taken before a write the store has not
                // been told about — settled and dropped it. The row went back
                // to what it was, and then forward again when the projection
                // caught up: the tick that un-ticks itself.
                //
                // An echo with nothing to wait on is not stranded by this. It
                // is bounded by the ceiling in `TasksView::catch_up`, which is
                // what the module note means by *no echo outlives the bounded
                // wait by more than a turn*.
                if matches!(
                    outcome,
                    WaitOutcome::Covered { .. }
                        | WaitOutcome::GaveUp { .. }
                        | WaitOutcome::Unsupported
                ) {
                    self.settled = true;
                }
                crate::tasks::debug_line(format_args!(
                    "page: answer rows={} covered={} settled={} changed={changed}",
                    self.rows.len(),
                    match covered {
                        Some(true) => "yes",
                        Some(false) => "no",
                        None => "unsupported",
                    },
                    self.settled,
                ));
                changed
            }
            Err(error) => {
                // **A failure does not settle anything.**
                //
                // This used to clear the wait and set `settled`, which threw
                // away the optimistic rows on the strength of an answer that
                // never arrived — so a store refusing every query (as one did,
                // for months, once the old covering revision outgrew its
                // length limit) looked exactly like a click that did not
                // register. A write is still owed until an answer says it is
                // included or the ceiling gives up on it; a transport that
                // could not ask is neither.
                //
                // The wait's own deadline still ends it, and the echo ceiling
                // still bounds the rows, so nothing waits for ever. What is
                // gone is *coverage being discarded by an error*.
                if self.wait.is_waiting() {
                    crate::tasks::debug_line(format_args!(
                        "page: query failed while waiting; the write is still owed: {error}"
                    ));
                } else {
                    crate::tasks::debug_line(format_args!("page: query failed: {error}"));
                }
                // Ask again on the ordinary schedule rather than stopping: a
                // refusal that is a client bug repeats identically and is
                // read in the notice, and a transient one heals.
                let changed = self.notice.as_deref() != Some(error.as_str());
                self.notice = Some(error);
                changed
            }
        }
    }

    /// Take one fabricated answer, exactly as the worker's would be taken.
    ///
    /// **The tests build what they assert on**, which is also what keeps this
    /// suite from dialling anything: the whole point of the freshness chip is
    /// that it comes off the page it describes, so a test that set a `bool`
    /// would be asserting nothing.
    /// Take one refusal, exactly as the worker's would be taken — which is the
    /// only way to arm the state where a page has a notice and has still never
    /// been answered. That combination is not a corner: it is what every failed
    /// first query looks like.
    #[cfg(test)]
    pub fn fail_for_test(&mut self, error: &str) -> bool {
        self.apply(Err(error.to_owned()), &[], Instant::now())
    }

    #[cfg(test)]
    pub fn accept_for_test(
        &mut self,
        answered: &TaskQueryResponse,
        today: NaiveDate,
        now_ms: i64,
    ) -> bool {
        self.apply(Ok(fold(answered, today, now_ms)), &[], Instant::now())
    }

    /// An answer to a request that carried `carried`, arriving at `now` — the
    /// seam the wait's panel-level tests drive time through.
    #[cfg(test)]
    pub fn accept_carrying_for_test(
        &mut self,
        answered: &TaskQueryResponse,
        carried: &[WriteRequirement],
        today: NaiveDate,
        now_ms: i64,
        now: Instant,
    ) -> bool {
        self.apply(Ok(fold(answered, today, now_ms)), carried, now)
    }

    /// Start one refresh. Answers whether the *drawn* state changed, which for
    /// a start is only ever "we could not spawn the thread".
    fn start(&mut self) -> bool {
        let (Some(workspace), Some(target)) = (self.workspace, self.target) else {
            return false;
        };
        let Some(client) = self.client.clone() else {
            // Not a failure worth a sentence on the page: a window with no
            // store shows a list with no rows, which is what `known: false`
            // already says.
            return false;
        };
        let generation = self.generation;
        let filters = self.filters;
        let carried = self.wait.carrying();
        self.wait.asked();
        let sent = carried.clone();
        let (send, receive) = mpsc::channel();
        match std::thread::Builder::new()
            .name("koru-task-page".into())
            .spawn(move || {
                let answer = run(&client, workspace, target, filters, &sent);
                let _ = send.send(Stamped {
                    workspace,
                    target,
                    generation,
                    carried,
                    answer,
                });
            }) {
            Ok(_) => {
                self.receiver = Some(receive);
                false
            }
            Err(error) => {
                self.notice = Some(format!("Could not list tasks: {error}"));
                true
            }
        }
    }
}

/// The worker: one query, then the rows.
fn run(
    client: &koru_api_client::Client,
    workspace: WorkspaceId,
    target: RailTarget,
    filters: Filters,
    covering: &[WriteRequirement],
) -> Result<Answer, String> {
    let (today, now_ms, windows) = windows_now();
    let store_workspace = koru_store_core::ids::WorkspaceId::from_uuid(workspace.as_uuid());
    let mut query = page_query(store_workspace, target, &windows, filters, today);
    query.covering = covering.iter().map(WriteRequirement::to_wire).collect();
    let answered = client
        .store()
        .query_tasks(&query)
        .map_err(|e| e.to_string())?;
    Ok(fold(&answered, today, now_ms))
}

/// Turn one answer into rows.
///
/// Separate from [`run`] and public to the module's tests so that *what a row
/// means* is asserted against a fabricated response rather than against a
/// store.
pub fn fold(answered: &TaskQueryResponse, today: NaiveDate, now_ms: i64) -> Answer {
    Answer {
        rows: answered
            .rows
            .iter()
            .filter_map(|row| TaskEntry::of(row, today, now_ms))
            .collect(),
        updating: !answered.updating_workspaces.is_empty() || !answered.projected,
        day: today,
        covered: answered.covered,
    }
}

#[cfg(test)]
mod tests;
