// SPDX-License-Identifier: MPL-2.0

//! The explore column's **tasks** mode: the rail.
//!
//! One sidebar, three modes. The icon row at the column's head switches
//! documents / files / tasks ([`NavigatorMode`]), and the column below becomes
//! that mode's index. In tasks mode the index is this: the four queues, a
//! Reminders row under Upcoming, the lists you made, and Completed under a
//! hairline. No groups, no saved filters, no Trash row — there is room for
//! them and we deliberately draw none of it.
//!
//! ## What is here and what is not
//!
//! This is chrome, so it holds *where things are* and never *what they mean*.
//! The rail is handed [`TaskCounts`], a `Vec<RailList>` and the bell's
//! attention count, and reports a [`TasksRailRequest`]; it opens nothing,
//! queries nothing and cannot see a workspace. Counting the tasks is the container's, and so is creating a list
//! — the inline field is drawn *over* the row this module lays out, exactly as
//! the workspace rename field is drawn over a switcher row, because
//! [`koru_ui::TextField`] state belongs with the surface that owns the
//! keyboard.
//!
//! ## The one geometry function
//!
//! [`TasksRail::rows`] is the only arithmetic in this file. Drawing,
//! hit-testing, the hover, the scroll clamp and the rect the container hangs
//! its field on all come from it, so a row cannot be clickable somewhere other
//! than where it is drawn (`DESIGN.md`'s first rule for chrome, pinned by
//! `every_rail_row_hit_tests_back_to_itself`).
//!
//! ## Urgency appears exactly once
//!
//! A count is quiet. The *overdue* share is the one warm thing in the column,
//! and it uses the red accent tone the swatch set already carries — no new
//! `Palette` field, which is the standing rule and the reason a fifth grey was
//! not invented for it either.

use std::collections::{HashMap, HashSet};

use koru_core::ids::ListId;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme;
use koru_draw::{icons, scene};
use vello::Scene;
use vello::kurbo::Affine;

use super::{FOOTER_PAD, SECTION_H, draw_chosen_row, icon_lane, row_chip};
use koru_shell_chrome::tabs::draw_icon;

/// SPEC §7's bucket, from the domain.
///
/// **Re-exported rather than declared.** This file used to hold a second enum
/// with the same five names, on the reasoning that the rail is where a bucket
/// is *drawn* and the domain is where one is *computed*. That held while
/// nothing carried an answer from one to the other. The queue icon does: the
/// document's chip asks [`koru_core::tasks::bucket`] which queue a due falls
/// in and has to turn that answer into a codepoint this file already maps its
/// own names to. Two enums meeting is one enum with two spellings, and the
/// module note in `tasks_view/query.rs` says what that costs.
///
/// What stays here is [`bucket_glyph`] and [`QUEUES`]: the codepoints
/// live where the icon set lives, and the domain crate knows nothing about
/// Phosphor and must not learn.
pub use koru_core::tasks::Bucket;

/// The four queues you work in, in the order they are drawn.
///
/// `Completed` is deliberately not among them — it is the row under the rule.
/// An inherent const on the domain enum would put a *drawing* order in the
/// domain; this is the rail's own list of what it draws above the hairline.
pub const QUEUES: [Bucket; 4] = [Bucket::Today, Bucket::Next, Bucket::Upcoming, Bucket::Later];

/// A bucket's word, as the rail and the Tasks page print it.
pub fn bucket_label(bucket: Bucket) -> &'static str {
    match bucket {
        Bucket::Today => "Today",
        Bucket::Next => "Next",
        Bucket::Upcoming => "Upcoming",
        Bucket::Later => "Later",
        Bucket::Completed => "Completed",
    }
}

/// The fixed Reminders destination in the reader's language. Shared by the
/// rail and its tab so the two cannot drift into different words.
pub fn reminders_label(lang: koru_i18n::Lang) -> &'static str {
    koru_i18n::t(lang, koru_i18n::Key::ReminderTitle)
}

/// A bucket's glyph — **the chip's table, not a second copy of it.**
///
/// It used to be declared here, and Next used to be [`icons::ARROW_UP`] turned
/// a quarter turn because the real right arrow had never been verified against
/// the pinned stylesheet. The chip is what forced both questions: it caches its
/// glyph by codepoint, so a rotation cannot ride along, and a rail table plus a
/// chip table is two answers about one picture that are free to disagree.
///
/// So the table moved to [`koru_editor::chips::bucket_glyph`] — the first
/// place above both `koru-draw` (which owns the codepoints and cannot see
/// `koru-core`) and `koru-core` (which owns the enum and must not learn about
/// Phosphor) — and this is the name the rail has always called it by.
pub use koru_editor::chips::bucket_glyph;

/// What the rail draws in its trailing lane, per bucket.
///
/// `known` is the whole cold state: before the first answer there is no number
/// at all, because a `0` that means *we have not looked yet* is a lie a reader
/// cannot see through. `capped` says the answer came off a page the store
/// clamped, and the count is therefore a floor — drawn `500+` rather than as a
/// number that is quietly wrong.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct TaskCounts {
    pub known: bool,
    pub capped: bool,
    pub today: usize,
    /// How many of [`today`](Self::today) are already past. The one warm number
    /// in the column.
    pub today_overdue: usize,
    pub next: usize,
    pub upcoming: usize,
    pub later: usize,
    pub completed: usize,
}

impl TaskCounts {
    fn of(&self, bucket: Bucket) -> usize {
        match bucket {
            Bucket::Today => self.today,
            Bucket::Next => self.next,
            Bucket::Upcoming => self.upcoming,
            Bucket::Later => self.later,
            Bucket::Completed => self.completed,
        }
    }
}

/// One row of the Lists section, as the rail knows it.
///
/// The name, the swatch, the order and **where the list sits in the tree**
/// come from the workspace's own list table (the system document's repaired
/// tree); only the counts come off the wire. Two spellings of one name in one
/// answer is a drift waiting to happen, which is why the store sends ids and
/// this carries the name.
///
/// The rail is pushed **every** live list, depth first, and decides for
/// itself which are showing: which rows are open is this device's business
/// (nested-lists D9), not the table's.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RailList {
    pub id: ListId,
    pub name: String,
    /// An `AccentColor` token — `"red"`, `"teal"` — never an RGB value. A
    /// colour chosen on one theme is read on the other.
    pub color: Option<String>,
    /// This list's own tasks. What a parent's row shows is decided when it
    /// is drawn (D5): everything inside while it is shut, nothing while open.
    pub count: usize,
    pub overdue: usize,
    /// The list this one is drawn under, after the tree's repair.
    pub parent: Option<ListId>,
    /// Levels below the top; `0` is a top-level list.
    pub depth: usize,
    pub has_children: bool,
}

impl RailList {
    /// A top-level list with nothing inside it: every list of a table that
    /// has never been nested.
    #[cfg(test)]
    pub fn flat(id: ListId, name: &str, color: Option<&str>, count: usize, overdue: usize) -> Self {
        Self {
            id,
            name: name.to_owned(),
            color: color.map(str::to_owned),
            count,
            overdue,
            parent: None,
            depth: 0,
            has_children: false,
        }
    }
}

/// Which row of the rail, in the order they are laid out.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum RailRow {
    Bucket(Bucket),
    /// The Reminders page's row, sitting under Upcoming. A [`RailTarget`]
    /// like a queue's: the page is a tab, so the selection card has a page
    /// to mean.
    Reminders,
    /// The "Lists" heading, which carries the `+`.
    ListsHeading,
    /// An index into the pushed list table. Only a row whose ancestors are
    /// all open is laid out.
    List(usize),
    /// The row a new list's name is being typed on: first under the heading
    /// for a top-level list, first under its parent for a child. There only
    /// while the field is up.
    Compose,
}

/// What the rail wants the container to do.
///
/// The same contract every other piece of chrome keeps: it reports the gesture
/// and performs none of it. `NewList` is the one variant the *container*
/// constructs — the inline field is its, because the field is a
/// [`koru_ui::TextField`] and this crate holds no keyboard — and it is in this
/// enum rather than beside it so that creating a list has one vocabulary from
/// the press on `+` to the row that appears.
///
/// **Managing a list keeps that arrangement.** [`RowMenu`](Self::RowMenu) is
/// a gesture on a row, and the rail knows where the rows are. Rename, recolour
/// and delete are the container's to construct — a menu, a field and a
/// confirmation are all things this crate cannot hold — and they are in this
/// enum for `NewList`'s reason, so that one list has one vocabulary from the
/// right-click to the row that changes.
///
/// **`PartialEq` and not `Eq`**, since [`RowMenu`](Self::RowMenu) carries a
/// point: the same reason [`crate::Outcome`], which wraps this, is not `Eq`
/// either.
#[derive(Clone, PartialEq, Debug)]
pub enum TasksRailRequest {
    /// `new_tab` is the disposition, carried rather than spelled as a second
    /// variant — `Request::Open` on the Tasks page took the same shape for the
    /// same reason. Ctrl+click and the row's *Open in new tab* are one ask.
    OpenBucket {
        bucket: Bucket,
        new_tab: bool,
    },
    OpenList {
        id: ListId,
        new_tab: bool,
    },
    /// The Reminders row: open the All Reminders page. Same disposition as a
    /// queue — a plain press re-points the Tasks tab, `ctrl` asks for a new
    /// one. Named for the row rather than the surface: the bell's compact
    /// popover is the *other* host, and this request never goes near it.
    OpenReminders {
        new_tab: bool,
    },
    /// Alt alone on a queue, a list or the Reminders row: ask where its page
    /// opens, with the place chooser at `at` (window coordinates) — the one
    /// a reference in a document shows (ADR-58). Nothing is selected and no
    /// drag is armed: the chooser is the answer.
    ChoosePlace {
        target: RailTarget,
        at: (f32, f32),
    },
    /// One or more list rows were dragged somewhere else: under `parent` (`None` is the
    /// top level), immediately before the sibling `before` (`None` is last).
    /// Nesting, un-nesting and reordering are this one request. The rows are
    /// already drawn that way here; what the container owes is the write that
    /// makes the table agree. **No task moves.**
    MoveList {
        /// One list, or a whole selection: in the order they are to sit.
        ids: Vec<ListId>,
        parent: Option<ListId>,
        before: Option<ListId>,
    },
    /// Open the inline field: the heading's `+` for a top-level list, a
    /// row's hover `+` (or *New list inside*) for a child of `parent`.
    BeginNewList {
        parent: Option<ListId>,
    },
    /// Enter, on a name the field accepted.
    NewList {
        parent: Option<ListId>,
        name: String,
        color: Option<String>,
    },
    /// A right-click on a queue or list row, or the menu key on the focused
    /// one: the container opens the small menu at `at`.
    ///
    /// **A bucket has a menu now, and it has one row.** *Open in new tab* is a
    /// thing you can do to a computed queue even though renaming, recolouring
    /// and deleting are not — which is what the old *only a list row has a
    /// menu* rule was really saying, and it said it one step too broadly.
    RowMenu {
        target: RailTarget,
        at: (f32, f32),
    },
    /// Enter on the rename field, over a name the field accepted.
    RenameList {
        id: ListId,
        name: String,
    },
    /// A press on a list's colour swatch: open its colour picker at the
    /// press, as the Tasks page's heading swatch does.
    ListColor {
        id: ListId,
        at: (f32, f32),
    },
    /// A swatch chosen in that menu.
    RecolorList {
        id: ListId,
        color: String,
    },
    /// Delete — **after** the confirmation, never before it. The dialog is the
    /// container's, so this arrives already agreed to, carrying exactly the
    /// lists the dialog counted inside `id`.
    DeleteList {
        id: ListId,
        inside: Vec<ListId>,
    },
}

/// What the rail is showing as selected — the seam the Tasks tab reads.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum RailTarget {
    Bucket(Bucket),
    List(ListId),
    /// The All Reminders page. The Tasks tab swaps its *view* for this one —
    /// a `TasksView` never holds it, which is why the matches down there treat
    /// it as unreachable rather than as a third page.
    Reminders,
}

/// Where the keyboard is in the rail, independent of the page that is open.
///
/// One ring, in the order the rows are drawn: the four queues, the heading's
/// `+`, every list that is showing, then Completed.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum RailFocus {
    Bucket(Bucket),
    /// The `+` beside *Lists*.
    NewList,
    List(ListId),
}

/// The tasks mode's index.
#[derive(Default)]
pub struct TasksRail {
    counts: TaskCounts,
    /// How many fired reminders are asking for attention — the bell badge's
    /// number, repeated on the Reminders row. Pushed by the container, which
    /// reads it off the reminder shell; `0` draws nothing, exactly as the
    /// badge is absent when nothing has fired.
    reminder_attention: usize,
    /// Every live list, depth first.
    lists: Vec<RailList>,
    /// The parents somebody shut, on this device. A set of what is *shut*,
    /// as the document tree's is, so a list nested a moment ago is showing.
    collapsed: HashSet<ListId>,
    /// The sidebar's quick filter, as lowercased words; `None` when off.
    filter: Option<Vec<Vec<char>>>,
    /// Lists (by index) shown only because a match is inside them.
    filter_dim: HashSet<usize>,
    filter_matches: usize,
    /// **The cache a drag reads**: the indices of `lists` whose ancestors are
    /// all open, in order. Rebuilt when the table, the open set or a carried
    /// row's place changes, and never per frame.
    visible: Vec<usize>,
    /// Per list: its own tasks plus everything inside it, `(count, overdue)`.
    /// What a shut parent's row shows.
    totals: Vec<(usize, usize)>,
    /// **The lists picked with Ctrl+click and Shift+click**, the document
    /// tree's gesture: a selection to drag together. A plain press picks one.
    /// It is not the page that is open — that is [`selected`](Self::selected).
    picked: Vec<ListId>,
    /// Where a Shift+click ranges from: the last list pressed without Shift.
    anchor: Option<ListId>,
    /// How many times the visible rows were rebuilt — what the work-count
    /// tests read, so "a drag reads the cache" is asserted and not hoped.
    rebuilds: usize,
    /// A list row on the move, and the slide its neighbours are riding.
    drag: Option<ListDrag>,
    slides: super::slides::RowSlides,
    /// What the Tasks tab is showing. Set here on a press so the card appears
    /// on the frame the row was clicked, and settable from outside so a tab
    /// restored from the session puts the rail where it left it.
    selected: Option<RailTarget>,
    /// Keyboard and AT position, independent of the page currently open.
    focus: Option<RailFocus>,
    hover: Option<RailRow>,
    /// A new list is being typed, under this parent (`None` is the top
    /// level). The rail reserves the row and draws its swatch; the container
    /// draws the field over the rest of it.
    composing: Option<Option<ListId>>,
    /// A list is being **renamed** in place, on the row it already has. Held as
    /// an id rather than an index for the reason a menu's subject is: the table
    /// is pushed from a document a peer also writes, so the row a name is being
    /// typed over may move under the field.
    renaming: Option<ListId>,
    /// **The list a dragged task would join**: tasks carried off a Tasks
    /// page over this list's row. Pushed by the container, which owns the
    /// drag; the rail only lights the row.
    drop_target: Option<ListId>,
    scroll: f32,
}

/// Where a carried list would land: under which list, before which sibling.
type Landing = (Option<ListId>, Option<ListId>);

/// A list row being dragged to a new place in the Lists section.
///
/// **The library's gesture, through the library's arithmetic**
/// (`koru_ui::tree_drag`): a press is latent, four pixels of travel arm it,
/// the top and bottom quarters of a row mean *before* and *after* and its
/// middle means *into*, a shut parent held under the pointer springs open,
/// and the section crawls at its edges. While it is armed the rows are
/// rearranged *here* and the ones that shift slide into place, through the
/// same [`RowSlides`](super::slides::RowSlides) component.
///
/// What is this module's own is which drops are allowed: a list goes into
/// any list but itself and its own descendants — and those are not on screen
/// to be dropped on, because the carried row is drawn shut.
struct ListDrag {
    /// The list under the pointer when the press began.
    id: ListId,
    /// **Everything being carried**, in table order: the grabbed list alone,
    /// or — when it is one of several picked — every picked list that is not
    /// inside another picked one (those travel with their parent). Filled
    /// when the drag arms.
    carried: Vec<ListId>,
    /// The press landed on one of several picked lists, so it did not open
    /// the list or drop the others: a drag may follow. A release that never
    /// armed does both then, as a click on a document row does.
    open_on_release: bool,
    /// The press's offset into the row, so the row travels from the point it
    /// was grabbed rather than snapping its top edge under the pointer.
    grab_dy: f32,
    /// Where the press began, for the arm threshold.
    origin_y: f32,
    /// The grabbed row's top when the press began, scroll taken out, so it
    /// is still the row's own slot after the section has crawled.
    home: f32,
    /// Where the pointer is now, and the band it is moving in — kept so the
    /// edge crawl can run from the frame clock with no pointer event.
    y: f32,
    area: Rect,
    top: f32,
    /// Whether the threshold has been crossed.
    armed: bool,
    /// The table as it stood when the drag armed. Every preview is this with
    /// one move applied, so a drag that wanders back to where it began shows
    /// exactly what it started from.
    base: Vec<RailList>,
    /// The landing the rows are showing now.
    last: Option<Landing>,
    spring: koru_ui::tree_drag::SpringOpen<ListId>,
    /// The frame clock's last tick, for the crawl's `dt`.
    tick: Option<std::time::Instant>,
}

/// How far a press has to travel before it is a drag — the library's own
/// threshold, so one hand learns one gesture.
const DRAG_ARM_PX: f32 = 4.0;

/// Where `id`'s subtree ends in a depth-first table: one past its last
/// descendant.
fn subtree_end(lists: &[RailList], at: usize) -> usize {
    let depth = lists[at].depth;
    lists[at + 1..]
        .iter()
        .position(|list| list.depth <= depth)
        .map_or(lists.len(), |n| at + 1 + n)
}

/// The sibling after the list at `at`, skipping what is being carried — what
/// *after this row* means as *before which list*.
fn next_sibling(lists: &[RailList], at: usize, skip: &[ListId]) -> Option<ListId> {
    let depth = lists[at].depth;
    for list in &lists[at + 1..] {
        if list.depth < depth {
            return None;
        }
        if list.depth == depth && !skip.contains(&list.id) {
            return Some(list.id);
        }
    }
    None
}

/// Where `id` sits in `lists`, as a landing: its parent and the sibling after
/// it.
fn landing_of(lists: &[RailList], id: ListId) -> Option<Landing> {
    let at = lists.iter().position(|list| list.id == id)?;
    let end = subtree_end(lists, at);
    let depth = lists[at].depth;
    let after = lists
        .get(end)
        .filter(|next| next.depth == depth)
        .map(|next| next.id);
    Some((lists[at].parent, after))
}

/// Move `id` and everything inside it under `parent`, before `before`, in a
/// depth-first table. Answers `false`, changing nothing, when the landing
/// does not exist — a parent that is gone, or one inside the list itself.
///
/// The rail's own arrangement while a drag is in flight and until the table
/// comes back from the write; the system document's `move_list` is the one
/// that counts.
fn apply_move(
    lists: &mut Vec<RailList>,
    id: ListId,
    parent: Option<ListId>,
    before: Option<ListId>,
) -> bool {
    let Some(at) = lists.iter().position(|list| list.id == id) else {
        return false;
    };
    let end = subtree_end(lists, at);
    let inside = |other: ListId| lists[at..end].iter().any(|list| list.id == other);
    if parent.is_some_and(inside) {
        return false;
    }
    let block: Vec<RailList> = lists.drain(at..end).collect();
    let (to, depth) = match parent {
        Some(parent) => match lists.iter().position(|list| list.id == parent) {
            Some(p) => (subtree_end(lists, p), lists[p].depth + 1),
            None => {
                lists.splice(at..at, block);
                return false;
            }
        },
        None => (lists.len(), 0),
    };
    let to = before
        .and_then(|before| lists.iter().position(|list| list.id == before))
        .unwrap_or(to);
    let shift = depth as isize - block[0].depth as isize;
    lists.splice(
        to..to,
        block.into_iter().enumerate().map(|(i, mut list)| {
            list.depth = (list.depth as isize + shift) as usize;
            if i == 0 {
                list.parent = parent;
            }
            list
        }),
    );
    for i in 0..lists.len() {
        lists[i].has_children = lists
            .get(i + 1)
            .is_some_and(|next| next.depth > lists[i].depth);
    }
    true
}

impl TasksRail {
    /// The counts this rail is drawn from. **Test-only, and marked so.**
    ///
    /// Every reader here that a test alone calls carries the gate: drawing
    /// goes to the fields, the container pushes through the `set_*` pairs,
    /// and a public reader with no caller outside the suite is dead code
    /// that reads as API.
    #[cfg(test)]
    pub fn counts(&self) -> &TaskCounts {
        &self.counts
    }

    /// Push the counts. Answers whether anything on screen changed, the same
    /// compare-inside-the-view contract the tag table has.
    pub fn set_counts(&mut self, counts: TaskCounts) -> bool {
        let changed = self.counts != counts;
        self.counts = counts;
        changed
    }

    /// Push the bell's attention count, for the Reminders row. The same
    /// compare-inside-the-view contract as [`set_counts`](Self::set_counts).
    pub fn set_reminder_attention(&mut self, count: usize) -> bool {
        let changed = self.reminder_attention != count;
        self.reminder_attention = count;
        changed
    }

    #[cfg(test)]
    pub fn lists(&self) -> &[RailList] {
        &self.lists
    }

    /// Push the list table: every live list, depth first.
    ///
    /// **Refused while a row is being carried.** The arrangement on screen is
    /// this module's until the release commits it, and the container pushes
    /// the table whenever it or a count changes — accepting one mid-gesture
    /// would snap the row back under the pointer. The library's
    /// `rebuild_during_drag` keeps the same rule for the same reason.
    pub fn set_lists(&mut self, lists: Vec<RailList>) -> bool {
        if self.drag_armed() {
            return false;
        }
        let changed = self.lists != lists;
        if changed {
            self.lists = lists;
            // A list a peer deleted is not picked any more.
            let lists = &self.lists;
            self.picked
                .retain(|picked| lists.iter().any(|list| list.id == *picked));
            self.rebuild_visible();
            // A slide is recorded against a row's place; a new table renumbers
            // them.
            self.slides.clear();
        }
        changed
    }

    /// How many times the visible rows and rolled-up counts have been made.
    #[cfg(test)]
    pub fn rebuilds(&self) -> usize {
        self.rebuilds
    }

    /// The parents this device has shut, so a session can put them back.
    pub fn collapsed(&self) -> Vec<ListId> {
        let mut out: Vec<ListId> = self.collapsed.iter().copied().collect();
        out.sort_unstable();
        out
    }

    pub fn set_collapsed(&mut self, ids: impl IntoIterator<Item = ListId>) {
        self.collapsed = ids.into_iter().collect();
        self.rebuild_visible();
    }

    /// Whether `id` is a parent that is showing what is inside it.
    pub fn is_open(&self, id: ListId) -> bool {
        !self.collapsed.contains(&id)
    }

    /// Open or shut a parent. Answers whether anything changed; a leaf has
    /// nothing to show or hide.
    pub fn set_open(&mut self, id: ListId, open: bool) -> bool {
        let is_parent = self
            .lists
            .iter()
            .any(|list| list.id == id && list.has_children);
        let changed = is_parent
            && if open {
                self.collapsed.remove(&id)
            } else {
                self.collapsed.insert(id)
            };
        if changed {
            self.rebuild_visible();
            self.slides.clear();
        }
        changed
    }

    /// The sidebar's quick filter: only the queues and lists whose name
    /// holds every word, with the lists a match is inside. Folds are not
    /// touched. Answers whether it changed.
    pub fn set_filter(&mut self, words: Option<Vec<Vec<char>>>) -> bool {
        if self.filter == words {
            return false;
        }
        self.filter = words;
        self.scroll = 0.0;
        self.rebuild_visible();
        self.slides.clear();
        true
    }

    /// Put the keyboard on the first row the filter matched: a queue, else
    /// the first matching list.
    pub fn focus_first_match(&mut self) {
        let Some(words) = self.filter.clone() else {
            return;
        };
        let holds = |name: &str| super::quick_filter::spans(name, &words).is_some();
        let queue = QUEUES
            .into_iter()
            .find(|bucket| holds(bucket_label(*bucket)))
            .map(RailFocus::Bucket);
        let list = self
            .visible
            .iter()
            .find(|&&i| !self.filter_dim.contains(&i))
            .map(|&i| RailFocus::List(self.lists[i].id));
        let completed =
            holds(bucket_label(Bucket::Completed)).then_some(RailFocus::Bucket(Bucket::Completed));
        if let Some(first) = queue.or(list).or(completed) {
            self.focus = Some(first);
        }
    }

    /// Whether the quick filter is narrowing the rail.
    pub(crate) fn filtering(&self) -> bool {
        self.filter.is_some()
    }

    /// How many rows the filter matched.
    pub fn filter_matches(&self) -> usize {
        self.filter_matches
    }

    fn filter_holds(&self, name: &str) -> bool {
        self.filter
            .as_ref()
            .is_none_or(|words| super::quick_filter::spans(name, words).is_some())
    }

    /// **The one place the visible rows and the rolled-up counts are made.**
    /// Linear in the table, and called when the table, the open set or a
    /// carried row's place changes — never from drawing or hit-testing.
    fn rebuild_visible(&mut self) {
        let carried: Vec<ListId> = self
            .drag
            .as_ref()
            .filter(|d| d.armed)
            .map(|d| d.carried.clone())
            .unwrap_or_default();
        self.visible.clear();
        self.filter_dim.clear();
        self.filter_matches = 0;
        if let Some(words) = &self.filter {
            let facts: Vec<(u16, bool)> = self
                .lists
                .iter()
                .map(|list| {
                    (
                        list.depth as u16,
                        super::quick_filter::spans(&list.name, words).is_some(),
                    )
                })
                .collect();
            let (keep, dim) = super::quick_filter::keep_with_ancestors(&facts);
            for i in 0..self.lists.len() {
                if keep[i] {
                    self.visible.push(i);
                }
                if dim[i] {
                    self.filter_dim.insert(i);
                }
            }
            self.filter_matches = facts.iter().filter(|(_, matches)| *matches).count()
                + QUEUES
                    .into_iter()
                    .chain([Bucket::Completed])
                    .filter(|bucket| {
                        super::quick_filter::spans(bucket_label(*bucket), words).is_some()
                    })
                    .count();
        }
        let filtering = self.filter.is_some();
        let mut shut_at: Option<usize> = None;
        for (i, list) in self.lists.iter().enumerate() {
            if filtering {
                break;
            }
            if let Some(depth) = shut_at {
                if list.depth > depth {
                    continue;
                }
                shut_at = None;
            }
            self.visible.push(i);
            // A carried row is drawn shut: what is inside travels with it
            // and is nowhere to be dropped on.
            if list.has_children
                && (self.collapsed.contains(&list.id) || carried.contains(&list.id))
            {
                shut_at = Some(list.depth);
            }
        }
        self.totals = self.lists.iter().map(|l| (l.count, l.overdue)).collect();
        let mut above: Vec<usize> = Vec::new();
        let mut parent_at: Vec<Option<usize>> = Vec::with_capacity(self.lists.len());
        for (i, list) in self.lists.iter().enumerate() {
            while above
                .last()
                .is_some_and(|&top| self.lists[top].depth >= list.depth)
            {
                above.pop();
            }
            parent_at.push(above.last().copied());
            above.push(i);
        }
        for i in (0..self.lists.len()).rev() {
            if let Some(p) = parent_at[i] {
                self.totals[p].0 += self.totals[i].0;
                self.totals[p].1 += self.totals[i].1;
            }
        }
        self.rebuilds += 1;
    }

    /// What a list row's trailing lane says, `(count, overdue)`, or `None`
    /// for nothing at all (nested-lists D5).
    ///
    /// A leaf shows its own tasks. A parent that is shut shows its own plus
    /// everything inside; one that is open shows **no number**, because each
    /// row under it is showing its own.
    pub fn shown_counts(&self, i: usize) -> Option<(usize, usize)> {
        let list = self.lists.get(i)?;
        if !list.has_children {
            Some((list.count, list.overdue))
        } else if self.collapsed.contains(&list.id) {
            self.totals.get(i).copied()
        } else {
            None
        }
    }

    /// Whether a press on a list row is in flight, latent or armed — what
    /// keeps pointer moves and the release coming to this module.
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Whether a row is being carried right now.
    pub fn drag_armed(&self) -> bool {
        self.drag.as_ref().is_some_and(|d| d.armed)
    }

    /// Whether a slide is still easing, or a row is still being carried — the
    /// frames this module owes. It ends with the release and the settle, so it
    /// returns to `false` on its own (ADR-26).
    pub fn animating(&self) -> bool {
        self.slides.animating() || self.drag_armed()
    }

    /// Advance the settle and, while a row is carried, the edge crawl and the
    /// spring-open of a shut parent held under the pointer. `true` when
    /// something moved.
    pub fn advance(&mut self, now: std::time::Instant) -> bool {
        let mut moved = self.slides.advance(now);
        let Some(drag) = self.drag.as_mut().filter(|d| d.armed) else {
            return moved;
        };
        let dt = drag
            .tick
            .map_or(0.0, |prev| now.duration_since(prev).as_secs_f32());
        drag.tick = Some(now);
        let (y, area, top) = (drag.y, drag.area, drag.top);
        let opened = drag.spring.due(now, super::SPRING_OPEN_SECS);
        let dy = koru_ui::tree_drag::edge_scroll(
            y,
            top,
            area.y + area.h - FOOTER_PAD,
            theme::AUTO_SCROLL_BAND,
            theme::AUTO_SCROLL_MAX_SPEED,
            dt,
        );
        if dy != 0.0 {
            let before = self.scroll;
            self.scroll_by(area, top, dy);
            if self.scroll != before {
                // The section moved under the pointer, so the landing may have.
                self.carry(now);
                moved = true;
            }
        }
        if let Some(id) = opened
            && self.collapsed.remove(&id)
        {
            self.rebuild_visible();
            self.slides.clear();
            moved = true;
        }
        moved
    }

    /// Abort a press or an armed drag without committing: the table goes back
    /// to what it was when the drag armed.
    pub fn cancel_drag(&mut self) -> bool {
        let Some(drag) = self.drag.take() else {
            return false;
        };
        if drag.armed {
            self.lists = drag.base;
        }
        self.slides.clear();
        self.rebuild_visible();
        true
    }

    /// Where the first list row's slot begins, with no slide applied.
    fn first_list_top(&self, area: Rect, top: f32) -> Option<f32> {
        self.laid_out(area, top)
            .into_iter()
            .find_map(|(row, r)| matches!(row, RailRow::List(_)).then_some(r.y))
    }

    /// Every showing list's slot top, by id, slide included: where each row
    /// is *drawn* now, which is where it has to ease from.
    fn drawn_tops(&self, area: Rect, top: f32) -> HashMap<ListId, f32> {
        let first = self.first_list_top(area, top).unwrap_or(top);
        self.visible
            .iter()
            .enumerate()
            .map(|(slot, &i)| {
                (
                    self.lists[i].id,
                    first + slot as f32 * theme::NAV_ROW_H + self.slides.offset(slot),
                )
            })
            .collect()
    }

    /// Carry an armed drag: work out where the grabbed row would land, show
    /// the table that way, and slide the rows that shifted.
    ///
    /// The **grabbed row's centre**, not the pointer's line, so the row
    /// follows the point it was picked up by — the library's `grab_dy`.
    /// Answers whether the arrangement changed.
    fn carry(&mut self, now: std::time::Instant) -> bool {
        use koru_ui::tree_drag::{Row, Target, Zone, drop_target};

        let Some(drag) = self.drag.as_ref().filter(|d| d.armed) else {
            return false;
        };
        let (id, area, top) = (drag.id, drag.area, drag.top);
        let carried = drag.carried.clone();
        let centre = drag.y - drag.grab_dy + theme::NAV_ROW_H * 0.5;
        let Some(first) = self.first_list_top(area, top) else {
            return false;
        };
        // **The landing is read from a layout that does not move**: the rows
        // as they stood when the drag armed, the carried one still in its
        // slot and shut. Reading it from the rows as they are drawn now would
        // chase its own tail — showing the row inside another shifts that
        // other up a slot, which changes the answer, which shifts it back.
        // What the hand aimed at when it set off is what it lands on.
        let Some(drag) = self.drag.as_ref() else {
            return false;
        };
        let base = &drag.base;
        let home = drag.home - self.scroll;
        let mut packed: Vec<usize> = Vec::with_capacity(base.len());
        let mut hidden_under: Option<usize> = None;
        for (i, list) in base.iter().enumerate() {
            if let Some(depth) = hidden_under {
                if list.depth > depth {
                    continue;
                }
                hidden_under = None;
            }
            packed.push(i);
            if list.has_children
                && (carried.contains(&list.id) || self.collapsed.contains(&list.id))
            {
                hidden_under = Some(list.depth);
            }
        }
        let rows: Vec<Row> = packed
            .iter()
            .map(|&i| Row {
                depth: base[i].depth,
                carried: carried.contains(&base[i].id),
                can_nest: true,
            })
            .collect();
        // **Still over the slot it was picked up from: where it began.**
        // Without this a row lifted a few pixels and put down would land in
        // whatever sits beside its old place — inside the open list above it,
        // say — and a slip of the hand would nest a list.
        let at_home = (drag.y - drag.grab_dy - home).abs() < theme::NAV_ROW_H * 0.5;
        let target = if at_home {
            None
        } else {
            drop_target(&rows, theme::NAV_ROW_H, first, centre)
        };
        let (landing, nest) = match target {
            None => match landing_of(base, id) {
                Some(began) => (began, None),
                None => return false,
            },
            Some(Target::Root) => ((None, None), None),
            Some(Target::Row { index, zone }) => {
                let at = packed[index];
                let over = &base[at];
                match zone {
                    Zone::Before => ((over.parent, Some(over.id)), None),
                    Zone::After => ((over.parent, next_sibling(base, at, &carried)), None),
                    Zone::Nest => ((Some(over.id), None), Some(over.id)),
                }
            }
        };
        // A shut parent rested on opens in place, so the drag can go deeper.
        let shut = nest.filter(|over| self.collapsed.contains(over));
        if let Some(drag) = self.drag.as_mut() {
            drag.spring.rest_on(shut, now);
            if drag.last == Some(landing) {
                return false;
            }
        }
        let before = self.drawn_tops(area, top);
        let mut next = self
            .drag
            .as_ref()
            .map(|d| d.base.clone())
            .unwrap_or_default();
        // Each carried list goes before the same sibling, in table order, so
        // a selection lands together and keeps its order. Back over the slot
        // it was lifted from, nothing has moved.
        if target.is_some() {
            for list in &carried {
                if !apply_move(&mut next, *list, landing.0, landing.1) {
                    return false;
                }
            }
        }
        self.lists = next;
        self.rebuild_visible();
        let first = self.first_list_top(area, top).unwrap_or(first);
        let offsets: Vec<f32> = self
            .visible
            .iter()
            .enumerate()
            .map(|(slot, &i)| {
                let now_at = first + slot as f32 * theme::NAV_ROW_H;
                before
                    .get(&self.lists[i].id)
                    .map_or(0.0, |was| was - now_at)
            })
            .collect();
        self.slides.relocate(offsets, now);
        if let Some(drag) = self.drag.as_mut() {
            drag.last = Some(landing);
        }
        true
    }

    /// The list a carried row would go *into*, for the ring that says so.
    fn nest_target(&self) -> Option<ListId> {
        let drag = self.drag.as_ref().filter(|d| d.armed)?;
        let (parent, before) = drag.last?;
        // Into, rather than beside: the landing names a parent the row did
        // not have, and no sibling to sit before.
        let was = landing_of(&drag.base, drag.id)?.0;
        (before.is_none() && parent != was)
            .then_some(parent)
            .flatten()
    }

    /// A release. `Some` when the row ended somewhere else and the table owes
    /// a move; `None` for a press that never armed, which has already done
    /// its job by opening the list.
    pub fn mouse_up(&mut self) -> Option<TasksRailRequest> {
        let drag = self.drag.take()?;
        if !drag.armed {
            if !drag.open_on_release {
                return None;
            }
            // A click, not a drag, on one of several picked lists: it is the
            // one picked now, and its page opens.
            self.picked = vec![drag.id];
            self.anchor = Some(drag.id);
            self.selected = Some(RailTarget::List(drag.id));
            return Some(TasksRailRequest::OpenList {
                id: drag.id,
                new_tab: false,
            });
        }
        // Where they began is not a move, however far they travelled.
        let moved = drag
            .carried
            .iter()
            .any(|list| landing_of(&drag.base, *list) != landing_of(&self.lists, *list));
        let landed = drag.last.filter(|_| moved);
        // Dropped into a shut parent: open it, so the lists that were just put
        // there are showing rather than gone under a caret.
        if let Some((Some(parent), _)) = landed {
            self.collapsed.remove(&parent);
        }
        self.rebuild_visible();
        self.slides.clear();
        let (parent, before) = landed?;
        Some(TasksRailRequest::MoveList {
            ids: drag.carried,
            parent,
            before,
        })
    }

    /// The picked lists that are not inside another picked list, in table
    /// order: what a drag of the selection carries.
    fn picked_roots(&self) -> Vec<ListId> {
        let mut roots = Vec::new();
        let mut under: Option<usize> = None;
        for list in &self.lists {
            if let Some(depth) = under {
                if list.depth > depth {
                    continue;
                }
                under = None;
            }
            if self.picked.contains(&list.id) {
                roots.push(list.id);
                under = Some(list.depth);
            }
        }
        roots
    }

    /// Shift+click: pick every showing list from the anchor to `id`.
    fn pick_range(&mut self, id: ListId) {
        let anchor = self
            .anchor
            .or(match self.selected {
                Some(RailTarget::List(open)) => Some(open),
                _ => None,
            })
            .unwrap_or(id);
        let slot = |want: ListId| self.visible.iter().position(|&i| self.lists[i].id == want);
        let (Some(from), Some(to)) = (slot(anchor), slot(id)) else {
            self.picked = vec![id];
            self.anchor = Some(id);
            return;
        };
        let (from, to) = (from.min(to), from.max(to));
        self.picked = self.visible[from..=to]
            .iter()
            .map(|&i| self.lists[i].id)
            .collect();
        self.anchor = Some(anchor);
    }

    /// The lists picked together, for the container and the suite.
    #[cfg(test)]
    pub fn picked(&self) -> &[ListId] {
        &self.picked
    }

    pub fn selected(&self) -> Option<RailTarget> {
        self.selected
    }

    pub fn set_selected(&mut self, target: Option<RailTarget>) -> bool {
        let changed = self.selected != target;
        self.selected = target;
        changed
    }

    /// Which parent a new list is being typed under: `Some(None)` is the top
    /// level, `None` is no field at all.
    #[cfg(test)]
    pub fn composing(&self) -> Option<Option<ListId>> {
        self.composing
    }

    /// Reserve the row a new list's name is typed on: first under the
    /// heading, or first under `parent`, which is opened to show it.
    pub fn begin_new_list(&mut self, parent: Option<ListId>) {
        let parent = parent.filter(|id| self.lists.iter().any(|list| list.id == *id));
        self.renaming = None;
        self.composing = Some(parent);
        if let Some(parent) = parent
            && self.collapsed.remove(&parent)
        {
            self.rebuild_visible();
            self.slides.clear();
        }
    }

    pub fn end_new_list(&mut self) {
        self.composing = None;
    }

    /// Which list is being renamed in place, if any.
    #[cfg(test)]
    pub fn renaming(&self) -> Option<ListId> {
        self.renaming
    }

    /// Reserve a list's row for the container's field. **The new-list field is
    /// put away first**, because one keyboard cannot be in two boxes and the
    /// rail is what both are drawn over.
    pub fn begin_rename(&mut self, id: ListId) {
        self.composing = None;
        self.renaming = Some(id);
    }

    pub fn end_rename(&mut self) {
        self.renaming = None;
    }

    /// The swatch a new list would be given: the first token of the shared
    /// accent set no live list is already wearing, and the first token again
    /// once every one is taken.
    ///
    /// **Picked, not asked for.** The design says one field, one colour from
    /// the existing swatch set, no dialog — so the rail offers a colour rather
    /// than a picker, and recolouring is a later, separate gesture.
    pub fn suggested_color(&self) -> &'static str {
        let taken: Vec<&str> = self
            .lists
            .iter()
            .filter_map(|list| list.color.as_deref())
            .collect();
        theme::ACCENT_TOKENS
            .into_iter()
            .find(|token| !taken.contains(token))
            .unwrap_or(theme::ACCENT_TOKENS[0])
    }

    // ------------------------------------------------------------- geometry

    /// **The one function that owns the rail's geometry.** Drawing, hit-testing
    /// and the container's field rect all come here.
    ///
    /// Rows outside the band are not returned at all, which is both the cull
    /// and the hit test's bounds check — `row_rects`'s contract, kept the same
    /// so the two modes cannot disagree about what a shut column offers
    /// (nothing).
    pub fn rows(&self, area: Rect, top: f32) -> Vec<(RailRow, Rect)> {
        if area.w <= 0.0 {
            return Vec::new();
        }
        let bottom = area.y + area.h - FOOTER_PAD;
        self.laid_out(area, top)
            .into_iter()
            .filter(|(_, r)| r.y + r.h > top && r.y < bottom)
            .collect()
    }

    /// Every row and its rect before the band is applied. The scroll extent
    /// and a reveal need the ones that are off-screen, which is the only
    /// reason this is separate from [`rows`](Self::rows).
    fn laid_out(&self, area: Rect, top: f32) -> Vec<(RailRow, Rect)> {
        let mut out = Vec::with_capacity(self.visible.len() + 9);
        let mut y = top - self.scroll;
        let row = |y: f32, h: f32| Rect::new(area.x, y, area.w, h);
        let filtering = self.filter.is_some();
        for bucket in QUEUES {
            // Filtered: a queue shows when its name holds what was typed.
            if !self.filter_holds(bucket_label(bucket)) {
                continue;
            }
            out.push((RailRow::Bucket(bucket), row(y, theme::NAV_ROW_H)));
            y += theme::NAV_ROW_H;
            if filtering {
                continue;
            }
            // Reminders sits under Upcoming: a page rather than a fifth
            // queue, kept inside the queue block because that is where a
            // reader looks for *what needs me*.
            // Absent, not dimmed, when this build hides reminders
            // (koru-features).
            if bucket == Bucket::Upcoming && koru_features::reminders() {
                out.push((RailRow::Reminders, row(y, theme::NAV_ROW_H)));
                y += theme::NAV_ROW_H;
            }
        }
        if filtering {
            // The matching lists, under their heading when there are any,
            // then Completed if it matches. No section air: a short list of
            // answers reads as one.
            if !self.visible.is_empty() {
                out.push((RailRow::ListsHeading, row(y, SECTION_H)));
                y += SECTION_H;
            }
            for &i in &self.visible {
                out.push((RailRow::List(i), row(y, theme::NAV_ROW_H)));
                y += theme::NAV_ROW_H;
            }
            if self.filter_holds(bucket_label(Bucket::Completed)) {
                y += RULE_GAP;
                out.push((RailRow::Bucket(Bucket::Completed), row(y, theme::NAV_ROW_H)));
            }
            return out;
        }
        y += SECTION_GAP;
        out.push((RailRow::ListsHeading, row(y, SECTION_H)));
        y += SECTION_H;
        // **A new list is typed where it will be**: first under the heading,
        // or first under its parent. Never at the foot of the section, where
        // a long rail scrolls it out of sight — which is what made the `+`
        // look as if it did nothing.
        if self.composing == Some(None) {
            out.push((RailRow::Compose, row(y, theme::NAV_ROW_H)));
            y += theme::NAV_ROW_H;
        }
        for &i in &self.visible {
            out.push((RailRow::List(i), row(y, theme::NAV_ROW_H)));
            y += theme::NAV_ROW_H;
            if self.composing == Some(Some(self.lists[i].id)) {
                out.push((RailRow::Compose, row(y, theme::NAV_ROW_H)));
                y += theme::NAV_ROW_H;
            }
        }
        y += SECTION_GAP + RULE_GAP;
        out.push((RailRow::Bucket(Bucket::Completed), row(y, theme::NAV_ROW_H)));
        out
    }

    /// How tall the rail is at this width, unscrolled — what the scroll clamps
    /// against.
    fn content_h(&self, area: Rect, top: f32) -> f32 {
        self.laid_out(area, top)
            .last()
            .map(|(_, r)| r.y + r.h + self.scroll - top)
            .unwrap_or(0.0)
    }

    /// Where the hairline over Completed is drawn — derived from the row under
    /// it rather than from a second sum, so it cannot drift away from it.
    fn rule_y(&self, area: Rect, top: f32) -> Option<f32> {
        self.laid_out(area, top)
            .into_iter()
            .find(|(row, _)| *row == RailRow::Bucket(Bucket::Completed))
            .map(|(_, r)| r.y - RULE_GAP * 0.5)
    }

    /// How deep a row is drawn. A list at its own depth; the compose row one
    /// under the parent it is being typed for; everything else at the edge.
    fn depth_of(&self, row: RailRow) -> usize {
        match row {
            RailRow::List(i) => self.lists.get(i).map_or(0, |list| list.depth),
            RailRow::Compose => self
                .composing
                .flatten()
                .and_then(|parent| self.lists.iter().find(|list| list.id == parent))
                .map_or(0, |parent| parent.depth + 1),
            _ => 0,
        }
    }

    /// Whether `row` is showing its chevron, its `+` and its `…` — the list
    /// row under the pointer, and never while a row is being carried. The
    /// document row's rule.
    fn shows_actions(&self, row: RailRow) -> bool {
        let RailRow::List(i) = row else {
            return false;
        };
        if self.drag_armed() {
            return false;
        }
        // Under the pointer only. The keyboard's row keeps its number: a
        // click leaves the keyboard on the row, and a row that swapped its
        // count for buttons until you clicked elsewhere would hide the one
        // thing the lane is for. The menu key opens the same menu.
        let _ = i;
        self.hover == Some(row)
    }

    /// Every rect inside a row, derived from the row and its band alone.
    ///
    /// `trailing` is the count lane on a bucket or a list row, and the `+` on
    /// the heading. Reserved on every row whether or not anything is drawn in
    /// it, so a label never reflows as a number arrives — `row_parts`'s rule.
    ///
    /// **A list row is laid out as a document row is.** It is indented by its
    /// depth and by nothing else — up to [`MAX_INDENT`] levels (nested-lists
    /// D7), so a deep chain cannot squeeze a name out of a narrow rail. A
    /// parent has no caret lane of its own: its chevron takes the swatch's
    /// slot while the row is under the pointer. Its `+` and `…` sit at the
    /// row's right edge, inside the count lane, and are drawn in the count's
    /// place while the row is under the pointer — so the label never reflows.
    pub fn row_parts(&self, row: RailRow, r: Rect) -> RailRowParts {
        let depth = self.depth_of(row).min(MAX_INDENT);
        let icon_x = icon_lane(r.x, depth as u16, false);
        let icon = Rect::new(icon_x, r.y, ICON_W, r.h);
        let has_caret = matches!(row, RailRow::List(i)
            if self.lists.get(i).is_some_and(|list| list.has_children));
        let caret = if has_caret {
            icon
        } else {
            Rect::new(icon_x, r.y, 0.0, 0.0)
        };
        let text_x = icon_x + ICON_W + 6.0;
        let trailing_w = match row {
            RailRow::ListsHeading => HEADING_PLUS,
            _ => COUNT_LANE,
        };
        let trailing = Rect::new(
            (r.x + r.w - theme::NAV_PAD - trailing_w).max(r.x),
            r.y + (r.h - trailing_w.min(r.h)) * 0.5,
            trailing_w,
            trailing_w.min(r.h),
        );
        let side = super::ROW_DOTS;
        let (add, dots) = if matches!(row, RailRow::List(_)) {
            let dots = Rect::new(
                (r.x + r.w - theme::NAV_PAD - side).max(r.x),
                r.y + (r.h - side) * 0.5,
                side,
                side,
            );
            (
                Rect::new(dots.x - side - ROW_PLUS_GAP, dots.y, side, side),
                dots,
            )
        } else {
            let none = Rect::new(trailing.x, r.y, 0.0, 0.0);
            (none, none)
        };
        // The heading is not a row of the list: its label starts at the pane's
        // own inset, where a section heading belongs, rather than in the icon
        // lane the rows share.
        let text_x = match row {
            RailRow::ListsHeading => r.x + theme::NAV_PAD + 2.0,
            _ => text_x,
        };
        RailRowParts {
            caret,
            icon,
            label: Rect::new(text_x, r.y, (trailing.x - 6.0 - text_x).max(1.0), r.h),
            add,
            dots,
            trailing,
        }
    }

    /// Which row `(x, y)` is on, if any.
    pub fn at(&self, area: Rect, top: f32, x: f32, y: f32) -> Option<RailRow> {
        self.rows(area, top)
            .into_iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(row, _)| row)
    }

    /// Where the row a new list is typed on is, or `None` when no field is up
    /// or it is scrolled out of the band. What the container hangs its inline
    /// field on.
    pub fn new_list_rect(&self, area: Rect, top: f32) -> Option<Rect> {
        self.rows(area, top)
            .into_iter()
            .find(|(row, _)| *row == RailRow::Compose)
            .map(|(_, r)| r)
    }

    /// The part of that row the field itself covers: everything after the
    /// swatch, so the colour the list is about to get stays visible while its
    /// name is typed.
    pub fn new_list_field_rect(&self, area: Rect, top: f32) -> Option<Rect> {
        let r = self.new_list_rect(area, top)?;
        Some(Self::field_rect_of(r, self.row_parts(RailRow::Compose, r)))
    }

    /// Which row a list is on, if the table still holds it. The lookup every
    /// id-keyed question goes through, so nothing here indexes a table a peer
    /// may have rewritten.
    pub fn row_of_list(&self, id: ListId) -> Option<RailRow> {
        self.lists
            .iter()
            .position(|list| list.id == id)
            .map(RailRow::List)
    }

    /// Where the **rename** field is drawn, and where its presses are measured.
    ///
    /// [`new_list_field_rect`](Self::new_list_field_rect)'s counterpart, through
    /// the same arithmetic: one function decides what a field over a rail row
    /// covers, so a rename box cannot sit anywhere a new-list box would not.
    /// `None` when the row is scrolled out of the band or hidden under a shut
    /// parent, or when the table no longer holds the list at all.
    pub fn list_field_rect(&self, area: Rect, top: f32, id: ListId) -> Option<Rect> {
        let want = self.row_of_list(id)?;
        let (row, r) = self
            .rows(area, top)
            .into_iter()
            .find(|(row, _)| *row == want)?;
        Some(Self::field_rect_of(r, self.row_parts(row, r)))
    }

    /// What a field over a rail row covers: from the swatch lane to the pane's
    /// own right-hand inset.
    fn field_rect_of(r: Rect, parts: RailRowParts) -> Rect {
        let x = parts.icon.x;
        Rect::new(x, r.y, (r.x + r.w - theme::NAV_PAD - x).max(1.0), r.h)
    }

    /// Scroll so `want` is inside the band. Answers whether the row exists.
    ///
    /// What makes a field that was just opened visible: the row it is drawn
    /// over can be anywhere in a rail longer than the column.
    fn reveal(&mut self, area: Rect, top: f32, want: RailRow) -> bool {
        let Some((_, bounds)) = self
            .laid_out(area, top)
            .into_iter()
            .find(|(row, _)| *row == want)
        else {
            return false;
        };
        let bottom = area.y + area.h - FOOTER_PAD;
        if bounds.y < top {
            self.scroll += bounds.y - top;
        } else if bounds.y + bounds.h > bottom {
            self.scroll += bounds.y + bounds.h - bottom;
        }
        self.clamp_scroll(area, top);
        true
    }

    /// Bring the row a new list is being typed on into view.
    pub fn reveal_new_list(&mut self, area: Rect, top: f32) -> bool {
        self.reveal(area, top, RailRow::Compose)
    }

    /// Bring a list's row into view, opening every parent above it first.
    pub fn reveal_list(&mut self, area: Rect, top: f32, id: ListId) -> bool {
        let mut above = self
            .lists
            .iter()
            .find(|list| list.id == id)
            .and_then(|list| list.parent);
        let mut opened = false;
        while let Some(parent) = above {
            opened |= self.collapsed.remove(&parent);
            above = self
                .lists
                .iter()
                .find(|list| list.id == parent)
                .and_then(|list| list.parent);
        }
        if opened {
            self.rebuild_visible();
            self.slides.clear();
        }
        match self.row_of_list(id) {
            Some(row) => self.reveal(area, top, row),
            None => false,
        }
    }

    // ---------------------------------------------------------------- input

    /// A press in the column while the rail is showing.
    ///
    /// `None` means *ours, and nothing for the container to do* — the caller
    /// turns that into `Outcome::Consumed`, because a press on the rail's
    /// background must not fall through to whatever is behind the column.
    /// On a queue or the Reminders row `ctrl` opens in a new tab. On a list
    /// row `ctrl` and `shift` pick lists instead, as they pick documents. **The selection does not move then**, and that is
    /// the point of the modifier: a background tab is opened *without* leaving
    /// the queue you are reading, so moving the rail's card would be the one
    /// visible side effect the gesture exists to avoid.
    pub fn mouse_down(
        &mut self,
        area: Rect,
        top: f32,
        x: f32,
        y: f32,
        ctrl: bool,
        shift: bool,
    ) -> Option<TasksRailRequest> {
        let (row, r) = self
            .rows(area, top)
            .into_iter()
            .find(|(_, r)| r.contains(x, y))?;
        let parts = self.row_parts(row, r);
        match row {
            // The `+` is the only live part of the heading; the word "Lists" is
            // furniture.
            RailRow::ListsHeading => parts
                .trailing
                .contains(x, y)
                .then_some(TasksRailRequest::BeginNewList { parent: None }),
            // The container's field is over this row and takes its own
            // presses; one that reaches here is on the swatch.
            RailRow::Compose => None,
            // **A press leaves the keyboard on the row it pressed**, as a
            // press on a document row does, so the arrows carry on from
            // there rather than going to the page that just opened.
            RailRow::Bucket(bucket) => {
                let request = self.open_bucket(bucket, ctrl);
                self.focus = Some(RailFocus::Bucket(bucket));
                Some(request)
            }
            // A queue row's behavior, verbatim: the page is a tab, so the
            // card moves on a plain press and `ctrl` carries the disposition.
            // The keyboard position is cleared for the same reason
            // `open_bucket` clears it: a click resets it.
            RailRow::Reminders => {
                if !ctrl {
                    self.selected = Some(RailTarget::Reminders);
                }
                self.focus = None;
                Some(TasksRailRequest::OpenReminders { new_tab: ctrl })
            }
            RailRow::List(i) => {
                let id = self.lists.get(i)?.id;
                // **The chevron opens and shuts; it opens no page and arms no
                // drag.** It is in the swatch's slot, as a document's is in
                // its icon's.
                if parts.caret.contains(x, y) {
                    let open = !self.is_open(id);
                    self.set_open(id, open);
                    return None;
                }
                // A leaf's swatch is the list's colour, as the page's heading
                // swatch is: a press there opens the colour picker rather than
                // the page. The chevron above already took the parent's slot.
                if parts.icon.contains(x, y) {
                    return Some(TasksRailRequest::ListColor { id, at: (x, y) });
                }
                if self.shows_actions(row) {
                    if parts.add.contains(x, y) {
                        return Some(TasksRailRequest::BeginNewList { parent: Some(id) });
                    }
                    if parts.dots.contains(x, y) {
                        return Some(TasksRailRequest::RowMenu {
                            target: RailTarget::List(id),
                            at: (parts.dots.x + parts.dots.w, parts.dots.y + parts.dots.h),
                        });
                    }
                }
                self.focus = Some(RailFocus::List(id));
                // **Ctrl and Shift pick lists, as they pick documents**: Ctrl
                // adds or takes away one, Shift takes the run from the last
                // list pressed. Neither opens a page. *Open in new tab* is in
                // the row's menu.
                let was_picked = self.picked.contains(&id);
                let mut opens = false;
                let mut open_on_release = false;
                if shift {
                    self.pick_range(id);
                } else if ctrl {
                    // The page that is open is where a selection starts from.
                    if self.picked.is_empty()
                        && let Some(RailTarget::List(open)) = self.selected
                        && open != id
                    {
                        self.picked.push(open);
                    }
                    if was_picked {
                        self.picked.retain(|picked| *picked != id);
                    } else {
                        self.picked.push(id);
                    }
                    self.anchor = Some(id);
                } else if was_picked && self.picked.len() > 1 {
                    // One of several: a drag of them all may follow, so the
                    // others are kept until the release says it was a click.
                    open_on_release = true;
                } else {
                    self.picked = vec![id];
                    self.anchor = Some(id);
                    self.selected = Some(RailTarget::List(id));
                    opens = true;
                }
                // **A plain press opens, and arms a drag behind it.** Opening
                // on the press is what keeps the card instant; the gesture
                // that may follow moves the list — or every picked list.
                if self.picked.contains(&id) {
                    self.drag = Some(ListDrag {
                        id,
                        carried: Vec::new(),
                        open_on_release,
                        grab_dy: y - r.y,
                        origin_y: y,
                        home: r.y + self.scroll,
                        y,
                        area,
                        top,
                        armed: false,
                        base: Vec::new(),
                        last: None,
                        spring: Default::default(),
                        tick: None,
                    });
                }
                opens.then_some(TasksRailRequest::OpenList { id, new_tab: false })
            }
        }
    }

    /// The page a press here would open — a queue, a list or the Reminders
    /// row — without opening it: nothing is selected, no drag is armed and
    /// the keyboard position stays. What the column asks before giving a
    /// modified press a meaning of its own (Alt, which asks where the page
    /// opens). The heading's `+` and the compose row name no page.
    pub fn target_at(&self, area: Rect, top: f32, x: f32, y: f32) -> Option<RailTarget> {
        let (row, _) = self
            .rows(area, top)
            .into_iter()
            .find(|(_, r)| r.contains(x, y))?;
        match row {
            RailRow::Bucket(bucket) => Some(RailTarget::Bucket(bucket)),
            RailRow::Reminders => Some(RailTarget::Reminders),
            RailRow::List(i) => Some(RailTarget::List(self.lists.get(i)?.id)),
            RailRow::ListsHeading | RailRow::Compose => None,
        }
    }

    /// A right-click in the column while the rail is showing.
    ///
    /// **Every row that names a page has a menu; the furniture does not.** A
    /// bucket is a computed queue, so there is nothing about it to rename,
    /// recolour or delete — but *Open in new tab* is a thing you can do to one,
    /// and withholding the menu withheld that too. The Reminders row is a page
    /// the same way, and wears the same one-row menu. The heading, the `+` and
    /// the rename field are furniture and answer `None`, which is *ours and
    /// nothing to open*: the caller swallows it rather than passing it to the
    /// view behind the column.
    ///
    /// It does not move the selection. A menu here acts on the row it opened
    /// over, and taking you off the queue you were reading in order to offer to
    /// rename something is a side effect nobody asked for — unlike the library's
    /// `…`, where the menu's verbs *are* about what is selected.
    pub fn context_menu(&self, area: Rect, top: f32, x: f32, y: f32) -> Option<TasksRailRequest> {
        let target = self.target_at(area, top, x, y)?;
        Some(TasksRailRequest::RowMenu { target, at: (x, y) })
    }

    /// Answers whether the hover moved, so the caller redraws only then.
    pub fn mouse_move(&mut self, area: Rect, top: f32, x: f32, y: f32) -> bool {
        if let Some(drag) = self.drag.as_mut() {
            let moved = drag.y != y;
            drag.y = y;
            drag.area = area;
            drag.top = top;
            // Nothing is carried while the quick filter is on: the rows
            // shown are not neighbours.
            let arming =
                self.filter.is_none() && !drag.armed && (y - drag.origin_y).abs() >= DRAG_ARM_PX;
            if arming {
                drag.armed = true;
                drag.base = self.lists.clone();
                let grabbed = drag.id;
                // The grabbed list alone, or every picked one with it.
                let roots = self.picked_roots();
                let carried = if roots.contains(&grabbed) {
                    roots
                } else {
                    vec![grabbed]
                };
                if let Some(drag) = self.drag.as_mut() {
                    drag.carried = carried;
                }
                // What is inside a carried row folds under it for the trip.
                self.rebuild_visible();
            }
            let carried = self.carry(std::time::Instant::now());
            // A carried row owns the pointer: nothing under it lights up.
            let cleared = self.drag_armed() && self.clear_hover();
            return moved || arming || carried || cleared;
        }
        let hover = self.at(area, top, x, y);
        let changed = self.hover != hover;
        self.hover = hover;
        changed
    }

    /// A double-click on a list that holds lists opens it, or shuts it.
    /// Not on the chevron's slot, where each press already toggled.
    pub fn toggle_fold_at(&mut self, area: Rect, top: f32, x: f32, y: f32) -> bool {
        let Some((row, r)) = self
            .rows(area, top)
            .into_iter()
            .find(|(_, r)| r.contains(x, y))
        else {
            return false;
        };
        let RailRow::List(i) = row else {
            return false;
        };
        let Some(list) = self.lists.get(i) else {
            return false;
        };
        if !list.has_children || self.row_parts(row, r).caret.contains(x, y) {
            return false;
        }
        let (id, open) = (list.id, self.is_open(list.id));
        // The first click of the two armed a drag; it is over.
        self.drag = None;
        self.set_open(id, !open)
    }

    /// Light `list`'s row as where dragged tasks would land, or none.
    /// Answers whether that changed.
    pub fn set_drop_target(&mut self, list: Option<ListId>) -> bool {
        let changed = self.drop_target != list;
        self.drop_target = list;
        changed
    }

    pub fn drop_target(&self) -> Option<ListId> {
        self.drop_target
    }

    pub fn clear_hover(&mut self) -> bool {
        let changed = self.hover.is_some();
        self.hover = None;
        changed
    }

    pub fn scroll_by(&mut self, area: Rect, top: f32, dy: f32) {
        self.scroll -= dy;
        self.clamp_scroll(area, top);
    }

    fn clamp_scroll(&mut self, area: Rect, top: f32) {
        let band = (area.y + area.h - FOOTER_PAD - top).max(0.0);
        let max = (self.content_h(area, top) - band).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    /// Whether the pointer is over anything pressable. `PRESS` and `IDLE` are
    /// the same arrow today (ADR-215 — Koru does not use the hand cursor), and
    /// answering the question anyway is what keeps the record of which
    /// surfaces are pressable.
    pub fn pressable(&self) -> bool {
        self.hover.is_some()
    }

    // -------------------------------------------------------------- drawing

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
        top: f32,
    ) {
        let p = theme::p();
        self.clamp_scroll(area, top);
        let bottom = area.y + area.h - FOOTER_PAD;
        scene.push_clip_layer(
            vello::peniko::Fill::NonZero,
            root,
            &scene::rect(area.x, top, area.w, (bottom - top).max(0.0)),
        );
        let carried = self.drag.as_ref().filter(|d| d.armed).map(|d| d.id);
        let nest = self.nest_target();
        // **The carried row goes last**, so it is drawn over the neighbours it
        // is passing rather than under them. Ordering the rows is the whole of
        // it; there is one raised row at a time and no z-order to keep.
        let mut rows = self.rows(area, top);
        if carried.is_some() {
            rows.sort_by_key(|(row, _)| match row {
                RailRow::List(i) => {
                    usize::from(self.lists.get(*i).is_some_and(|l| Some(l.id) == carried))
                }
                _ => 0,
            });
        }
        let rule = self.rule_y(area, top);
        let mut rule_drawn = false;
        for (row, r) in rows {
            // The divider is recorded **where it sits in the stack** — between
            // the last list row and the Completed row — so the inspector's
            // hierarchy reads in the order the column reads, rather than
            // leading a pass it is not a part of.
            if !rule_drawn && matches!(row, RailRow::Bucket(Bucket::Completed)) {
                if let Some(y) = rule {
                    self.draw_rule(scene, root, area, y);
                }
                rule_drawn = true;
            }
            // **Where a list row is drawn while the section is in motion**:
            // the carried one under the pointer, and every other one easing
            // from where it was into the slot it now has.
            let r = match row {
                RailRow::List(i) => {
                    let is_carried = self.lists.get(i).is_some_and(|l| Some(l.id) == carried);
                    if is_carried {
                        let dy = self.drag.as_ref().map(|d| d.y - d.grab_dy).unwrap_or(r.y);
                        Rect::new(r.x, dy, r.w, r.h)
                    } else {
                        let slot = self.visible.binary_search(&i).unwrap_or(usize::MAX);
                        Rect::new(r.x, r.y + self.slides.offset(slot), r.w, r.h)
                    }
                }
                _ => r,
            };
            let parts = self.row_parts(row, r);
            let active = match row {
                RailRow::Bucket(b) => self.selected == Some(RailTarget::Bucket(b)),
                RailRow::Reminders => self.selected == Some(RailTarget::Reminders),
                RailRow::List(i) => self
                    .lists
                    .get(i)
                    .is_some_and(|list| self.selected == Some(RailTarget::List(list.id))),
                _ => false,
            };
            let _row_scope = koru_draw::inspect::scope(
                "ExploreRow",
                "container",
                root,
                scene::rect(r.x, r.y, r.w, r.h),
                |record| {
                    record.property("row", format!("{row:?}"), "RailRow");
                    record.property("active", active, "view state");
                    if matches!(row, RailRow::List(_) | RailRow::Compose) {
                        record.property("depth", self.depth_of(row) as f64, "list tree");
                        record.property(
                            "indent",
                            format!("{} x NAV_INDENT", self.depth_of(row).min(MAX_INDENT)),
                            "capped at MAX_INDENT levels",
                        );
                    }
                },
            );
            let row_list = match row {
                RailRow::List(i) => self.lists.get(i).map(|list| list.id),
                _ => None,
            };
            let dropping = row_list.is_some()
                && (row_list == self.drop_target || (row_list == nest && row_list != carried));
            if dropping {
                // Where the carried tasks — or the carried list — would land:
                // lit, and ringed in the accent so it reads as a target
                // rather than a hover.
                scene::fill(scene, root, p.hover_tint, &row_chip(r));
                scene::stroke(
                    scene,
                    root,
                    p.accent,
                    1.5,
                    &row_chip(Rect::new(r.x + 0.75, r.y + 0.75, r.w - 1.5, r.h - 1.5)),
                );
            } else if active
                || row_list.is_some_and(|id| self.picked.len() > 1 && self.picked.contains(&id))
            {
                // The open page's row, and every row of a selection.
                draw_chosen_row(scene, root, r, p);
            } else if (self.hover == Some(row) && !matches!(row, RailRow::ListsHeading))
                || (row_list.is_some() && row_list == carried)
            {
                scene::fill(scene, root, p.hover_tint, &row_chip(r));
            }
            let focused = match (self.focus, row) {
                (Some(RailFocus::Bucket(b)), RailRow::Bucket(bucket)) => b == bucket,
                (Some(RailFocus::List(id)), RailRow::List(_)) => row_list == Some(id),
                _ => false,
            };
            if focused {
                scene::stroke(
                    scene,
                    root,
                    p.panel_border,
                    theme::BORDER_W,
                    &row_chip(Rect::new(r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0)),
                );
            }
            // The library's ink rule, so one column does not tell the same
            // story in two greys: a row's icon is always `text_dim`, and its
            // label is full-strength only while the row wears the chosen grey — a
            // chosen row drawn dim would read as half-disabled, the opposite
            // of what choosing it meant.
            let fg = if active { p.text } else { p.text_dim };
            match row {
                RailRow::Bucket(bucket) => {
                    self.draw_glyph(scene, root, engine, bucket, parts.icon, p.text_dim);
                    self.draw_label(scene, root, engine, bucket_label(bucket), parts.label, fg);
                    if self.counts.known {
                        let overdue = (bucket == Bucket::Today)
                            .then_some(self.counts.today_overdue)
                            .filter(|n| *n > 0);
                        self.draw_count(
                            scene,
                            root,
                            engine,
                            r,
                            Some(self.counts.of(bucket)),
                            overdue,
                            self.counts.capped,
                        );
                    }
                }
                RailRow::Reminders => {
                    draw_icon(scene, root, engine, icons::BELL, parts.icon, p.text_dim);
                    self.draw_label(
                        scene,
                        root,
                        engine,
                        reminders_label(crate::format::lang()),
                        parts.label,
                        fg,
                    );
                    // The badge's rule: nothing fired, nothing drawn. Never
                    // capped — that flag is about the store clamping a *task*
                    // page, and says nothing about this number.
                    if self.reminder_attention > 0 {
                        self.draw_count(
                            scene,
                            root,
                            engine,
                            r,
                            Some(self.reminder_attention),
                            None,
                            false,
                        );
                    }
                }
                RailRow::ListsHeading => {
                    let label = engine.simple_layout(
                        koru_i18n::t(crate::format::lang(), koru_i18n::Key::TaskListsHeading),
                        theme::role::section_heading(),
                    );
                    scene::draw_layout_first_line(
                        scene,
                        root,
                        &label,
                        parts.label.x,
                        parts.label.y + (parts.label.h - scene::first_line_height(&label)) * 0.5,
                        Some(p.text_dim),
                    );
                    if self.focus == Some(RailFocus::NewList) {
                        let t = parts.trailing;
                        scene::stroke(
                            scene,
                            root,
                            p.panel_border,
                            theme::BORDER_W,
                            &scene::rrect(t.x - 1.0, t.y - 1.0, t.w + 2.0, t.h + 2.0, 4.0),
                        );
                    }
                    draw_icon(scene, root, engine, icons::PLUS, parts.trailing, p.text_dim);
                }
                RailRow::List(i) => {
                    let Some(list) = self.lists.get(i) else {
                        continue;
                    };
                    let acting = self.shows_actions(row);
                    if list.has_children && acting {
                        // **The chevron takes the swatch's slot under the
                        // pointer**, as a document's takes its icon's.
                        let open = self.is_open(list.id);
                        let _caret = koru_draw::inspect::scope(
                            "ExploreRowCaret",
                            "part",
                            root,
                            scene::rect(parts.caret.x, parts.caret.y, parts.caret.w, parts.caret.h),
                            |record| {
                                record.property("open", open, "this device's open set");
                                record.property("slot", "the swatch's", "shown on hover");
                            },
                        );
                        let (cx, cy) = parts.caret.centre();
                        let turn = if open {
                            std::f64::consts::FRAC_PI_2
                        } else {
                            0.0
                        };
                        let spun = root * Affine::rotate_about(turn, (cx as f64, cy as f64));
                        super::draw_caret(scene, spun, engine, parts.caret, p.text_dim);
                    } else {
                        draw_swatch(
                            scene,
                            root,
                            parts.icon,
                            list.color.as_deref(),
                            list.has_children,
                        );
                    }
                    // While this row is being renamed the container draws the
                    // field over it, exactly as it does over the compose row:
                    // the swatch is this module's contribution and the name is
                    // the field's, or the two would draw over each other.
                    if self.renaming == Some(list.id) {
                        continue;
                    }
                    self.draw_label(scene, root, engine, &list.name, parts.label, fg);
                    if acting {
                        // **`+` and `…` at the row's right edge, in the
                        // count's place**, as a document row has them.
                        let _add = koru_draw::inspect::scope(
                            "ExploreRowAdd",
                            "part",
                            root,
                            scene::rect(parts.add.x, parts.add.y, parts.add.w, parts.add.h),
                            |record| {
                                record.property("action", "new list inside", "TasksRailRequest");
                                record.property("gap", "ROW_PLUS_GAP", "left of the dots");
                            },
                        );
                        draw_icon(scene, root, engine, icons::PLUS, parts.add, p.text_dim);
                        drop(_add);
                        let _dots = koru_draw::inspect::scope(
                            "ExploreRowDots",
                            "part",
                            root,
                            scene::rect(parts.dots.x, parts.dots.y, parts.dots.w, parts.dots.h),
                            |record| {
                                record.property("action", "row menu", "TasksRailRequest");
                                record.property("side", "ROW_DOTS", "the document row's");
                            },
                        );
                        super::actions::draw_dots(scene, root, engine, parts.dots, false);
                        continue;
                    }
                    // **The overdue share replaces the count on a list row**,
                    // where Today's row shows both. A queue is a place you look
                    // at every day and the total is what you are managing; a
                    // list is a bucket you glance at, and the only thing worth
                    // interrupting that glance is work that is already late.
                    //
                    // **And an open parent shows no number at all** (D5): the
                    // rows under it are each showing their own.
                    if let Some((count, overdue)) = self.shown_counts(i) {
                        if overdue > 0 {
                            self.draw_count(scene, root, engine, r, None, Some(overdue), false);
                        } else if self.counts.known {
                            self.draw_count(
                                scene,
                                root,
                                engine,
                                r,
                                Some(count),
                                None,
                                self.counts.capped,
                            );
                        }
                    }
                }
                RailRow::Compose => {
                    // The container draws the field over this row: this
                    // module contributes the swatch the new list will wear and
                    // nothing else, or the two would draw over each other.
                    draw_swatch(scene, root, parts.icon, Some(self.suggested_color()), false);
                }
            }
        }
        // Completed was culled by the scroll band: the rule's band can still
        // be on screen when the row beneath it is not, so it is painted here
        // rather than dropped with the row.
        if !rule_drawn && let Some(y) = rule {
            self.draw_rule(scene, root, area, y);
        }
        scene.pop_layer();
    }

    /// The hairline over Completed, recorded as the **band it sits in**: the
    /// divider as chrome is the RULE_GAP of air with a 1px stroke centred in
    /// it, and the band is what a pointer can reach in the inspector.
    fn draw_rule(&self, scene: &mut Scene, root: Affine, area: Rect, y: f32) {
        let band = scene::rect(
            area.x + theme::NAV_PAD,
            y - RULE_GAP * 0.5,
            (area.w - theme::NAV_PAD * 2.0).max(0.0),
            RULE_GAP,
        );
        let _divider = koru_draw::inspect::scope("ExploreDivider", "part", root, band, |record| {
            record.property("stroke", "1 px", "hairline drawn at the band's middle");
            record.property("separates", "Lists / Completed", "rail stack");
        });
        scene::fill(
            scene,
            root,
            theme::p().panel_border,
            &scene::rect(
                area.x + theme::NAV_PAD,
                y,
                (area.w - theme::NAV_PAD * 2.0).max(0.0),
                1.0,
            ),
        );
    }

    fn draw_glyph(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        bucket: Bucket,
        rect: Rect,
        colour: vello::peniko::Color,
    ) {
        // **No rotation.** Next wore [`icons::ARROW_UP`] turned a quarter turn
        // while the real right arrow was an unverified codepoint; it is
        // verified now, so the transform and the branch that chose it are gone
        // — see [`bucket_glyph`].
        draw_icon(scene, root, engine, bucket_glyph(bucket), rect, colour);
    }

    fn draw_label(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        text: &str,
        rect: Rect,
        colour: vello::peniko::Color,
    ) {
        // While the quick filter is on, a match wears its highlight and a
        // list kept for what is inside it is dim.
        if let Some(words) = &self.filter {
            match super::quick_filter::spans(text, words) {
                Some(spans) => {
                    let p = theme::p();
                    let line = engine.simple_layout(text, theme::role::row());
                    koru_ui::highlighted_text::draw_highlighted_line(
                        scene,
                        root,
                        engine,
                        text,
                        spans,
                        rect.x,
                        rect.y + (rect.h - scene::first_line_height(&line)) * 0.5,
                        rect.w,
                        theme::role::row(),
                        colour,
                        p.text,
                        p.highlights[0].1,
                    );
                    return;
                }
                None => {
                    let label = engine.simple_layout(text, theme::role::row());
                    scene.push_clip_layer(
                        vello::peniko::Fill::NonZero,
                        root,
                        &scene::rect(rect.x, rect.y, rect.w, rect.h),
                    );
                    scene::draw_layout_first_line(
                        scene,
                        root,
                        &label,
                        rect.x,
                        rect.y + (rect.h - scene::first_line_height(&label)) * 0.5,
                        Some(theme::p().placeholder),
                    );
                    scene.pop_layer();
                    return;
                }
            }
        }
        let label = engine.simple_layout(text, theme::role::row());
        scene.push_clip_layer(
            vello::peniko::Fill::NonZero,
            root,
            &scene::rect(rect.x, rect.y, rect.w, rect.h),
        );
        scene::draw_layout_first_line(
            scene,
            root,
            &label,
            rect.x,
            rect.y + (rect.h - scene::first_line_height(&label)) * 0.5,
            Some(colour),
        );
        scene.pop_layer();
    }

    /// The trailing number, right-aligned, with the overdue half in the warm
    /// tone. Laid out from the right so the two parts cannot overlap however
    /// wide either becomes. `capped` is the caller's to say: it marks a count
    /// that came off a page the store clamped, and the Reminders row's number
    /// never did.
    fn draw_count(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        r: Rect,
        count: Option<usize>,
        overdue: Option<usize>,
        capped: bool,
    ) {
        let p = theme::p();
        let (count, overdue) = count_parts(count, overdue, capped);
        let mut right = r.x + r.w - theme::NAV_PAD;
        if let Some(text) = overdue {
            let layout = engine.simple_layout(&text, theme::role::meta());
            right -= layout.width();
            scene::draw_layout_first_line(
                scene,
                root,
                &layout,
                right,
                r.y + (r.h - scene::first_line_height(&layout)) * 0.5,
                Some(warm()),
            );
        }
        if let Some(text) = count {
            let layout = engine.simple_layout(&text, theme::role::meta());
            right -= layout.width();
            scene::draw_layout_first_line(
                scene,
                root,
                &layout,
                right,
                r.y + (r.h - scene::first_line_height(&layout)) * 0.5,
                Some(p.text_dim),
            );
        }
    }
}

/// Every rect inside a rail row. See [`TasksRail::row_parts`].
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct RailRowParts {
    /// A parent list's chevron: the swatch's own slot. Zero-sized on every
    /// other row, so a press on a leaf's swatch opens the row instead.
    pub caret: Rect,
    /// The glyph lane: a queue's icon, a list's swatch.
    pub icon: Rect,
    pub label: Rect,
    /// A list row's `+` — *new list inside* — immediately left of its `…`.
    /// Zero-sized on every other row; drawn and pressable only while the row
    /// is under the pointer or the keyboard.
    pub add: Rect,
    /// A list row's `…`, at the row's right edge: its menu. As `add`.
    pub dots: Rect,
    /// The count lane on a row, the `+` on the heading.
    pub trailing: Rect,
}

/// The two strings a trailing lane draws: the quiet count and the warm overdue
/// share, either of which may be absent.
///
/// A pure function so what the rail *says* is testable without a font: the
/// separator, the `+` on a capped page and the order the two are laid out in
/// are the whole of the design's "5 · 3 overdue".
fn count_parts(
    count: Option<usize>,
    overdue: Option<usize>,
    capped: bool,
) -> (Option<String>, Option<String>) {
    let warm = overdue.map(|n| format!("{n} overdue"));
    let quiet = count.map(|n| {
        let n = if capped {
            format!("{n}+")
        } else {
            n.to_string()
        };
        if warm.is_some() {
            format!("{n} · ")
        } else {
            n
        }
    });
    (quiet, warm)
}

/// The one warm ink in the column: the red accent tone's, in whichever theme is
/// on. **Not a new `Palette` field** — the accent set is already the app's
/// answer to "a colour a document chose", and overdue borrows its red rather
/// than inventing a tenth colour nobody can point at.
fn warm() -> vello::peniko::Color {
    theme::accent_tone("red")
        .map(|tone| tone.ink)
        .unwrap_or(theme::p().text)
}

/// A list's colour square. A square rather than a dot, because a list is a
/// bucket and the round swatch already means *this node's colour*.
///
/// **A list that holds lists wears two**, one behind and offset, as a
/// document that holds documents wears the stacked-pages glyph: a group
/// reads as one before its chevron is found by hovering.
fn draw_swatch(scene: &mut Scene, root: Affine, lane: Rect, token: Option<&str>, group: bool) {
    let colour = token
        .and_then(theme::accent_tone)
        .map(|tone| tone.swatch)
        .unwrap_or(theme::p().text_dim);
    let (cx, cy) = lane.centre();
    let square = |dx: f32, dy: f32| {
        scene::rrect(
            cx - SWATCH * 0.5 + dx,
            cy - SWATCH * 0.5 + dy,
            SWATCH,
            SWATCH,
            SWATCH * 0.25,
        )
    };
    if !group {
        scene::fill(scene, root, colour, &square(0.0, 0.0));
        return;
    }
    // The pair is centred in the lane as one mark: the back square up and to
    // the right and fainter, the front one over it. Told apart by strength
    // rather than by an outline, because the row behind them changes ground
    // when it is hovered or chosen and an outline in one ground would show.
    let step = SWATCH_STACK * 0.5;
    scene::fill(
        scene,
        root,
        colour.multiply_alpha(0.45),
        &square(step, -step),
    );
    scene::fill(scene, root, colour, &square(-step, step));
}

/// The glyph lane's width. Narrower than the library's page-icon slot, which
/// has to hold a colour emoji; a rail row never does.
const ICON_W: f32 = 16.0;
/// The list swatch inside that lane.
const SWATCH: f32 = 9.0;
/// How far apart the two squares of a group's swatch sit.
const SWATCH_STACK: f32 = 3.0;
/// Air above the Lists heading and above the rule over Completed.
const SECTION_GAP: f32 = 10.0;
/// The band the hairline sits in the middle of.
const RULE_GAP: f32 = 9.0;
/// The trailing lane a count is right-aligned into, reserved on every row.
const COUNT_LANE: f32 = 76.0;
/// The heading's `+`, and a list row's.
const HEADING_PLUS: f32 = 18.0;
/// Between a list row's `+` and its count lane.
const ROW_PLUS_GAP: f32 = 2.0;
/// How many levels a list row is indented by, at most (nested-lists D7). A
/// deeper list keeps this indent; its level is still published in full to
/// accessibility.
pub(crate) const MAX_INDENT: usize = 4;

mod accessibility;

#[cfg(test)]
mod tests;
