// SPDX-License-Identifier: MPL-2.0

//! **What the page shows before the store has caught up.**
//!
//! A write made from a Tasks surface is a write this window made. The read
//! model that answers the page is fed asynchronously and holds a document
//! half a second after its last commit before projecting it, so the honest
//! answer to *has the store got it yet* is no for about a second after every
//! tick, capture or date — and a row that ticks a second after the click feels
//! broken, whatever the log says. FRESHNESS made the surfaces *wait* for the
//! read model; this makes them *say* what they already know while they wait.
//!
//! An echo is one change applied over the rows the store last answered with:
//! a row the capture row made, a tick, a date, a level, a membership, a
//! removal. It is re-applied every time the page takes the store's rows —
//! which are behind it — and it is dropped when the wait the write started
//! ends, because the answer that ended it includes the write. **Never a
//! second source of truth**: an echo has no life of its own past the bounded
//! wait, and a failed command takes its echo away with the refusal.
//!
//! The rail's numbers get the same treatment one level down: a
//! [`CountDelta`] the container computes from the row and the request, held by
//! the counts panel until its own wait ends, and added to the store's answer
//! on the way to the rail. Both are the arithmetic the store will do, done
//! first; `bucket` is asked, never re-derived.

use std::time::Instant;

use chrono::NaiveDate;
use koru_core::attributes::Priority;
use koru_core::ids::ListId;
use koru_core::time::{DateOnly, Temporal};
use koru_core::{DocumentId, NodeId, WorkspaceId};

use koru_store_wire::tasks::LifecycleFilter;

use super::query::{Due, RowKind, TaskEntry, is_overdue};
use crate::panes::explore::rail::{Bucket, RailTarget};
use koru_workspace_host::tasks::TaskCommand;

/// One change this window made, shown before the store answers with it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Echo {
    pub document: DocumentId,
    pub node: NodeId,
    pub patch: Patch,
    /// Whether the command has landed — the receipt arrived, the wait began.
    /// Until then the echo is a promise, dropped if the command is refused.
    pub armed: bool,
    pub since: Instant,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Patch {
    /// A row the capture row made. Its ids are placeholders until the receipt
    /// names the real ones.
    Created(TaskEntry),
    Completed(bool),
    Due(Option<Due>),
    Priority(Option<String>),
    /// The line's colour token; `None` cleared.
    Color(Option<String>),
    /// Its place in the list's dragged order; `None` cleared.
    Rank(Option<i64>),
    /// The row's words, renamed on the page.
    Title(String),
    List(Option<ListId>),
    Removed,
    /// *Create document*: the row is the same task with a home. `from` is the
    /// page-made ids the store still answers with until it catches up — hidden
    /// — and `row` is the row as it will be, carrying the new page's name as
    /// its context and, from the receipt on, the new ids.
    Homed {
        from: (DocumentId, NodeId),
        row: TaskEntry,
    },
}

/// A stored due, in the two shapes a row compares — the same fold the wire's
/// row takes on the way in.
pub(super) fn due_of(temporal: &Temporal) -> Due {
    match temporal {
        Temporal::Date(d) => Due::Day(d.date()),
        Temporal::Floating(f) => Due::Day(f.date().date()),
        Temporal::Zoned(z) => Due::Instant(z.instant_ms()),
    }
}

/// The row a capture will become, with placeholder ids the receipt replaces.
pub(super) fn provisional_entry(
    workspace: WorkspaceId,
    document: DocumentId,
    title: &str,
    due: Option<&Temporal>,
    list: Option<ListId>,
    priority: Option<koru_core::attributes::Priority>,
    today: NaiveDate,
    now_ms: i64,
) -> TaskEntry {
    let due = due.map(due_of);
    let (icon, rest) = koru_shell_chrome::split_leading_emoji(title);
    let title = if rest.trim().is_empty() {
        "Untitled".to_owned()
    } else {
        rest.trim().to_owned()
    };
    TaskEntry {
        workspace,
        document,
        node: NodeId::new(),
        task: String::new(),
        kind: RowKind::Text,
        mime: None,
        title,
        icon: icon.map(str::to_owned),
        breadcrumb: String::new(),
        due,
        overdue: due.is_some_and(|due| is_overdue(due, today, now_ms)),
        priority: priority.map(|p| p.as_str().to_owned()),
        list,
        // The capture row sets no colour, and its rank is set by the page.
        color: None,
        rank: None,
        completed: false,
        // A row typed into the capture field is not pinned: pinning is a
        // gesture on a task that exists, and the capture row makes one.
        pinned_today: false,
        trashed: false,
        residue: false,
    }
}

/// **The queue a row is in, asked of the domain function** — the rail's enum,
/// from SPEC §7's rule and nothing written here.
pub fn entry_bucket(row: &TaskEntry, today: NaiveDate) -> Bucket {
    let due = row.due_day().map(|day| Temporal::Date(DateOnly::new(day)));
    let lifecycle = if row.completed {
        koru_core::attributes::Lifecycle::Completed
    } else {
        koru_core::attributes::Lifecycle::Open
    };
    let priority = row.priority.as_deref().and_then(Priority::parse);
    match koru_core::tasks::bucket(
        due.as_ref(),
        lifecycle,
        priority,
        DateOnly::new(today),
        &koru_editor::utils::zone::SystemZone::detect(),
    ) {
        koru_core::tasks::Bucket::Today => Bucket::Today,
        koru_core::tasks::Bucket::Next => Bucket::Next,
        koru_core::tasks::Bucket::Upcoming => Bucket::Upcoming,
        koru_core::tasks::Bucket::Later => Bucket::Later,
        koru_core::tasks::Bucket::Completed => Bucket::Completed,
    }
}

/// Whether a row, after a patch, is still one this page shows — the page's
/// own predicate, applied to a row the store has not re-filtered yet.
pub(super) fn belongs(
    row: &TaskEntry,
    target: RailTarget,
    lifecycle: LifecycleFilter,
    today: NaiveDate,
) -> bool {
    match target {
        RailTarget::Bucket(Bucket::Completed) => row.completed,
        RailTarget::Bucket(bucket) => {
            let shown = match lifecycle {
                LifecycleFilter::Open => !row.completed,
                LifecycleFilter::Completed => row.completed,
                LifecycleFilter::All => true,
            };
            // A queue names a lifecycle: a completed row on an open queue is
            // gone unless the page is showing completed inline.
            shown && (row.completed || entry_bucket(row, today) == bucket)
        }
        RailTarget::List(id) => {
            let shown = match lifecycle {
                LifecycleFilter::Open => !row.completed,
                LifecycleFilter::Completed => row.completed,
                LifecycleFilter::All => true,
            };
            shown && row.list == Some(id)
        }
        RailTarget::Reminders => unreachable!(
            "the rail's Reminders row swaps the tab's view; a Tasks page never targets it"
        ),
    }
}

/// Whether a row a write may have moved is still in this page's queue.
///
/// **A write that did not move the queue cannot take the row off the page.**
/// The store sent the row, and the store's filter is the authority on why; a
/// row leaves only when the patch moved it from one queue to another and the
/// other is not this page's.
fn stays_in_queue(row: &TaskEntry, before: Bucket, target: RailTarget, today: NaiveDate) -> bool {
    match target {
        RailTarget::Bucket(Bucket::Completed) | RailTarget::List(_) => true,
        RailTarget::Bucket(bucket) => {
            let after = entry_bucket(row, today);
            after == before || after == bucket
        }
        RailTarget::Reminders => unreachable!(
            "the rail's Reminders row swaps the tab's view; a Tasks page never targets it"
        ),
    }
}

/// Whether a row's lifecycle is one the page shows.
fn lifecycle_shown(row: &TaskEntry, target: RailTarget, lifecycle: LifecycleFilter) -> bool {
    match target {
        RailTarget::Bucket(Bucket::Completed) => row.completed,
        _ => match lifecycle {
            LifecycleFilter::Open => !row.completed,
            LifecycleFilter::Completed => row.completed,
            LifecycleFilter::All => true,
        },
    }
}

/// Apply one patch to a row. Answers whether the row still belongs on the
/// page afterwards.
///
/// **Only the part of the page's predicate the patch could have changed is
/// asked again.** The store's own filter sent the row, and a row it sent
/// belongs until a write moves it: a membership change cannot move a row
/// between queues, and a date cannot take a row out of a list.
pub(super) fn apply_patch(
    row: &mut TaskEntry,
    patch: &Patch,
    target: RailTarget,
    lifecycle: LifecycleFilter,
    today: NaiveDate,
    now_ms: i64,
) -> bool {
    match patch {
        Patch::Created(_) => belongs(row, target, lifecycle, today),
        Patch::Completed(done) => {
            row.completed = *done;
            row.overdue = !done && row.due.is_some_and(|d| is_overdue(d, today, now_ms));
            lifecycle_shown(row, target, lifecycle)
        }
        Patch::Due(due) => {
            let before = entry_bucket(row, today);
            row.due = *due;
            row.overdue = !row.completed && due.is_some_and(|d| is_overdue(d, today, now_ms));
            stays_in_queue(row, before, target, today)
        }
        Patch::Priority(level) => {
            let before = entry_bucket(row, today);
            row.priority = level.clone();
            stays_in_queue(row, before, target, today)
        }
        // A colour moves no row: it is not a queue's or a list's question.
        Patch::Color(color) => {
            row.color = color.clone();
            true
        }
        // Nor does a rank: it moves the row *within* the page, which the
        // page's own sort puts right after the echoes are drawn.
        Patch::Rank(rank) => {
            row.rank = *rank;
            true
        }
        // Nor do its words: the same row, renamed.
        Patch::Title(title) => {
            row.title = title.clone();
            true
        }
        Patch::List(list) => {
            row.list = *list;
            match target {
                RailTarget::List(id) => row.list == Some(id),
                RailTarget::Bucket(_) => true,
                RailTarget::Reminders => unreachable!(
                    "the rail's Reminders row swaps the tab's view; a Tasks page never targets it"
                ),
            }
        }
        Patch::Removed => false,
        Patch::Homed { .. } => belongs(row, target, lifecycle, today),
    }
}

// ------------------------------------------------------------ the rail

/// What one write does to the rail's numbers, before the store says so.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CountDelta {
    pub today: i32,
    pub today_overdue: i32,
    pub next: i32,
    pub upcoming: i32,
    pub later: i32,
    pub completed: i32,
    /// Per list: the open count and its overdue share.
    pub lists: Vec<(ListId, i32, i32)>,
}

impl CountDelta {
    fn bucket(&mut self, bucket: Bucket, by: i32, overdue: bool) {
        match bucket {
            Bucket::Today => {
                self.today += by;
                if overdue {
                    self.today_overdue += by;
                }
            }
            Bucket::Next => self.next += by,
            Bucket::Upcoming => self.upcoming += by,
            Bucket::Later => self.later += by,
            Bucket::Completed => self.completed += by,
        }
    }

    fn list(&mut self, list: Option<ListId>, by: i32, overdue: bool) {
        let Some(list) = list else { return };
        self.lists.push((list, by, if overdue { by } else { 0 }));
    }

    /// Take a row off the numbers, or put it on, in whichever queue and list
    /// it is in right now.
    fn row(&mut self, row: &TaskEntry, by: i32, today: NaiveDate) {
        self.bucket(entry_bucket(row, today), by, row.overdue);
        if !row.completed {
            self.list(row.list, by, row.overdue);
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The delta one command makes, from the row it acts on — computed where the
/// command is queued, whichever door it came in by, and applied when its
/// receipt lands. `None` for a command that changes no number, or acts on a
/// row this page is not showing.
pub fn delta_of_command(
    command: &TaskCommand,
    row: Option<&TaskEntry>,
    today: NaiveDate,
    now_ms: i64,
) -> Option<CountDelta> {
    let mut delta = CountDelta::default();
    match command {
        TaskCommand::Create {
            due,
            list,
            priority,
            ..
        } => {
            let row = provisional_entry(
                WorkspaceId::new(),
                DocumentId::new(),
                "",
                due.as_ref(),
                *list,
                *priority,
                today,
                now_ms,
            );
            delta.row(&row, 1, today);
        }
        // **The rail's numbers do not move for a pin.** A queue count is
        // computed from the row's *natural* bucket, and pinning does not change
        // that — it adds a membership the count arithmetic here cannot see,
        // because `bucket` is the one authority on placement and it is not
        // asked about pins. The Today number is corrected by the store's own
        // answer, one query later, rather than guessed at here.
        TaskCommand::PinToday { .. } => return None,
        TaskCommand::Complete { completed, .. } => {
            let row = row?;
            let mut after = row.clone();
            after.completed = *completed;
            after.overdue = !completed && after.due.is_some_and(|d| is_overdue(d, today, now_ms));
            delta.row(row, -1, today);
            delta.row(&after, 1, today);
        }
        TaskCommand::SetDue { due, .. } => {
            let row = row?;
            let mut after = row.clone();
            after.due = due.as_ref().map(due_of);
            after.overdue =
                !after.completed && after.due.is_some_and(|d| is_overdue(d, today, now_ms));
            delta.row(row, -1, today);
            delta.row(&after, 1, today);
        }
        TaskCommand::SetPriority { priority, .. } => {
            let row = row?;
            let mut after = row.clone();
            after.priority = priority.map(|p| p.as_str().to_owned());
            delta.row(row, -1, today);
            delta.row(&after, 1, today);
        }
        TaskCommand::SetList { list, .. } => {
            let row = row?;
            if !row.completed {
                delta.list(row.list, -1, row.overdue);
                delta.list(*list, 1, row.overdue);
            }
        }
        // The same task in the same queue: the numbers do not move.
        TaskCommand::CreateHome { .. }
        | TaskCommand::SetColor { .. }
        | TaskCommand::SetRank { .. }
        | TaskCommand::Retitle { .. } => {}
        TaskCommand::Remove { .. } => {
            delta.row(row?, -1, today);
        }
    }
    Some(delta)
}

/// Add a delta to the store's numbers on the way to the rail. Saturating at
/// zero: a number the store has not counted yet cannot go below nothing.
pub fn adjusted<'a>(
    counts: &crate::panes::explore::rail::TaskCounts,
    deltas: impl IntoIterator<Item = &'a CountDelta>,
) -> crate::panes::explore::rail::TaskCounts {
    let add = |base: usize, by: i32| (base as i64 + by as i64).max(0) as usize;
    let mut out = counts.clone();
    for delta in deltas {
        out.today = add(out.today, delta.today);
        out.today_overdue = add(out.today_overdue, delta.today_overdue);
        out.next = add(out.next, delta.next);
        out.upcoming = add(out.upcoming, delta.upcoming);
        out.later = add(out.later, delta.later);
        out.completed = add(out.completed, delta.completed);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panes::explore::rail::TaskCounts;
    use koru_store_wire::attributes::AttributeRowValue;
    use koru_store_wire::tasks::TaskRow;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()
    }

    fn now_ms() -> i64 {
        today()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
    }

    fn attr(role: &str, kind: &str, value: &str) -> AttributeRowValue {
        AttributeRowValue {
            id: format!("{role}-1"),
            role: role.to_owned(),
            kind: kind.to_owned(),
            value: value.to_owned(),
            zone: None,
            label: None,
        }
    }

    fn row(attributes: Vec<AttributeRowValue>) -> TaskEntry {
        let workspace = koru_store_core::ids::WorkspaceId::new();
        let document = koru_store_core::ids::DocumentId::new();
        let node = NodeId::new();
        TaskEntry::of(
            &TaskRow {
                workspace,
                document,
                node: node.to_storage(),
                task: String::new(),
                node_kind: "paragraph".to_owned(),
                lifecycle: "open".to_owned(),
                document_trashed: false,
                in_trash: false,
                breadcrumb: String::new(),
                title: "a task".to_owned(),
                attributes,
            },
            today(),
            now_ms(),
        )
        .unwrap()
    }

    fn on(day: &str) -> Temporal {
        Temporal::Date(DateOnly::parse(day).unwrap())
    }

    /// The queue a row is in is the domain's answer, not one written here.
    #[test]
    fn a_rows_queue_is_asked_of_the_domain_function() {
        assert_eq!(entry_bucket(&row(Vec::new()), today()), Bucket::Later);
        let dated = row(vec![attr("koru.due", "date", "2026-09-02")]);
        assert_eq!(entry_bucket(&dated, today()), Bucket::Next);
        let overdue = row(vec![attr("koru.due", "date", "2026-08-01")]);
        assert_eq!(entry_bucket(&overdue, today()), Bucket::Today);
        let urgent = row(vec![attr("koru.priority", "text", "high")]);
        assert_eq!(entry_bucket(&urgent, today()), Bucket::Next);
        let mut done = dated.clone();
        done.completed = true;
        assert_eq!(entry_bucket(&done, today()), Bucket::Completed);
    }

    /// A capture on Today is one more in Today; a tick on an overdue row is
    /// one fewer there, one fewer overdue, one more completed, and one fewer
    /// in its list.
    #[test]
    fn the_delta_of_a_capture_and_a_tick() {
        let capture = delta_of_command(
            &TaskCommand::Create {
                title: "Chase it".into(),
                due: Some(on("2026-08-31")),
                list: None,
                priority: None,
                rank: None,
            },
            None,
            today(),
            now_ms(),
        )
        .unwrap();
        assert_eq!((capture.today, capture.today_overdue), (1, 0));

        let list = ListId::new();
        let late = row(vec![
            attr("koru.due", "date", "2026-08-01"),
            attr("koru.list", "text", &list.to_storage()),
        ]);
        assert!(late.overdue);
        let tick = delta_of_command(
            &TaskCommand::Complete {
                document: late.document,
                node: late.node,
                completed: true,
            },
            Some(&late),
            today(),
            now_ms(),
        )
        .unwrap();
        assert_eq!(
            (tick.today, tick.today_overdue, tick.completed),
            (-1, -1, 1)
        );
        assert_eq!(tick.lists, vec![(list, -1, -1)]);
    }

    /// A date past the queue moves the row from one number to another; a
    /// list change moves it between groups; a request that changes no number
    /// makes no delta.
    #[test]
    fn the_delta_of_a_move() {
        let today_row = row(vec![attr("koru.due", "date", "2026-08-31")]);
        let moved = delta_of_command(
            &TaskCommand::SetDue {
                document: today_row.document,
                node: today_row.node,
                due: Some(on("2026-09-20")),
            },
            Some(&today_row),
            today(),
            now_ms(),
        )
        .unwrap();
        assert_eq!((moved.today, moved.upcoming), (-1, 1));

        let (a, b) = (ListId::new(), ListId::new());
        let in_a = row(vec![attr("koru.list", "text", &a.to_storage())]);
        let relisted = delta_of_command(
            &TaskCommand::SetList {
                document: in_a.document,
                node: in_a.node,
                list: Some(b),
            },
            Some(&in_a),
            today(),
            now_ms(),
        )
        .unwrap();
        assert_eq!(relisted.lists, vec![(a, -1, 0), (b, 1, 0)]);
        assert_eq!((relisted.today, relisted.later), (0, 0));

        assert!(
            delta_of_command(
                &TaskCommand::Complete {
                    document: in_a.document,
                    node: in_a.node,
                    completed: true,
                },
                None,
                today(),
                now_ms()
            )
            .is_none(),
            "a row this page does not show moves no number here"
        );
    }

    /// The numbers never go below nothing, whatever a delta says about a
    /// count the store has not made yet.
    #[test]
    fn adjusted_counts_saturate_at_zero() {
        let base = TaskCounts {
            known: true,
            today: 1,
            ..TaskCounts::default()
        };
        let out = adjusted(
            &base,
            &[CountDelta {
                today: -2,
                completed: 1,
                ..CountDelta::default()
            }],
        );
        assert_eq!((out.today, out.completed), (0, 1));
        assert!(out.known);
    }
}
