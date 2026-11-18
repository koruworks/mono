// SPDX-License-Identifier: MPL-2.0

//! What the tasks rail's numbers come from.
//!
//! The rail draws five bucket counts, a per-list count and the overdue share of
//! each. None of that is knowable from the open document: a task is a marked
//! node anywhere in the workspace, and the answer is the store's read model. So
//! this is the adapter - **one refresh, two statements, on a worker thread,
//! never on the event thread** - with the answer stamped so a stale one is
//! discarded rather than drawn.
//!
//! It is `HistoryPanel`'s shape, deliberately: a cached client, a generation, a
//! one-shot thread per unit of work, and a deadline that is `None` while the
//! surface is hidden - so a window whose sidebar is showing documents makes no
//! task traffic at all.
//!
//! ## Two statements, and neither of them counts a page
//!
//! `group_by: "bucket"` answers five corpus-wide rows - one per agenda bucket,
//! with the overdue share of each - and `group_by: "list"` answers one per list.
//! Both are counts over the whole matching set rather than over the page, so
//! **both queries ask for one row and read none of it**: `limit: 1` is not a
//! sample, it is a statement that the rows are not what was wanted.
//!
//! **This used to fold the buckets here, and it was wrong twice.** The open page
//! was fetched at `MAX_LIMIT` and bucketed client-side, so every count was a
//! *floor* past five hundred tasks (`capped`, drawn `500+`); and the local copy
//! of the rule had no thirty-day horizon, so every dated task past next week
//! counted as *Upcoming* and *Later* held nothing but undated work. R-1b put
//! bucketing in the store - where `bucket_placement_agrees_with_the_domain_function`
//! holds it equal to `koru_core::tasks::bucket` over a five-thousand-task
//! corpus - and this asks for it instead. `capped` is now always `false`, and
//! stays in [`TaskCounts`] because it is the honest thing to say the day a
//! count comes off a page again.
//!
//! The windows the store compares against are the reader's, computed once in
//! [`crate::tasks_view::query::windows_now`] and shared with the Tasks tab - so
//! *today* means one thing in the window rather than two.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use crate::panes::explore::rail::TaskCounts;
use crate::tasks_view::echo::{CountDelta, adjusted};
use crate::tasks_view::query::{Wait, WaitOutcome, WriteRequirement, note_wait};
use chrono::NaiveDate;
use koru_core::WorkspaceId;
use koru_store_wire::tasks::{BucketWindows, LifecycleFilter, TaskQuery, TaskQueryResponse};

/// How often the numbers are asked for again while the rail is on screen
/// **and nothing is telling it when to**.
///
/// A task completed in this window refreshes it immediately through
/// [`TaskCountsPanel::invalidate`], and so does the Store's own
/// `projection.tasks.changed` for everything else — a peer's edit, a
/// document imported in another window (`mainview::task_events`). This
/// interval is what is left for a Store that publishes no events.
const REFRESH_EVERY: Duration = Duration::from_secs(30);

/// The corpus-wide count and overdue share of one list, keyed by its stored id.
type Groups = HashMap<String, (usize, usize)>;

#[derive(Debug)]
enum Answer {
    /// The five numbers, the list groups, and whether **both** statements
    /// included every write the request named — `None` when the store cannot
    /// say. Two questions, one verdict: a current second answer cannot
    /// certify the first.
    Counts(TaskCounts, Groups, Option<bool>),
    Failed(String),
}

#[derive(Debug)]
struct Stamped {
    workspace: WorkspaceId,
    generation: u64,
    /// What the two requests carried, acknowledged against exactly this.
    carried: Vec<WriteRequirement>,
    answer: Answer,
}

#[derive(Default)]
pub struct TaskCountsPanel {
    client: Option<koru_api_client::Client>,
    workspace: Option<WorkspaceId>,
    visible: bool,
    /// Whether the Store's task events are being heard; see
    /// [`REFRESH_WHEN_TOLD`](crate::tasks_view::query::REFRESH_WHEN_TOLD).
    told: bool,
    generation: u64,
    receiver: Option<Receiver<Stamped>>,
    next_refresh: Option<Instant>,
    counts: TaskCounts,
    groups: Groups,
    /// The last failure, so a rail that cannot count says nothing rather than
    /// showing a stale number as though it were current.
    notice: Option<String>,
    /// The writes the numbers are waiting for the read model to include, and
    /// the policy that stops waiting — the page's, shared.
    wait: Wait,
    /// What this window's own writes did to the numbers, added to the store's
    /// answer until the wait ends and the answer has them itself.
    ///
    /// Each carries **when it was made**, because the wait it is paired with is
    /// not always armed: a write the store was never told to wait for (an
    /// unpublished one, or a receipt naming no addressable workspace) leaves
    /// nothing to end, and without a ceiling such a delta would be added to the
    /// rail's numbers for the rest of the session. The page's echoes are
    /// bounded the same way, by `TasksView::catch_up`.
    pending: Vec<PendingDelta>,
}

/// One optimistic count adjustment, and when it was made.
#[derive(Clone, Debug)]
struct PendingDelta {
    delta: CountDelta,
    since: Instant,
}

impl TaskCountsPanel {
    /// Cache the store client supplied during drawing. **No request is made
    /// here** — the maintenance hook owns every piece of background work, and
    /// a view with no client (every unit test) makes none at all.
    /// Say whether the Store's task events are being heard: the timer is the
    /// way the numbers learn anything only while they are not.
    pub fn set_event_driven(&mut self, told: bool) {
        if self.told == told {
            return;
        }
        self.told = told;
        if !told && let Some(due) = self.next_refresh {
            self.next_refresh = Some(due.min(Instant::now() + REFRESH_EVERY));
        }
    }

    fn refresh_every(&self) -> Duration {
        if self.told {
            crate::tasks_view::query::REFRESH_WHEN_TOLD
        } else {
            REFRESH_EVERY
        }
    }

    pub fn set_client(&mut self, client: Option<koru_api_client::Client>) {
        self.client = client;
    }

    /// The numbers as the rail should draw them: the store's answer, plus
    /// what this window has written since and the store has not counted yet.
    pub fn counts(&self) -> TaskCounts {
        adjusted(&self.counts, self.live_deltas())
    }

    /// One list's `(count, overdue)`. **A missing group is a zero**, never an
    /// unknown: the store omits a list nothing matches rather than sending it
    /// as `0`, and the rail is drawn from the list table either way. Adjusted
    /// by this window's own writes, as [`counts`](Self::counts) is.
    pub fn group(&self, list: koru_core::ids::ListId) -> (usize, usize) {
        let (mut count, mut overdue) = self
            .groups
            .get(&list.to_storage())
            .copied()
            .map(|(c, o)| (c as i64, o as i64))
            .unwrap_or((0, 0));
        for delta in self.live_deltas() {
            for (id, by, late) in &delta.lists {
                if *id == list {
                    count += *by as i64;
                    overdue += *late as i64;
                }
            }
        }
        (count.max(0) as usize, overdue.max(0) as usize)
    }

    /// **A write this window made, before the store has counted it.** Added to
    /// every number drawn until the wait the write started ends. Answers
    /// whether the numbers moved.
    pub fn expect(&mut self, delta: CountDelta) -> bool {
        if delta.is_empty() {
            return false;
        }
        // Deltas past the ceiling stop counting the moment they pass it (see
        // `live_deltas`), but only a spent wait ever emptied the list — and a
        // write the store was never told to wait for has no wait to spend. So
        // the list is pruned here, where it grows: at most one expired entry
        // outlives the next write, whatever the session does.
        let ceiling = crate::tasks_view::query::wait_ceiling() + Duration::from_secs(1);
        self.pending.retain(|p| p.since.elapsed() < ceiling);
        crate::tasks::debug_line(format_args!(
            "rail: delta expected ({} standing)",
            self.pending.len() + 1
        ));
        self.pending.push(PendingDelta {
            delta,
            since: Instant::now(),
        });
        true
    }

    /// The deltas still worth adding, oldest expired ones dropped.
    ///
    /// **Read at every use rather than swept on a tick**, because the numbers
    /// are drawn far more often than they are refreshed and a delta that has
    /// outlived its ceiling must stop counting the moment it does, not at the
    /// next answer — which for a write nothing is waiting on may never come.
    fn live_deltas(&self) -> Vec<&CountDelta> {
        let ceiling = crate::tasks_view::query::wait_ceiling() + Duration::from_secs(1);
        self.pending
            .iter()
            .filter(|p| p.since.elapsed() < ceiling)
            .map(|p| &p.delta)
            .collect()
    }

    #[cfg(test)]
    pub fn pending_for_test(&self) -> usize {
        self.pending.len()
    }

    /// Ask again on the next maintenance turn — what a task mutation in this
    /// window calls, so completing something moves the number now rather than
    /// in half a minute.
    pub fn invalidate(&mut self) {
        self.invalidate_at(Instant::now());
    }

    fn invalidate_at(&mut self, now: Instant) {
        if self.visible && self.workspace.is_some() {
            self.next_refresh = Some(now);
        }
    }

    /// Ask again, **and keep asking until the answer includes this write** —
    /// the page's [`require`](crate::tasks_view::query::TaskListPanel::require),
    /// for the numbers.
    pub fn require(&mut self, requirement: WriteRequirement) {
        self.require_at(requirement, Instant::now());
    }

    fn require_at(&mut self, requirement: WriteRequirement, now: Instant) {
        match self.wait.require(requirement, now) {
            Some(ask_at) => {
                if self.visible && self.workspace.is_some() {
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

    /// An answer to two requests that carried `carried`, arriving at `now`.
    #[cfg(test)]
    pub fn accept_carrying_for_test(
        &mut self,
        buckets: &TaskQueryResponse,
        lists: &TaskQueryResponse,
        carried: &[WriteRequirement],
        now: Instant,
    ) -> bool {
        self.apply(
            Answer::Counts(
                counts_of(buckets),
                groups_of(lists),
                both_covered(buckets, lists),
            ),
            carried,
            now,
        )
    }

    /// The only clock this panel contributes to the event loop. A hidden rail
    /// has no deadline and therefore no background traffic.
    pub fn deadline(&self) -> Option<Instant> {
        (self.visible && self.workspace.is_some())
            .then_some(self.next_refresh)
            .flatten()
    }

    /// Answers whether anything the rail draws changed.
    pub fn maintain(
        &mut self,
        workspace: Option<WorkspaceId>,
        visible: bool,
        now: Instant,
    ) -> bool {
        let mut changed = false;

        if workspace != self.workspace {
            self.workspace = workspace;
            self.generation = self.generation.wrapping_add(1);
            self.counts = TaskCounts::default();
            self.groups.clear();
            self.notice = None;
            self.wait.clear();
            self.wait.reset_support();
            self.pending.clear();
            self.next_refresh = (visible && workspace.is_some()).then_some(now);
            changed = true;
        }

        if visible != self.visible {
            self.visible = visible;
            self.next_refresh = (visible && self.workspace.is_some()).then_some(now);
            if !visible {
                self.wait.clear();
            }
        }

        // **The rollover is not detected here** — see
        // [`crate::tasks_view::query::DayWatch`], which is the one place it is,
        // and which invalidates this panel through [`Self::invalidate`]. The
        // copy that used to live here could not fire until the first answer had
        // set the day it kept, and read the clock a second time on every tick.

        if let Some(receiver) = &self.receiver
            && let Ok(stamped) = receiver.try_recv()
        {
            self.receiver = None;
            let current =
                Some(stamped.workspace) == self.workspace && stamped.generation == self.generation;
            if current {
                changed |= self.apply(stamped.answer, &stamped.carried, now);
            }
        }

        if self.visible
            && self.receiver.is_none()
            && self.next_refresh.is_some_and(|due| due <= now)
        {
            self.next_refresh = Some(now + self.refresh_every());
            changed |= self.start();
        }
        changed
    }

    fn apply(&mut self, answer: Answer, carried: &[WriteRequirement], now: Instant) -> bool {
        match answer {
            Answer::Counts(counts, groups, covered) => {
                let changed =
                    counts != self.counts || groups != self.groups || self.notice.is_some();
                self.counts = counts;
                self.groups = groups;
                self.notice = None;
                let outcome = self.wait.acknowledge(carried, covered, now);
                note_wait("rail", &outcome);
                if let WaitOutcome::Retry(at) = outcome {
                    self.next_refresh = Some(at);
                }
                // The deltas were the store's answer in advance; once the wait
                // **ends**, the answer is the answer.
                //
                // A wait that ended, not the mere absence of one — the page's
                // echoes had the same hole and it is the same fix. `Idle` means
                // there was nothing to wait for, which is ordinary: a write the
                // store was never told about arms no requirement, and the very
                // next answer is necessarily one taken before it. Clearing
                // there put the rail's numbers back to what they were, so a
                // count moved and then moved back.
                let spent = matches!(
                    outcome,
                    WaitOutcome::Covered { .. }
                        | WaitOutcome::GaveUp { .. }
                        | WaitOutcome::Unsupported
                );
                let before = self.pending.len();
                if spent {
                    self.pending.clear();
                }
                let live = self.live_deltas().len();
                if before > 0 || spent {
                    // **The rail's half of the same story.** A delta count
                    // falling to zero on an answer the write cannot be in is
                    // the number going back — which reads as a click that did
                    // not register.
                    crate::tasks::debug_line(format_args!(
                        "rail: deltas {before} -> {} (spent={spent}, {} still live) \
                         today={} next={}",
                        self.pending.len(),
                        live,
                        self.counts.today,
                        self.counts.next,
                    ));
                }
                changed
            }
            Answer::Failed(error) => {
                // **A failure does not spend the deltas.** The page's half of
                // this is in `TaskListPanel::apply`, and for the same reason:
                // discarding what this window knows because the transport
                // could not ask puts the numbers back to what they were, which
                // reads as a click that did not register. The wait's deadline
                // and the deltas' own ceiling still bound both.
                if self.wait.is_waiting() {
                    crate::tasks::debug_line(format_args!(
                        "rail: query failed while waiting; the write is still owed: {error}"
                    ));
                } else {
                    crate::tasks::debug_line(format_args!("rail: query failed: {error}"));
                }
                let changed = self.notice.as_deref() != Some(error.as_str());
                self.notice = Some(error);
                changed
            }
        }
    }

    /// Start one refresh. Answers whether the *drawn* state changed, which for
    /// a start is only ever "we have no client and have not said so yet".
    fn start(&mut self) -> bool {
        let Some(workspace) = self.workspace else {
            return false;
        };
        let Some(client) = self.client.clone() else {
            // Not a failure worth a sentence in the column: a window with no
            // store shows a rail with no numbers, which is what `known: false`
            // already says.
            return false;
        };
        let generation = self.generation;
        let carried = self.wait.carrying();
        self.wait.asked();
        let sent = carried.clone();
        let (send, receive) = mpsc::channel();
        match std::thread::Builder::new()
            .name("koru-task-counts".into())
            .spawn(move || {
                let answer = run(&client, workspace, &sent).unwrap_or_else(Answer::Failed);
                let _ = send.send(Stamped {
                    workspace,
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
                self.notice = Some(format!("Could not count tasks: {error}"));
                true
            }
        }
    }
}

/// The worker: two statements of counts, and no page at all.
fn run(
    client: &koru_api_client::Client,
    workspace: WorkspaceId,
    covering: &[WriteRequirement],
) -> Result<Answer, String> {
    let (today, now_ms, windows) = crate::tasks_view::query::windows_now();
    let store_workspace = koru_store_core::ids::WorkspaceId::from_uuid(workspace.as_uuid());
    let covering: Vec<_> = covering.iter().map(WriteRequirement::to_wire).collect();
    let mut buckets_query = bucket_query(store_workspace, &windows, today);
    buckets_query.covering = covering.clone();
    let buckets = client
        .store()
        .query_tasks(&buckets_query)
        .map_err(|e| e.to_string())?;
    let mut lists_query = list_query(store_workspace, today, now_ms);
    lists_query.covering = covering;
    let lists = client
        .store()
        .query_tasks(&lists_query)
        .map_err(|e| e.to_string())?;
    Ok(Answer::Counts(
        counts_of(&buckets),
        groups_of(&lists),
        both_covered(&buckets, &lists),
    ))
}

/// **Both statements count.** The rail is two questions, and a current answer
/// to the second cannot certify the first; a store that cannot say for either
/// cannot say for the rail.
pub fn both_covered(buckets: &TaskQueryResponse, lists: &TaskQueryResponse) -> Option<bool> {
    match (buckets.covered, lists.covered) {
        (Some(a), Some(b)) => Some(a && b),
        _ => None,
    }
}

/// The five bucket counts, corpus-wide.
///
/// **No `lifecycle`**, and that is not an omission: grouping by bucket
/// partitions by lifecycle - `Completed` is one of the five - so the store does
/// not apply the field, and naming it would say something that is then ignored.
pub fn bucket_query(
    workspace: koru_store_core::ids::WorkspaceId,
    windows: &BucketWindows,
    today: NaiveDate,
) -> TaskQuery {
    TaskQuery {
        workspaces: vec![workspace],
        group_by: Some("bucket".to_owned()),
        bucket_windows: Some(windows.clone()),
        // **The window the page is drawn under**, so the rail's Completed
        // number counts the rows the page would list.
        completed_on_or_after: Some(crate::tasks_view::query::completed_since(today)),
        // One row, and it is not read. The groups are corpus-wide; asking for a
        // page as well would be a page nobody looks at.
        limit: 1,
        ..TaskQuery::default()
    }
}

/// The per-list counts, over open tasks.
///
/// Overdue is *the reader's* definition and the store has no timezone database,
/// so the windows are stated the way `TaskQuery::overdue` asks: dates before
/// today, instants before now.
pub fn list_query(
    workspace: koru_store_core::ids::WorkspaceId,
    today: NaiveDate,
    now_ms: i64,
) -> TaskQuery {
    TaskQuery {
        workspaces: vec![workspace],
        lifecycle: LifecycleFilter::Open,
        group_by: Some("list".to_owned()),
        overdue: Some(koru_store_wire::attributes::DueFilter {
            dates: Some(koru_store_wire::attributes::DateWindow {
                after: None,
                before: Some(today.format("%Y-%m-%d").to_string()),
            }),
            instants: Some(koru_store_wire::attributes::InstantWindow {
                after_ms: None,
                before_ms: Some(now_ms),
            }),
        }),
        limit: 1,
        ..TaskQuery::default()
    }
}

/// The rail's numbers, read off the bucket groups.
///
/// **A missing group is a zero**, never an unknown: the store omits a bucket
/// nothing matches rather than sending it as `0`, and the rail draws all five
/// rows either way. `capped` is `false` by construction - a corpus-wide count
/// is not a page.
///
/// Public to the module's tests so that *what the numbers mean* is asserted
/// against a fabricated response rather than against a store.
pub fn counts_of(answered: &TaskQueryResponse) -> TaskCounts {
    let mut counts = TaskCounts {
        known: true,
        capped: false,
        ..TaskCounts::default()
    };
    for group in &answered.groups {
        match group.list.as_str() {
            "today" => {
                counts.today = group.count;
                // The one warm number in the column, and the store's own split
                // rather than a second pass over rows nobody fetched.
                counts.today_overdue = group.overdue;
            }
            "next" => counts.next = group.count,
            "upcoming" => counts.upcoming = group.count,
            "later" => counts.later = group.count,
            "completed" => counts.completed = group.count,
            // A bucket token a newer store knows and this build does not.
            // Counted nowhere rather than folded into a neighbour: a number in
            // the wrong queue is worse than a number that is missing.
            _ => {}
        }
    }
    counts
}

/// The per-list `(count, overdue)` table, keyed by stored list id.
pub fn groups_of(answered: &TaskQueryResponse) -> Groups {
    answered
        .groups
        .iter()
        .map(|group| (group.list.clone(), (group.count, group.overdue)))
        .collect()
}

#[cfg(test)]
mod tests;
