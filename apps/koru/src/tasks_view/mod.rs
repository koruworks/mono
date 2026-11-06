// SPDX-License-Identifier: MPL-2.0

//! The Tasks tab: one bucket or one list of the workspace's tasks, as a page.
//!
//! Everything task-list-shaped lives under this directory except the two pieces
//! that genuinely belong to their hosts — the rail in the explore column
//! (`panes::explore::rail`, which owns that column's geometry) and the drains in
//! `mainview/tasks.rs` (which need a `&mut Workspace` a view never holds).
//!
//! A **tab**, on the Trash tab's precedent: it survives a restart, it can sit
//! beside a document, and it is a list of rows — which is a shape nothing else
//! in the window has room for. Built on [`koru_ui::table`], the same component
//! the Trash tab and the workspace manager use, so the three screens are one
//! table with different cells rather than three sets of six numbers that drift.
//!
//! ## Where the rows come from
//!
//! **The store, never a document walk.** A task is a marked node anywhere in
//! the workspace ([ADR-285]), so nothing this window holds open can enumerate
//! one; `query.rs` asks the tasks query and nothing else. The one thing the
//! answer cannot carry — SPEC §5's *completed but still holding unfinished
//! work* — is computed from the open document for the rows that live in it, and
//! left `false` for the rows that do not. See
//! [`TasksView::catch_up`](view::TasksView::catch_up).
//!
//! ## What narrows it
//!
//! **The chips row and the overflow menu move fields of the query and nothing
//! else** — `min_priority`, `lifecycle`, `due`, `include_trashed`, `sort`. There
//! is no filtering after the answer arrives, because a page is a page: a chip
//! that dropped rows the store had already counted would disagree with its own
//! header the moment the list was longer than the limit. The chips carry the
//! dimensions a person changes while reading; the overflow carries the three
//! that change what the query *returns*, and that is the whole rule for what may
//! ever be added to it ([`menu::OverflowRow`]).
//!
//! ## A list is another bucket, not a new surface
//!
//! `RailTarget::List` is one more value of the same target, so a list page is
//! this page: the same header band (with the list's own colour square and
//! name), the same chips, the same rows. What it changes is one predicate on
//! the query — SPEC §8's *a list is a filter, not a place* — and two things
//! that follow from that: the lifecycle chip is offered here, because a bucket
//! names its own lifecycle and a list does not; and the due dimension is drawn
//! as one selector reading *Any date*, because a list spans all time and
//! dateless rows are normal in it rather than something to schedule.
//!
//! **Membership shows on the row as one more chip in the existing family**, so
//! a task can be in Today *and* in Bike Project without a second vocabulary —
//! and it is set through the view's own chooser (`menu::ListPick`), because
//! there is no editor picker for `koru.list` to go to.
//!
//! ## Which bucket, and who says so
//!
//! One answer, held in two places that are kept equal by construction: the rail
//! records the selection when it is pressed, the container reads it with
//! `MainView::tasks_selection`, and the tab is opened on it. A tab restored from
//! the session pushes its own target back with
//! `ExploreView::rail_mut().set_selected(..)`, so a window that reopens on
//! Upcoming has an Upcoming rail.
//!
//! [ADR-285]: ../../../docs/decisions/ADR-276-300.md#adr-285-taskmodel--a-task-is-a-marked-node-and-the-marker-or-a-due-date-is-the-whole-derivation

pub mod accessibility;
mod capture;
mod draw;
pub mod echo;
mod heading;
mod menu;
mod pickers;
pub mod query;
mod rank;
mod row;
mod states;
mod view;

pub use menu::{row_menu, rows_menu};
pub use query::TaskEntry;
pub use view::{ListFace, Request, TasksView, due_sentence};

// The pure rules the suite drives directly: how a typed line splits into a
// title and a date word, and which rule-label a dated row falls under. Public
// rather than private because a rule nothing outside can name is a rule nothing
// outside can check.
#[cfg(test)]
pub use view::{Rule, split_date};

#[cfg(test)]
mod tests;

/// P-Base's client-side baselines. `#[ignore]`d as a set; see the module's own
/// header for the command and for why they are not in the ordinary suite.
#[cfg(test)]
mod perf;
