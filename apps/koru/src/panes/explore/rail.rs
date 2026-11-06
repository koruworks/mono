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
/// The name, the swatch and the order come from the workspace's own list table
/// (the system document); only the counts come off the wire. Two spellings of
/// one name in one answer is a drift waiting to happen, which is why the store
/// sends ids and this carries the name.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RailList {
    pub id: ListId,
    pub name: String,
    /// An `AccentColor` token — `"red"`, `"teal"` — never an RGB value. A
    /// colour chosen on one theme is read on the other.
    pub color: Option<String>,
    pub count: usize,
    pub overdue: usize,
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
    /// An index into the pushed list table.
    List(usize),
    /// "+ New list" — and, while one is being typed, the row the field covers.
    NewList,
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
/// **Managing a list keeps that arrangement.** [`ListMenu`](Self::ListMenu) is
/// the only one of the four this module raises: it is a gesture on a row, and
/// the rail knows where the rows are. The other three are the container's to
/// construct — a menu, a field and a confirmation are all things this crate
/// cannot hold — and they are in this enum for `NewList`'s reason, so that one
/// list has one vocabulary from the right-click to the row that changes.
///
/// **`PartialEq` and not `Eq`**, since [`ListMenu`](Self::ListMenu) carries a
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
    /// A list row was dragged to index `to` in the Lists section. The rows
    /// are already in that order here; what the container owes is the write
    /// that makes the table agree.
    ReorderList {
        id: ListId,
        to: usize,
    },
    /// The `+`, or the "+ New list" row: open the inline field.
    BeginNewList,
    /// Enter, on a name the field accepted.
    NewList {
        name: String,
        color: Option<String>,
    },
    /// A right-click on a queue or list row: the container opens the small menu
    /// at `at`.
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
    /// A swatch chosen in that menu.
    RecolorList {
        id: ListId,
        color: String,
    },
    /// Delete — **after** the confirmation, never before it. The dialog is the
    /// container's, so this arrives already agreed to.
    DeleteList(ListId),
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

/// The tasks mode's index.
#[derive(Default)]
pub struct TasksRail {
    counts: TaskCounts,
    /// How many fired reminders are asking for attention — the bell badge's
    /// number, repeated on the Reminders row. Pushed by the container, which
    /// reads it off the reminder shell; `0` draws nothing, exactly as the
    /// badge is absent when nothing has fired.
    reminder_attention: usize,
    lists: Vec<RailList>,
    /// A list row on the move, and the slide its neighbours are riding.
    drag: Option<ListDrag>,
    slides: super::slides::RowSlides,
    /// What the Tasks tab is showing. Set here on a press so the card appears
    /// on the frame the row was clicked, and settable from outside so a tab
    /// restored from the session puts the rail where it left it.
    selected: Option<RailTarget>,
    /// Keyboard and AT position, independent of the page currently open.
    queue_focus: Option<Bucket>,
    hover: Option<RailRow>,
    /// A new list is being typed. The rail reserves the row and draws its
    /// swatch; the container draws the field over the rest of it.
    composing: bool,
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

/// A list row being dragged to a new place in the Lists section.
///
/// **The library's gesture, on a flat section.** A press is latent, four
/// pixels of travel arm it, and while it is armed the rows are reordered
/// *here* and the ones that shift slide into place — which is what the
/// document tree does (`explore::RowDrag`), through the same
/// [`RowSlides`](super::slides::RowSlides) component. What does not come with
/// it is the tree's own vocabulary: a list nests into nothing, there is no
/// collapsed row to spring open, and a section of a handful of rows needs no
/// edge-band auto-scroll.
struct ListDrag {
    /// The list under the pointer when the press began.
    id: ListId,
    /// Where it sat then, so a release that ended where it started asks for
    /// nothing.
    from: usize,
    /// The press's offset into the row, so the row travels from the point it
    /// was grabbed rather than snapping its top edge under the pointer.
    grab_dy: f32,
    /// Where the press began, for the arm threshold.
    origin_y: f32,
    /// Where the pointer is now.
    y: f32,
    /// Whether the threshold has been crossed.
    armed: bool,
}

/// How far a press has to travel before it is a drag — the library's own
/// threshold, so one hand learns one gesture.
const DRAG_ARM_PX: f32 = 4.0;

/// Which index the row now at `i` came from, after one was taken out of
/// `from` and put back at `to`.
///
/// The slide component records offsets against a row's **current** index, so
/// this is what turns "where everything was" into "where the row at this slot
/// was" — one arithmetic rather than a second pass over the old list.
fn old_index(i: usize, from: usize, to: usize) -> usize {
    if i == to {
        from
    } else if from < to && i >= from && i < to {
        i + 1
    } else if from > to && i > to && i <= from {
        i - 1
    } else {
        i
    }
}

impl TasksRail {
    /// The counts this rail is drawn from. **Test-only, and marked so.**
    ///
    /// Every reader here that a test alone calls carries the gate: drawing
    /// goes to the fields, the container pushes through the `set_*` pairs,
    /// and a public reader with no caller outside the suite is dead code
    /// that reads as API. Four of them — `counts`, `lists`,
    /// `composing`, `renaming` — were one warning on every build of
    /// this binary.
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

    /// Push the list table.
    ///
    /// **Refused while a row is being carried.** The order on screen is this
    /// module's until the release commits it, and the container pushes the
    /// table every turn — accepting one mid-gesture would snap the row back
    /// under the pointer. The library's `rebuild_during_drag` keeps the same
    /// rule for the same reason.
    pub fn set_lists(&mut self, lists: Vec<RailList>) -> bool {
        if self.drag_armed() {
            return false;
        }
        let changed = self.lists != lists;
        self.lists = lists;
        changed
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

    /// Advance the settle. `true` when something moved.
    pub fn advance(&mut self, now: std::time::Instant) -> bool {
        self.slides.advance(now)
    }

    /// Abort a press or an armed drag without committing. The pushed table is
    /// the truth again on the next turn.
    pub fn cancel_drag(&mut self) -> bool {
        let had = self.drag.take().is_some();
        self.slides.clear();
        had
    }

    /// Carry an armed drag: reorder the rows here as the grabbed row's own top
    /// edge passes its neighbours' slots, and slide the ones that shifted.
    ///
    /// The **grabbed row's top**, not the pointer's line, so the row follows
    /// the point it was picked up by — the library's `grab_dy`.
    fn carry(&mut self, area: Rect, top: f32) -> bool {
        let Some(drag) = self.drag.as_ref() else {
            return false;
        };
        if !drag.armed {
            return false;
        }
        let (carried_top, id) = (drag.y - drag.grab_dy, drag.id);
        let Some(now_at) = self.lists.iter().position(|l| l.id == id) else {
            return false;
        };
        let before: Vec<f32> = self.list_slots(area, top);
        let mut want = 0usize;
        for (i, y) in before.iter().enumerate() {
            if carried_top >= *y {
                want = i;
            }
        }
        if want == now_at {
            return false;
        }
        let carried = self.lists.remove(now_at);
        self.lists.insert(want, carried);
        let after = self.list_slots(area, top);
        // `from_offsets[i]` is how far below its new slot the row now at `i`
        // should be drawn — where it was, plus whatever it still owed.
        let offsets: Vec<f32> = after
            .iter()
            .enumerate()
            .map(|(i, y_after)| {
                let was = old_index(i, now_at, want);
                before.get(was).copied().unwrap_or(*y_after) - y_after + self.slides.offset(was)
            })
            .collect();
        self.slides.relocate(offsets, std::time::Instant::now());
        true
    }

    /// The top edge of every list row's slot, in order.
    fn list_slots(&self, area: Rect, top: f32) -> Vec<f32> {
        self.laid_out(area, top)
            .into_iter()
            .filter_map(|(row, r)| matches!(row, RailRow::List(_)).then_some(r.y))
            .collect()
    }

    /// A release. `Some` when the row ended somewhere else and the table owes
    /// a new order; `None` for a press that never armed, which has already
    /// done its job by opening the list.
    pub fn mouse_up(&mut self) -> Option<TasksRailRequest> {
        let drag = self.drag.take()?;
        if !drag.armed {
            return None;
        }
        let to = self.lists.iter().position(|l| l.id == drag.id)?;
        (to != drag.from).then_some(TasksRailRequest::ReorderList { id: drag.id, to })
    }

    /// The list ids in the order the rail is showing them — what a reorder
    /// writes to the table.
    pub fn list_order(&self) -> Vec<ListId> {
        self.lists.iter().map(|l| l.id).collect()
    }

    pub fn selected(&self) -> Option<RailTarget> {
        self.selected
    }

    pub fn set_selected(&mut self, target: Option<RailTarget>) -> bool {
        let changed = self.selected != target;
        self.selected = target;
        changed
    }

    #[cfg(test)]
    pub fn composing(&self) -> bool {
        self.composing
    }

    pub fn begin_new_list(&mut self) {
        self.composing = true;
    }

    pub fn end_new_list(&mut self) {
        self.composing = false;
    }

    /// Which list is being renamed in place, if any.
    #[cfg(test)]
    pub fn renaming(&self) -> Option<ListId> {
        self.renaming
    }

    /// Reserve a list's row for the container's field. **The `+` row's field is
    /// put away first**, because one keyboard cannot be in two boxes and the
    /// rail is what both are drawn over.
    pub fn begin_rename(&mut self, id: ListId) {
        self.composing = false;
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
    /// needs the ones that are off-screen, which is the only reason this is
    /// separate from [`rows`](Self::rows).
    fn laid_out(&self, area: Rect, top: f32) -> Vec<(RailRow, Rect)> {
        let mut out = Vec::with_capacity(self.lists.len() + 8);
        let mut y = top - self.scroll;
        let row = |y: f32, h: f32| Rect::new(area.x, y, area.w, h);
        for bucket in QUEUES {
            out.push((RailRow::Bucket(bucket), row(y, theme::NAV_ROW_H)));
            y += theme::NAV_ROW_H;
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
        y += SECTION_GAP;
        out.push((RailRow::ListsHeading, row(y, SECTION_H)));
        y += SECTION_H;
        for i in 0..self.lists.len() {
            out.push((RailRow::List(i), row(y, theme::NAV_ROW_H)));
            y += theme::NAV_ROW_H;
        }
        out.push((RailRow::NewList, row(y, theme::NAV_ROW_H)));
        y += theme::NAV_ROW_H + SECTION_GAP + RULE_GAP;
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

    /// Every rect inside a row, derived from the row and its band alone.
    ///
    /// `trailing` is the count lane on a bucket or a list row, and the `+` on
    /// the heading. Reserved on every row whether or not anything is drawn in
    /// it, so a label never reflows as a number arrives — `row_parts`'s rule.
    pub fn row_parts(&self, row: RailRow, r: Rect) -> RailRowParts {
        // No caret lane: nothing in the rail expands, so the rows do not pay
        // for the disclosure placeholder the document tree reserves.
        let icon_x = icon_lane(r.x, 0, false);
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
        // The heading is not a row of the list: its label starts at the pane's
        // own inset, where a section heading belongs, rather than in the icon
        // lane the rows share.
        let text_x = match row {
            RailRow::ListsHeading => r.x + theme::NAV_PAD + 2.0,
            _ => text_x,
        };
        RailRowParts {
            icon: Rect::new(icon_x, r.y, ICON_W, r.h),
            label: Rect::new(text_x, r.y, (trailing.x - 6.0 - text_x).max(1.0), r.h),
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

    /// Where the "+ New list" row is, or `None` when it is scrolled out of the
    /// band. What the container hangs its inline field on.
    pub fn new_list_rect(&self, area: Rect, top: f32) -> Option<Rect> {
        self.rows(area, top)
            .into_iter()
            .find(|(row, _)| *row == RailRow::NewList)
            .map(|(_, r)| r)
    }

    /// The part of that row the field itself covers: everything after the
    /// swatch, so the colour the list is about to get stays visible while its
    /// name is typed.
    pub fn new_list_field_rect(&self, area: Rect, top: f32) -> Option<Rect> {
        let r = self.new_list_rect(area, top)?;
        Some(Self::field_rect_of(r, self.row_parts(RailRow::NewList, r)))
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
    /// `None` when the row is scrolled out of the band, or when the table no
    /// longer holds the list at all.
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

    // ---------------------------------------------------------------- input

    /// A press in the column while the rail is showing.
    ///
    /// `None` means *ours, and nothing for the container to do* — the caller
    /// turns that into `Outcome::Consumed`, because a press on the rail's
    /// background must not fall through to whatever is behind the column.
    /// `ctrl` opens in a new tab, the modifier the library rows beside this one
    /// already answer with. **The selection does not move then**, and that is
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
                .then_some(TasksRailRequest::BeginNewList),
            RailRow::NewList => Some(TasksRailRequest::BeginNewList),
            RailRow::Bucket(bucket) => Some(self.open_bucket(bucket, ctrl)),
            // A queue row's behavior, verbatim: the page is a tab, so the
            // card moves on a plain press and `ctrl` carries the disposition.
            // The keyboard position is cleared for the same reason
            // `open_bucket` clears it: a click resets it.
            RailRow::Reminders => {
                if !ctrl {
                    self.selected = Some(RailTarget::Reminders);
                }
                self.queue_focus = None;
                Some(TasksRailRequest::OpenReminders { new_tab: ctrl })
            }
            RailRow::List(i) => {
                let id = self.lists.get(i)?.id;
                if !ctrl {
                    self.selected = Some(RailTarget::List(id));
                }
                // **The press opens, and arms a drag behind it.** Opening on
                // the press is what keeps the card instant; the gesture that
                // may follow reorders. A release that never armed has already
                // done its job, which is why it asks for nothing.
                self.drag = Some(ListDrag {
                    id,
                    from: i,
                    grab_dy: y - r.y,
                    origin_y: y,
                    y,
                    armed: false,
                });
                Some(TasksRailRequest::OpenList { id, new_tab: ctrl })
            }
        }
    }

    /// The page a press here would open — a queue, a list or the Reminders
    /// row — without opening it: nothing is selected, no drag is armed and
    /// the keyboard position stays. What the column asks before giving a
    /// modified press a meaning of its own (Alt, which asks where the page
    /// opens). The heading's `+` and the "+ New list" row name no page.
    pub fn target_at(&self, area: Rect, top: f32, x: f32, y: f32) -> Option<RailTarget> {
        let (row, _) = self
            .rows(area, top)
            .into_iter()
            .find(|(_, r)| r.contains(x, y))?;
        match row {
            RailRow::Bucket(bucket) => Some(RailTarget::Bucket(bucket)),
            RailRow::Reminders => Some(RailTarget::Reminders),
            RailRow::List(i) => Some(RailTarget::List(self.lists.get(i)?.id)),
            RailRow::ListsHeading | RailRow::NewList => None,
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
            if !drag.armed && (y - drag.origin_y).abs() >= DRAG_ARM_PX {
                drag.armed = true;
            }
            let carried = self.carry(area, top);
            // A carried row owns the pointer: nothing under it lights up.
            let cleared = self.drag_armed() && self.clear_hover();
            return moved || carried || cleared;
        }
        let hover = self.at(area, top, x, y);
        let changed = self.hover != hover;
        self.hover = hover;
        changed
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
                        Rect::new(r.x, r.y + self.slides.offset(i), r.w, r.h)
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
                },
            );
            let dropping = matches!(row, RailRow::List(i)
                if self.drop_target.is_some()
                    && self.lists.get(i).map(|list| list.id) == self.drop_target);
            if dropping {
                // Where the carried tasks would land: lit, and ringed in the
                // accent so it reads as a target rather than a hover.
                scene::fill(scene, root, p.hover_tint, &row_chip(r));
                scene::stroke(
                    scene,
                    root,
                    p.accent,
                    1.5,
                    &row_chip(Rect::new(r.x + 0.75, r.y + 0.75, r.w - 1.5, r.h - 1.5)),
                );
            } else if active {
                draw_chosen_row(scene, root, r, p);
            } else if self.hover == Some(row) && !matches!(row, RailRow::ListsHeading) {
                scene::fill(scene, root, p.hover_tint, &row_chip(r));
            }
            if matches!(row, RailRow::Bucket(bucket) if self.queue_focus == Some(bucket)) {
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
                    let label = engine.simple_layout("Lists", theme::role::section_heading());
                    scene::draw_layout_first_line(
                        scene,
                        root,
                        &label,
                        parts.label.x,
                        parts.label.y + (parts.label.h - scene::first_line_height(&label)) * 0.5,
                        Some(p.text_dim),
                    );
                    draw_icon(scene, root, engine, icons::PLUS, parts.trailing, p.text_dim);
                }
                RailRow::List(i) => {
                    let Some(list) = self.lists.get(i) else {
                        continue;
                    };
                    draw_swatch(scene, root, parts.icon, list.color.as_deref());
                    // While this row is being renamed the container draws the
                    // field over it, exactly as it does over the `+` row: the
                    // swatch is this module's contribution and the name is the
                    // field's, or the two would draw over each other.
                    if self.renaming == Some(list.id) {
                        continue;
                    }
                    self.draw_label(scene, root, engine, &list.name, parts.label, fg);
                    // **The overdue share replaces the count on a list row**,
                    // where Today's row shows both. A queue is a place you look
                    // at every day and the total is what you are managing; a
                    // list is a bucket you glance at, and the only thing worth
                    // interrupting that glance is work that is already late.
                    if list.overdue > 0 {
                        self.draw_count(scene, root, engine, r, None, Some(list.overdue), false);
                    } else if self.counts.known {
                        self.draw_count(
                            scene,
                            root,
                            engine,
                            r,
                            Some(list.count),
                            None,
                            self.counts.capped,
                        );
                    }
                }
                RailRow::NewList => {
                    // While the field is up the container draws the row: this
                    // module contributes the swatch the new list will wear and
                    // nothing else, or the two would draw over each other.
                    if self.composing {
                        draw_swatch(scene, root, parts.icon, Some(self.suggested_color()));
                    } else {
                        draw_icon(scene, root, engine, icons::PLUS, parts.icon, p.text_dim);
                        self.draw_label(scene, root, engine, "New list", parts.label, p.text_dim);
                    }
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
    /// The glyph lane, at the pane's own inset — the rail draws no caret, so
    /// unlike the library's tree rows it reserves no disclosure lane.
    pub icon: Rect,
    pub label: Rect,
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
fn draw_swatch(scene: &mut Scene, root: Affine, lane: Rect, token: Option<&str>) {
    let colour = token
        .and_then(theme::accent_tone)
        .map(|tone| tone.swatch)
        .unwrap_or(theme::p().text_dim);
    let (cx, cy) = lane.centre();
    scene::fill(
        scene,
        root,
        colour,
        &scene::rrect(
            cx - SWATCH * 0.5,
            cy - SWATCH * 0.5,
            SWATCH,
            SWATCH,
            SWATCH * 0.25,
        ),
    );
}

/// The glyph lane's width. Narrower than the library's page-icon slot, which
/// has to hold a colour emoji; a rail row never does.
const ICON_W: f32 = 16.0;
/// The list swatch inside that lane.
const SWATCH: f32 = 9.0;
/// Air above the Lists heading and above the rule over Completed.
const SECTION_GAP: f32 = 10.0;
/// The band the hairline sits in the middle of.
const RULE_GAP: f32 = 9.0;
/// The trailing lane a count is right-aligned into, reserved on every row.
const COUNT_LANE: f32 = 76.0;
/// The heading's `+`.
const HEADING_PLUS: f32 = 18.0;

mod accessibility;

#[cfg(test)]
mod tests;
