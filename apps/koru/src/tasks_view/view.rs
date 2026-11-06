// SPDX-License-Identifier: MPL-2.0

//! `TasksView`: state, geometry and input. What it draws is in `draw.rs`.
//!
//! The Trash tab's shape throughout: its own [`TextEngine`], a `stale: bool` so
//! a background tab costs nothing, an outward [`Request`] enum that mutates
//! nothing and builds no menu, and every rectangle from one function so a
//! control and its hit test cannot drift.

use crate::panes::explore::rail::{Bucket, RailTarget};
use chrono::NaiveDate;
use koru_core::attributes::Priority;
use koru_core::ids::{ListId, PageKey};
use koru_core::schema::WorkspaceDoc;
use koru_core::{DocumentId, NodeId, WorkspaceId};
use koru_draw::chip::ChipKind;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme;
use koru_editor::utils::when::{self, Parsed, Reference};
use koru_i18n::{fill, t};
use koru_store_wire::tasks::TaskSort;
use koru_ui::date_grid::DateGrid;
use koru_ui::label_chip::LabelChip;
use koru_ui::menu::{Anchor, Menu};
use koru_ui::table::{Band, Metrics, Table};
use koru_workspace::pages::{PagePhase, PageState};
use koru_workspace_host::{Settings, View};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

use super::body::PageBody;
use super::capture::{Capture, SlashRow, Typed};
use super::echo::{self, Echo, Patch};
use super::menu::ColorPick;
use super::menu::{ListPick, OverflowItem, OverflowRow};
use super::pickers::{
    Choice, ColorPicker, DuePick, ListPicker, Picked, Picker, PriorityPick, Target, due_seeds,
    priority_rows,
};
use super::query::{Due, DueWindow, Filters, TaskEntry, TaskListPanel};
use super::states::{self, Empty};
use crate::leaf::Leaf;

/// The page's own margin — the Trash tab's, because this is the same kind of
/// page: a list read on a wide sheet, which pinned to the window's edges reads
/// as a spreadsheet.
pub(super) const PAD: f32 = 40.0;
/// How long typing on a task's line in the note is left to settle before the
/// rows are asked again — one re-ask per burst, never one per keystroke.
const TASK_TOUCH_SETTLE: std::time::Duration = std::time::Duration::from_millis(250);
/// Between the title and the row area.
pub(super) const HEADER_GAP: f32 = 22.0;
/// The complete control's square, at the head of every row: the press
/// target. The box drawn in it is smaller (`RowParts::checkbox`).
pub(super) const CONTROL_W: f32 = 18.0;
/// Between the control and the text column.
pub(super) const CONTROL_GAP: f32 = 10.0;
/// The `…` at a row's right end.
pub(super) const DOTS_W: f32 = 24.0;
/// The lane the `…` sits in, **reserved on every row** whether or not it is
/// showing. A button that took its space only on hover would reflow the chips
/// beside it as the pointer went down the list.
pub(super) const LANE_W: f32 = DOTS_W;
/// Air between the text column and the chips.
pub(super) const CHIP_GAP: f32 = 14.0;
/// The header block's height before it has been measured. Replaced by
/// [`TasksView::measure_header`] on the first `set_viewport`.
const HEADER_H_GUESS: f32 = 128.0;
/// The `…` beside the heading, which opens the overflow menu.
pub(super) const HEADER_DOTS: f32 = 24.0;
/// How long a revealed row stays lit — the document's own search flash.
const FLASH: std::time::Duration = std::time::Duration::from_millis(1200);
/// A list's colour square, ahead of its name in the header band. Larger than
/// the rail's 9 px because it sits against the heading rather than against a
/// nav row, and it is the same shape for the same reason: a list is a bucket,
/// and the round swatch already means *this node's colour*.
pub(super) const HEADER_SWATCH: f32 = 11.0;
/// Between that square and the name — `heading::SWATCH_STEP` now; kept as
/// the number the suite holds the step to, and what the heading's press
/// targets (the square, the title's field) are placed by.
pub(super) const SWATCH_GAP: f32 = 10.0;
/// The most trailing words the capture row will try to read as a date.
///
/// Three, because the longest phrase the grammar accepts in this position is
/// three words (`in 3 days`, `next tuesday morning`). Trying every suffix would
/// let a four-word title lose its last three words to a phrase nobody typed.
const DATE_WORDS: usize = 3;

/// One line of the table. Not one row: a week rule is a line and is not a task,
/// and the capture row is a line and is not a task either.
#[derive(Clone, PartialEq, Debug)]
pub(super) enum Line {
    /// The ghost row at the top. **The list's own first row, not a sidebar
    /// button** — it is the one creation affordance, and it reads as the next
    /// empty line.
    Capture,
    /// A rule-label in Upcoming: sentence case, over a hairline.
    Rule(String),
    /// An index into [`TasksView::rows`].
    Task(usize),
}

/// What a press landed on.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(super) enum Hit {
    /// The `…` at the head of the page, which opens the overflow menu.
    HeaderDots,
    Capture,
    /// The complete control. Outranks the row it sits in, exactly as a tab's
    /// close box outranks the tab.
    Complete(usize),
    Dots(usize),
    /// The row itself — **a whole-row click opens the node**, which is the
    /// design's one rule about this surface: everything else lives in the menu.
    Row(usize),
    /// **The list chip on a row**, which is the one chip that is a control:
    /// pressing it opens the list's own page. Resolved ahead of the row it
    /// sits in, as the tick and the `…` are.
    ListChip(usize),
    /// **A value chip on a row** — its due, its priority or its colour —
    /// which opens that value's picker at the pointer. The row and the kind.
    ValueChip(usize, ChipKind),
    /// The list's colour square ahead of the heading, which opens the
    /// list's colour menu — the rail row's *Color*, one press nearer.
    Swatch,
    /// The note's *open in a new tab* control, at its band's top-right.
    NoteOpen,
    /// The note's grip, along its band's bottom edge: a drag sizes the band.
    NoteGrip,
}

/// A drag of rows, from the press on a row of a list ordered by hand to the
/// release. A press that never moves past the threshold is a click.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct RowDrag {
    /// What is moving, by identity and in page order: the pressed row, or
    /// the whole selection when the pressed row is part of it.
    pub moving: Vec<(DocumentId, NodeId)>,
    start_y: f32,
    /// The pressed row, when the press landed inside a selection of
    /// several: kept whole so it can be dragged, and narrowed to this row
    /// on a release that never became a drag — what a click there means.
    narrow_to: Option<usize>,
    /// Over a list in the sidebar rather than over the table: the rows
    /// would join that list, so no gap is offered here.
    pub outside: bool,
    /// Past the few pixels that tell a drag from a press.
    pub active: bool,
    /// The gap the rows would land in: before row `gap`, in the page as it
    /// stands (`rows.len()` is the end).
    pub gap: Option<usize>,
}

/// A row's words being renamed in place.
pub(super) struct Rename {
    pub document: DocumentId,
    pub node: NodeId,
    /// The words as they were, so an unchanged Enter asks for nothing.
    pub before: String,
    pub input: koru_editor::components::text_input::TextInput,
}

/// What the view wants the container to do.
///
/// Deliberately thin: it never opens a document, never runs a command and never
/// builds a menu. All three need something a view does not hold — a tab set, a
/// `&mut WorkspaceDoc` for a document this window may not even have open, and
/// the container's one menu vocabulary.
#[derive(Clone, PartialEq, Debug)]
pub enum Request {
    /// Open the task's node in its document.
    ///
    /// `new_tab` is the disposition, carried rather than spelled as a second
    /// variant: *Go to origin*, `Enter` and a double click are one gesture
    /// asking for one thing, and only the window differs. The menu already has
    /// both doors and they must not drift.
    Open {
        document: DocumentId,
        node: NodeId,
        new_tab: bool,
    },
    /// Alt alone on a row: ask where the task opens, with the place chooser
    /// at `at` (this page's coordinates) — the one a reference in a document
    /// shows (ADR-58). The arms and the middle open what [`Open`](Self::Open)
    /// would.
    ChoosePlace {
        document: DocumentId,
        node: NodeId,
        at: (f32, f32),
    },
    /// The row's `…`, or a right-click on the row.
    ///
    /// `native` is a task typed into this page — it lives in the workspace's
    /// hidden tasks document, which the row already says with *Added here, no
    /// document*, so the menu offers no *View in origin* for it.
    RowMenu {
        document: DocumentId,
        node: NodeId,
        completed: bool,
        native: bool,
        /// The list the row is in, when this page is not already that list's
        /// own: the menu offers *Go to list* for it.
        list: Option<ListId>,
        /// Whether the row is already pinned to Today, which decides between
        /// *Add to Today* and *Remove from Today*.
        pinned_today: bool,
        at: (f32, f32),
    },
    /// Open the list's page — the list chip was pressed, or *Go to list* was
    /// chosen. The container owns the tabs and the rail, so it is asked.
    OpenList { list: ListId },
    /// Tick or untick — the canonical command, for every kind.
    Complete {
        document: DocumentId,
        node: NodeId,
        completed: bool,
    },
    /// Enter on the capture row. `due` is what the trailing date word parsed
    /// to, already resolved into a stored temporal; `list` is the membership a
    /// task typed into a list carries (SPEC §8).
    Capture {
        /// Its place in a list ordered by hand: the top. `None` elsewhere.
        rank: Option<i64>,
        title: String,
        due: Option<koru_core::time::Temporal>,
        list: Option<ListId>,
        /// What `/priority` picked on the row, if anything.
        priority: Option<Priority>,
    },
    /// A row of the membership picker was chosen. `None` is *Clear* — take the
    /// task out of whatever list it is in.
    ///
    /// **One variant for both directions**, on `Complete`'s precedent: putting
    /// a task in a list and taking it out are one dimension, and two requests
    /// would be two paths to the one attribute.
    SetList {
        document: DocumentId,
        node: NodeId,
        list: Option<ListId>,
    },
    /// A day, a timed phrase or Clear on the row's calendar. `None` is Clear.
    /// The same shape as [`SetList`](Self::SetList), for the same reason;
    /// the value is already what the store keeps, converted by the one
    /// function every surface uses.
    SetDue {
        document: DocumentId,
        node: NodeId,
        due: Option<koru_core::time::Temporal>,
    },
    /// A level, or Clear, on the row's priority picker.
    SetPriority {
        document: DocumentId,
        node: NodeId,
        priority: Option<Priority>,
    },
    /// A colour, or Clear, on the row's colour picker.
    SetColor {
        document: DocumentId,
        node: NodeId,
        color: Option<String>,
    },
    /// A row's new place in its list's dragged order. A drop of several
    /// rows is a [`Many`](Self::Many) of these.
    SetRank {
        document: DocumentId,
        node: NodeId,
        rank: Option<i64>,
    },
    /// The row's words, renamed in place (a double click, or F2).
    Retitle {
        document: DocumentId,
        node: NodeId,
        title: String,
    },
    /// **One gesture over a selection**: the same edit for each selected row
    /// — a due, a priority or a list picked once for all of them. The
    /// container takes them in order, each through its own one door, so a
    /// batch is a run of ordinary edits and nothing else.
    Many(Vec<Request>),
    /// The `…` or a right-click on a row that is part of a selection of
    /// several: the selection's menu. `native` are the selected tasks typed
    /// into this page — the ones *Add document* can give a home to.
    RowsMenu {
        tasks: Vec<(DocumentId, NodeId)>,
        native: Vec<(DocumentId, NodeId)>,
        /// Every selected task is completed, which offers *Reopen* rather
        /// than *Complete*.
        completed: bool,
        at: (f32, f32),
    },
    /// Delete on the selection: take these tasks off Tasks, which the
    /// container asks about once for the set.
    RemoveTasks { tasks: Vec<(DocumentId, NodeId)> },
    /// The note's corner control: open the page's document in a tab of
    /// its own. The container owns the tabs, so it is asked.
    OpenNote { document: DocumentId },
    /// The heading's colour square was pressed: the list's colour menu at
    /// `at` (this page's coordinates) — the container's one menu vocabulary,
    /// so the square and the rail row offer the same rows.
    ListColor { list: ListId, at: (f32, f32) },
    /// The heading's words, renamed in place (a double click on the title).
    RenameList { list: ListId, name: String },
}

/// One list, as the header and the membership chip need it. Pushed by the
/// container from the workspace's own list table — names and swatches never
/// come off the wire, for the reason the rail's `RailList` gives.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ListFace {
    pub id: ListId,
    pub name: String,
    pub color: Option<String>,
}

pub struct TasksView {
    pub(super) viewport: (f32, f32),
    pub(super) engine: TextEngine,
    pub(super) panel: TaskListPanel,
    /// What the rail is showing as selected — **the tab's own copy of the one
    /// answer**, pushed in by the container so the two never disagree.
    pub(super) target: RailTarget,
    pub(super) rows: Vec<TaskEntry>,
    pub(super) lines: Vec<Line>,
    /// The workspace's list table, for the membership chip and the list header.
    pub(super) lists: Vec<ListFace>,
    /// The workspace's `task-list` documents. A row in one of them was typed
    /// here and names no page, which is what its context line says instead.
    pub(super) task_documents: Vec<DocumentId>,
    /// The day the page was drawn for — what the header prints and what the due
    /// chips are relative to.
    pub(super) today: NaiveDate,
    pub(super) now_ms: i64,
    pub(super) hover: Option<Hit>,
    /// A drag of rows, while the button is down.
    pub(super) drag: Option<RowDrag>,
    /// **A row's words being renamed in place** (a double click, or F2):
    /// the task, and the field standing where its title was.
    pub(super) rename: Option<Rename>,
    /// The row the keyboard is on, **by identity rather than by index**: the
    /// page refreshes underneath it every half minute, and an index would move
    /// the selection onto whichever task had slid into that position.
    pub(super) selected: Option<(DocumentId, NodeId)>,
    /// **The rest of a selection** — the rows Ctrl+click or Shift+click added
    /// beside [`selected`](Self::selected), which stays the focused row the
    /// keyboard walks from and a Shift+click ranges from. Empty for a
    /// selection of one, which is every selection but those two gestures make.
    pub(super) also: Vec<(DocumentId, NodeId)>,
    /// A row somebody was sent to — *View task*, *View in Tasks* — lit for a
    /// moment so the eye finds it, as the document flashes a search hit.
    /// The deadline is what drives the frames; it clears itself.
    pub(super) flash: Option<((DocumentId, NodeId), std::time::Instant)>,
    /// The *Add a task…* row while it has the caret — the words, the date
    /// word, and what `/due`, `/priority` and `/task_list` picked.
    pub(super) capture: Option<Capture>,
    /// **The idle capture row's field** — never focused, never typed into —
    /// which draws the row's placeholder while nobody is typing. The field
    /// that takes the typing draws the same placeholder through the same
    /// code, so the words cannot move when the row is pressed: the field
    /// places them by its own text's baseline, and a placeholder drawn any
    /// other way sat a few pixels away from it.
    pub(super) idle_input: koru_editor::components::text_input::TextInput,
    /// What the chips row and the overflow menu are set to. **The query's own
    /// fields and nothing else** — see [`Filters`].
    pub(super) filters: Filters,
    /// Each row's chips, measured, parallel to [`rows`](Self::rows): a press
    /// on a chip is resolved from the widths, and a hit test holds no
    /// `&mut TextEngine` to measure with.
    pub(super) row_chip_ws: Vec<Vec<f32>>,
    /// The overflow menu while it is open. **The view's own**, because every row
    /// of it changes this page's query and nothing else — and because it is a
    /// *setting* menu, which stays open when a row is chosen.
    pub(super) overflow: Option<Menu<OverflowItem>>,
    /// The picker over a row while one is open — membership, priority or the
    /// calendar — and the row it was opened over.
    ///
    /// **One field for three panels**, so two open at once is not a state
    /// (see [`super::pickers`]). The view's own, for a reason on top of the
    /// overflow's: nothing on this page opens a document to make a change,
    /// and each of the three leaves as a [`Request`] the container turns into
    /// the ordinary task command. Unlike the overflow they **close on a
    /// choice** — a date is a choice, not a setting you adjust twice.
    pub(super) picker: Option<Picker>,
    /// **One opening of one panel**, numbered once and never reused.
    ///
    /// The overflow and the three pickers are this view's own floating
    /// surfaces, and a screen reader has to be able to address their rows — but
    /// a row identified by the thing it names would let a request queued
    /// against a panel that has closed land in its successor.
    /// `ContextMenu::instance` is the same number for the same reason
    /// (ADR-338-accessibleshell); this is that mechanism for the panels this
    /// view owns rather than the container. Bumped on every open, so a
    /// reopened panel cannot inherit anything.
    pub(super) panel_instance: u64,
    pub(super) header_h: f32,
    pub(super) scroll: f32,
    mods: ModifiersState,
    /// Set by `adopt`, drained by `catch_up`. **A background Tasks tab costs a
    /// bool** — the same deferred-work contract a document tab has.
    stale: bool,
    pending: Option<Request>,
    /// What this window has changed that the store's rows do not show yet —
    /// drawn over them until the wait ends. See [`super::echo`].
    echoes: Vec<Echo>,
    /// The page's document and where it stands, as the shell last said.
    page: Option<PageState>,
    /// The note above the rows, once its document is the one handed in.
    pub(super) body: Option<PageBody>,
    /// *Try again* was pressed on a note that failed; drained by the container.
    wants_retry: bool,
    /// Where the conflict band's *Keep deleted* and *Recover* were drawn
    /// this frame, and the person's answer once they press one.
    conflict_actions: Option<(Rect, Rect)>,
    wants_decision: Option<koru_workspace::pages::PageDecision>,
    /// Where the band's notice and its *Try again* were drawn this frame.
    pub(super) notice: Option<(Rect, Rect)>,
    /// The band's height this frame — what the rows move down by.
    band_h: f32,
    /// The height a person dragged the band to, kept across the note being
    /// rebuilt; `None` while it follows its content.
    band_fixed_h: Option<f32>,
    /// A drag of the band's grip while the button is down: the distance
    /// from the pointer to the band's bottom edge at the press, so the edge
    /// does not jump under the pointer.
    band_resize: Option<f32>,
    /// **The heading's words being renamed in place** (a double click on
    /// the title of a list page): the field standing where the title was.
    pub(super) heading_rename: Option<koru_editor::components::text_input::TextInput>,
    /// Where the pointer last was, so a wheel over the note scrolls the note.
    pointer: (f32, f32),
    /// Typing landed on a task's line in the note: when, and the write the
    /// rows have to show. Coalesced — one re-ask per quarter second of
    /// typing, not one per keystroke — and flushed by `maintain`.
    task_touch: Option<(std::time::Instant, super::query::WriteRequirement)>,
}

impl TasksView {
    pub fn new(target: RailTarget) -> Self {
        let today = chrono::Local::now().date_naive();
        let mut engine = TextEngine::new();
        let idle_input =
            koru_editor::components::text_input::TextInput::new(&mut engine, theme::role::row());
        Self {
            viewport: (900.0, 600.0),
            engine,
            idle_input,
            panel: TaskListPanel::default(),
            target,
            rows: Vec::new(),
            lines: vec![Line::Capture],
            lists: Vec::new(),
            task_documents: Vec::new(),
            today,
            now_ms: chrono::Utc::now().timestamp_millis(),
            hover: None,
            drag: None,
            rename: None,
            selected: None,
            also: Vec::new(),
            flash: None,
            capture: None,
            filters: Filters::default(),
            row_chip_ws: Vec::new(),
            overflow: None,
            picker: None,
            panel_instance: 0,
            header_h: HEADER_H_GUESS,
            scroll: 0.0,
            mods: ModifiersState::empty(),
            stale: true,
            pending: None,
            echoes: Vec::new(),
            page: None,
            body: None,
            wants_retry: false,
            conflict_actions: None,
            wants_decision: None,
            notice: None,
            band_h: 0.0,
            band_fixed_h: None,
            band_resize: None,
            heading_rename: None,
            pointer: (-1.0, -1.0),
            task_touch: None,
        }
    }

    pub fn target(&self) -> RailTarget {
        self.target
    }

    /// Point the tab at another bucket or list. Answers whether it moved.
    pub fn set_target(&mut self, target: RailTarget) -> bool {
        if self.target == target {
            return false;
        }
        self.target = target;
        self.scroll = 0.0;
        self.hover = None;
        // The selection belongs to the page it was made on. Another bucket is
        // another page, and a row carried across would be a keyboard sitting on
        // something this page is not showing.
        self.clear_selection();
        self.end_capture();
        // **The menu belongs to the page it was opened over.** Another bucket is
        // another page, and a panel left standing over it would be describing
        // the query it is no longer above.
        self.overflow = None;
        // And the picker belongs to the *row* it was opened over, which the new
        // page may not even hold.
        self.picker = None;
        // An echo is a change to *this* page's rows; another page starts from
        // the store's answer.
        self.echoes.clear();
        // The note belongs to the page it was written above. Another page has
        // its own document, which the shell names on the next turn.
        self.page = None;
        self.body = None;
        self.notice = None;
        self.band_h = 0.0;
        // **A filter the new page cannot show is a filter the new page must not
        // apply.** The due dimension is offered on a list and on nothing else,
        // so carrying a list's *Overdue* across to a queue would go on
        // narrowing the answer with no chip on screen saying so — a page
        // quietly missing rows, which is worse than a wrong filter because
        // there is nothing to press to undo it. Cleared through the panel too,
        // so the next answer is the one the chips describe.
        if !Filters::due_selects(target) && self.filters.window != DueWindow::Any {
            self.filters.window = DueWindow::Any;
            self.panel.set_filters(self.filters);
        }
        self.stale = true;
        true
    }

    /// What the page is filtered by. **Test-only** for now: nothing outside
    /// this view reads it, and the query it builds is the production answer.
    #[cfg(test)]
    pub fn filters(&self) -> Filters {
        self.filters
    }

    /// Point the page at another set of filters, and ask the store again.
    ///
    /// **One door.** A chip, a menu toggle and a sort row all arrive here, so
    /// there is no path from a control to the query that skips the re-query, the
    /// scroll reset or the re-measure.
    pub(super) fn set_filters(&mut self, filters: Filters) -> bool {
        if self.filters == filters {
            return false;
        }
        self.filters = filters;
        self.panel.set_filters(filters);
        self.scroll = 0.0;
        self.rows.clear();
        self.relayout();
        self.stale = true;
        true
    }

    pub fn panel(&self) -> &TaskListPanel {
        &self.panel
    }

    pub fn panel_mut(&mut self) -> &mut TaskListPanel {
        &mut self.panel
    }

    pub fn set_lists(&mut self, lists: Vec<ListFace>) -> bool {
        if self.lists == lists {
            return false;
        }
        self.lists = lists;
        // The list chip's word is the table's, so a rename moves its width.
        self.remeasure_row_chips();
        true
    }

    /// The workspace's `task-list` documents, pushed by the container.
    pub fn set_task_documents(&mut self, documents: Vec<DocumentId>) -> bool {
        if self.task_documents == documents {
            return false;
        }
        self.task_documents = documents;
        true
    }

    /// Put the keyboard on this task, if this page is showing it.
    ///
    /// **By identity and answering whether it landed**, which is what lets the
    /// container hold the ask until the row arrives: the selection is already
    /// `(DocumentId, NodeId)` for the reason this needs — the page refreshes
    /// under the keyboard and an index would move it to whichever task slid
    /// into that position.
    pub fn select_task(&mut self, document: DocumentId, node: NodeId) -> bool {
        if self.row_of(document, node).is_none() {
            return false;
        }
        self.selected = Some((document, node));
        self.also.clear();
        true
    }

    /// [`select_task`](Self::select_task), and light the row for a moment —
    /// the landing of a gesture that sent somebody here.
    pub fn reveal_task(&mut self, document: DocumentId, node: NodeId) -> bool {
        if !self.select_task(document, node) {
            return false;
        }
        self.flash = Some(((document, node), std::time::Instant::now() + FLASH));
        true
    }

    /// How lit a row is now: `1.0` at the landing fading to nothing at the
    /// deadline, `None` for every other row and after it.
    pub(super) fn flash_alpha(&self, document: DocumentId, node: NodeId) -> Option<f32> {
        let ((d, n), until) = self.flash?;
        if (d, n) != (document, node) {
            return None;
        }
        let left = until.saturating_duration_since(std::time::Instant::now());
        (left > std::time::Duration::ZERO).then(|| left.as_secs_f32() / FLASH.as_secs_f32())
    }

    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    /// **Every request leaves through here, and leaves its echo behind.** The
    /// row the request acts on is shown as it will be — ticked, dated,
    /// moved off the page — before the store has heard, and the store's
    /// answer replaces the echo when it includes the write.
    pub fn take_request(&mut self) -> Option<Request> {
        let request = self.pending.take()?;
        self.record_echo(&request);
        Some(request)
    }

    fn record_echo(&mut self, request: &Request) {
        if let Request::Many(requests) = request {
            for request in requests {
                self.record_echo(request);
            }
            return;
        }
        let (document, node, patch) = match request {
            Request::Capture {
                title,
                due,
                list,
                priority,
                rank,
            } => {
                // The workspace only matters for opening the row, which the
                // real ids replace on the receipt; a placeholder serves.
                let workspace = self.panel.workspace().unwrap_or_else(WorkspaceId::new);
                let document = self
                    .task_documents
                    .first()
                    .copied()
                    .unwrap_or_else(DocumentId::new);
                let row = echo::provisional_entry(
                    workspace,
                    document,
                    title,
                    due.as_ref(),
                    *list,
                    *priority,
                    self.today,
                    self.now_ms,
                );
                let row = TaskEntry { rank: *rank, ..row };
                (row.document, row.node, Patch::Created(row))
            }
            Request::Complete {
                document,
                node,
                completed,
            } => (*document, *node, Patch::Completed(*completed)),
            Request::SetDue {
                document,
                node,
                due,
            } => (*document, *node, Patch::Due(due.as_ref().map(echo::due_of))),
            Request::SetPriority {
                document,
                node,
                priority,
            } => (
                *document,
                *node,
                Patch::Priority(priority.map(|p| p.as_str().to_owned())),
            ),
            Request::SetList {
                document,
                node,
                list,
            } => (*document, *node, Patch::List(*list)),
            Request::SetColor {
                document,
                node,
                color,
            } => (*document, *node, Patch::Color(color.clone())),
            Request::SetRank {
                document,
                node,
                rank,
            } => (*document, *node, Patch::Rank(*rank)),
            Request::Retitle {
                document,
                node,
                title,
            } => (
                *document,
                *node,
                // Without its emoji, as the store's row is: the page draws
                // the emoji apart from the words.
                Patch::Title(
                    koru_shell_chrome::split_leading_emoji(title)
                        .1
                        .trim_start()
                        .to_owned(),
                ),
            ),
            Request::Open { .. }
            | Request::ChoosePlace { .. }
            | Request::RowMenu { .. }
            | Request::RowsMenu { .. }
            | Request::RemoveTasks { .. }
            | Request::OpenList { .. }
            | Request::OpenNote { .. }
            | Request::ListColor { .. }
            | Request::RenameList { .. } => return,
            Request::Many(_) => unreachable!("taken apart above"),
        };
        crate::tasks::debug_line(format_args!(
            "page: echo pushed by this page ({} standing)",
            self.echoes.len() + 1
        ));
        self.echoes.push(Echo {
            document,
            node,
            patch,
            armed: false,
            since: std::time::Instant::now(),
        });
        self.apply_echoes();
        self.relayout();
    }

    /// A command the container queued on its own — a menu row's *Complete*,
    /// a confirmed *Remove* — rather than one this page asked for and echoed
    /// on the way out. The two that exist; the rest only ever leave as
    /// requests.
    pub fn echo_command(&mut self, command: &koru_workspace_host::tasks::TaskCommand) {
        use koru_workspace_host::tasks::TaskCommand;
        let (document, node, patch) = match command {
            TaskCommand::Complete {
                document,
                node,
                completed,
            } => (*document, *node, Patch::Completed(*completed)),
            TaskCommand::Remove { document, node } => (*document, *node, Patch::Removed),
            TaskCommand::CreateHome { document, node } => {
                let Some(row) = self.row_of(*document, *node) else {
                    return;
                };
                // The page is the task now: the row reads the page's name,
                // wears the page's glyph, and sits at the workspace's root.
                let mut row = row.clone();
                row.title = koru_workspace::tasks::home_title(&row.title);
                row.kind = super::query::RowKind::Document;
                row.breadcrumb = String::new();
                (
                    *document,
                    *node,
                    Patch::Homed {
                        from: (*document, *node),
                        row,
                    },
                )
            }
            _ => return,
        };
        self.echoes.push(Echo {
            document,
            node,
            patch,
            armed: false,
            since: std::time::Instant::now(),
        });
        self.apply_echoes();
        self.relayout();
    }

    /// The receipt for the oldest command still waiting for one. A capture's
    /// provisional row takes the real ids, so it is a row that can be acted
    /// on from this moment rather than after the store answers.
    pub fn echo_landed(&mut self, landed: &koru_workspace_host::tasks::Landed) {
        // **The receipt, and whether anything can wait on it.** This is the
        // fork the tick-that-unticks-itself turns on: a receipt that is not
        // published, or that names no workspace, arms the echo but arms no
        // requirement — so nothing is left to end the wait, and the next answer
        // has to be the one that decides.
        crate::tasks::debug_line(format_args!(
            "page: receipt published={} workspace={} echoes={}",
            landed.published,
            if landed.workspace.is_some() {
                "named"
            } else {
                "none"
            },
            self.echoes.len(),
        ));
        let Some(echo) = self.echoes.iter_mut().find(|e| !e.armed) else {
            crate::tasks::debug_line(format_args!("page: receipt matched no unarmed echo"));
            return;
        };
        echo.armed = true;
        if let Patch::Created(row) | Patch::Homed { row, .. } = &mut echo.patch
            && let Some(node) = landed.node
        {
            let was = (echo.document, echo.node);
            row.document = landed.document;
            row.node = node;
            echo.document = landed.document;
            echo.node = node;
            for shown in &mut self.rows {
                if (shown.document, shown.node) == was {
                    shown.document = landed.document;
                    shown.node = node;
                }
            }
            if self.selected == Some(was) {
                self.selected = Some((landed.document, node));
            }
        }
    }

    /// A command the shell refused: its echo was a promise, and it is taken
    /// back with the refusal.
    pub fn echo_failed(&mut self) {
        if let Some(i) = self.echoes.iter().position(|e| !e.armed) {
            self.echoes.remove(i);
            self.rows = self.panel.rows().to_vec();
            self.apply_echoes();
            self.relayout();
        }
    }

    /// Draw the echoes over [`rows`](Self::rows), which are the store's.
    fn apply_echoes(&mut self) {
        let (target, lifecycle, today, now_ms) =
            (self.target, self.filters.lifecycle, self.today, self.now_ms);
        for echo in &self.echoes {
            if let Patch::Homed { from, row } = &echo.patch {
                // **The same row, re-identified, where it was.** The page-made
                // row the store still answers with is the task that moved, so
                // it is drawn as the echo's row in its place — and not filtered
                // by the page again: the store admitted it, and a home changes
                // neither its queue nor its lifecycle.
                let at = self.rows.iter().position(|r| (r.document, r.node) == *from);
                self.rows.retain(|r| (r.document, r.node) != *from);
                if !self
                    .rows
                    .iter()
                    .any(|r| r.document == row.document && r.node == row.node)
                {
                    let at = at.unwrap_or(self.rows.len()).min(self.rows.len());
                    self.rows.insert(at, row.clone());
                }
                continue;
            }
            if let Patch::Created(row) = &echo.patch {
                if self
                    .rows
                    .iter()
                    .any(|r| r.document == row.document && r.node == row.node)
                {
                    continue;
                }
                if !echo::belongs(row, target, lifecycle, today) {
                    continue;
                }
                // Where the store would sort it: among the dated rows by day
                // when the page is in date order, at the end otherwise.
                let at = match (self.filters.sort, row.due_day()) {
                    (TaskSort::Due, Some(day)) => self
                        .rows
                        .iter()
                        .position(|r| r.due_day().is_none_or(|d| d > day))
                        .unwrap_or(self.rows.len()),
                    _ => self.rows.len(),
                };
                self.rows.insert(at, row.clone());
                continue;
            }
            let Some(i) = self
                .rows
                .iter()
                .position(|r| r.document == echo.document && r.node == echo.node)
            else {
                continue;
            };
            let keep = echo::apply_patch(
                &mut self.rows[i],
                &echo.patch,
                target,
                lifecycle,
                today,
                now_ms,
            );
            if !keep {
                self.rows.remove(i);
            }
        }
        // **A list ordered by hand is drawn in its order now**, not when the
        // store answers: the store's manual sort is the ranked by rank, then
        // the rest as it sent them, and a stable sort on the same key keeps
        // the store's own order wherever no echo moved a rank.
        if self.manual_order() {
            self.rows
                .sort_by_key(|row| (row.rank.is_none(), row.rank.unwrap_or(0)));
        }
    }

    /// Back-date every standing echo, so a test can reach the ceiling without
    /// sleeping through it. The ceiling is wall-clock (`since.elapsed()`) — it
    /// is a promise to a reader about how long a stale row may look right, and
    /// a clock this type could be handed would be a clock the production path
    /// never uses.
    #[cfg(test)]
    pub(super) fn age_echoes_for_test(&mut self, by: std::time::Duration) {
        for echo in &mut self.echoes {
            if let Some(earlier) = echo.since.checked_sub(by) {
                echo.since = earlier;
            }
        }
    }

    #[cfg(test)]
    pub(super) fn echoes_for_test(&self) -> usize {
        self.echoes.len()
    }

    /// The row this list is showing for a node, if it is showing one. What the
    /// E-3 confirm quotes, so the sentence names the row a person is looking at
    /// rather than a page fetched again a moment later.
    pub fn row_of(&self, document: DocumentId, node: NodeId) -> Option<&TaskEntry> {
        self.rows
            .iter()
            .find(|row| row.document == document && row.node == node)
    }

    pub fn today(&self) -> NaiveDate {
        self.today
    }

    /// Put a refused capture's text back in the field.
    ///
    /// **Losing what somebody typed is the one unrecoverable bug**, so a
    /// capture the store would not take reopens the row with the words in it —
    /// including the date word, which is re-parsed and previews again exactly
    /// as it did before Enter.
    pub fn restore_capture(&mut self, title: &str) {
        let today = self.today;
        let mut capture = Capture::with_text(&mut self.engine, title, today);
        capture.set_anchor(self.capture_anchor());
        self.capture = Some(capture);
    }

    /// The name and swatch of a list this workspace knows, or `None` for an id
    /// its table has forgotten — a stale membership is not a missing task, so
    /// the chip is simply not drawn.
    pub(super) fn list_face(&self, id: ListId) -> Option<&ListFace> {
        self.lists.iter().find(|face| face.id == id)
    }

    /// The glyph a row leads its title with: its kind's own for an image, a
    /// file or a whole page; **the page glyph for a paragraph that lives in a
    /// document**, so a row with an origin says so at a glance beside the
    /// ones added here; nothing for a task typed into this page. A row whose
    /// title begins with an emoji wears that instead — it is the task's own
    /// picture, drawn by `paint_row` from `icon`.
    pub(super) fn row_glyph(&self, row: &TaskEntry) -> Option<char> {
        if let Some(glyph) = row.kind.glyph(row.mime.as_deref(), &row.title) {
            return Some(glyph);
        }
        if row.icon.is_some() || self.task_documents.contains(&row.document) {
            return None;
        }
        Some(koru_draw::icons::DOCUMENT)
    }

    /// The chips one row wears, in the order they are laid out from the right:
    /// what is wrong with the row, then where it belongs, then when, then how
    /// much it matters.
    ///
    /// **Content, not paint** — `paint_row` measures and draws exactly this
    /// list, so what a row says can be asserted without a scene. The membership
    /// chip is the one that carries a *name the wire never sent*: the answer
    /// holds a [`ListId`], and the local list table is what turns it into a word
    /// (design note 9 — a task can be in Today *and* in Bike Project without a
    /// second vocabulary).
    pub(super) fn row_chips(&self, row: &TaskEntry) -> Vec<RowChip> {
        let mut chips = Vec::new();
        if row.trashed {
            chips.push(RowChip::tag("In trash".to_owned(), None));
        }
        if let Some(face) = row.list.and_then(|id| self.list_face(id)) {
            chips.push(RowChip {
                kind: ChipKind::Tag,
                label: face.name.clone(),
                accent: face.color.clone(),
                list: row.list,
            });
        }
        // **A dateless open row wears the dash**, not nothing: the design's own
        // sentence about a list page is that dateless tasks are normal there, so
        // the chip reads as *no date* rather than pushing anybody to schedule.
        if row.due.is_some() || !row.completed {
            chips.push(RowChip {
                kind: ChipKind::Due,
                label: due_label(row.due, self.today),
                accent: row.overdue.then(|| super::row::WARM.to_owned()),
                list: None,
            });
        }
        if let Some(priority) = &row.priority
            && let Some(level) = koru_core::attributes::Priority::parse(priority)
        {
            chips.push(RowChip {
                kind: ChipKind::Priority,
                label: koru_editor::chips::priority_label(level).to_owned(),
                // Its level's colour, the document chip's own.
                accent: Some(koru_editor::chips::priority_accent(level).to_owned()),
                list: None,
            });
        }
        // **The line's colour, last** — the swatch the document draws after
        // the same task's chips, so a task coloured red there is red here.
        if let Some(token) = &row.color {
            chips.push(RowChip {
                kind: ChipKind::Swatch,
                label: String::new(),
                accent: Some(token.clone()),
                list: None,
            });
        }
        chips
    }

    /// The list a row's chip and menu lead to: its membership, **when this
    /// page is not already that list's own** — a chip that opens the page it
    /// is on is a label wearing a control, and the menu row would be a no-op
    /// with a name — and only while the table still has the list, since a
    /// membership the table cannot name draws no chip either.
    pub(super) fn leads_to_list(&self, row: &TaskEntry) -> Option<ListId> {
        row.list
            .filter(|id| self.target != RailTarget::List(*id) && self.list_face(*id).is_some())
    }

    /// Re-measure every row's chips. Called from `relayout`, which runs
    /// wherever the rows can have changed, and from `set_lists`, since the
    /// list chip's word is the table's.
    pub(super) fn remeasure_row_chips(&mut self) {
        let specs: Vec<Vec<RowChip>> = self.rows.iter().map(|row| self.row_chips(row)).collect();
        self.row_chip_ws = specs
            .iter()
            .map(|chips| {
                chips
                    .iter()
                    .map(|chip| chip.label_chip().width(&mut self.engine))
                    .collect()
            })
            .collect();
    }

    /// Where row `idx`'s chips are in the line drawn at `r`, laid out from the
    /// right so the title's cell takes what is left.
    ///
    /// **The one place this arithmetic exists**, as [`chip_rects`](Self::chip_rects)
    /// is for the filter row: `paint_row` draws into these and `hit_at`
    /// resolves a press on the list chip against them.
    pub(super) fn row_chip_rects(&self, r: Rect, idx: usize) -> Vec<Rect> {
        match self.row_chip_ws.get(idx) {
            Some(widths) => super::row::chip_rects(r, widths),
            None => Vec::new(),
        }
    }

    /// Take the page the panel answered with, and compute the one thing the
    /// query cannot carry.
    ///
    /// Called from `TabView::catch_up`, which runs **only for the tab about to
    /// be drawn** — so a Tasks tab sitting in the background does not re-read
    /// anything while somebody types in another one.
    ///
    /// **The residue triangle costs a walk, and only where one is possible.**
    /// SPEC §5's *completed but still holding unfinished work* is a fact about
    /// the task's own children, and no column of `TaskRow` carries it; the
    /// walk needs the document open. So it is computed for completed rows in
    /// **this window's open document only** — one `children()` plus a
    /// `classify_child` per child, bounded by the page — and a completed row
    /// living anywhere else is drawn without the triangle rather than with a
    /// guess. A false *absence* is quiet; a false *presence* would be a warning
    /// about nothing.
    pub fn catch_up(&mut self, ws: &WorkspaceDoc) -> bool {
        // The note settles first: it is a leaf over `ws` when `ws` is the
        // page's document, and nothing else about the page depends on it.
        let note = self.settle_body(ws);
        if !std::mem::take(&mut self.stale) {
            return note;
        }
        self.today = self
            .panel
            .day()
            .unwrap_or_else(|| chrono::Local::now().date_naive());
        self.now_ms = chrono::Utc::now().timestamp_millis();
        // **The wait ended: the store's rows have the write, or nothing will.**
        // Only the echoes whose command had landed go — one still waiting for
        // its receipt is a promise the store could not have kept yet. And no
        // echo outlives the bounded wait by more than a turn, whatever the
        // panel says, so a receipt that never came cannot leave a phantom.
        let before = self.echoes.len();
        let settled = self.panel.take_settled();
        if settled {
            self.echoes.retain(|e| !e.armed);
        }
        let by_settle = before - self.echoes.len();
        let ceiling = super::query::wait_ceiling() + std::time::Duration::from_secs(1);
        let after_settle = self.echoes.len();
        self.echoes.retain(|e| e.since.elapsed() < ceiling);
        let by_ceiling = after_settle - self.echoes.len();
        if before > 0 || settled {
            // **What happened to the rows drawn over the store's answer.** A
            // count that falls to zero on an answer the write cannot be in is
            // the row coming back under the pointer.
            crate::tasks::debug_line(format_args!(
                "page: echoes {before} -> {} (settled={settled}, dropped {by_settle} spent,                  {by_ceiling} expired)",
                self.echoes.len(),
            ));
        }
        self.rows = self.panel.rows().to_vec();
        self.apply_echoes();
        let here = ws.id();
        for row in &mut self.rows {
            row.residue = row.completed
                && here.is_some_and(|id| id == row.workspace)
                && koru_core::tasks::has_unfinished_structural_child(ws, row.node).unwrap_or(false);
        }
        // A selection whose row has left the page is dropped rather than
        // silently re-pointed: the keyboard must not end up on a task nobody
        // put it on.
        if self.selected_row().is_none() {
            self.clear_selection();
        }
        let rows = &self.rows;
        self.also.retain(|&(document, node)| {
            rows.iter()
                .any(|row| row.document == document && row.node == node)
        });
        self.relayout();
        self.clamp_scroll();
        true
    }

    /// Rebuild [`lines`](Self::lines) from [`rows`](Self::rows).
    ///
    /// **Rule labels are Upcoming's alone, and only while the page is in date
    /// order.** The design gives them to the one bucket whose span is a month;
    /// Today has no weeks in it, and putting a rule over Later's undated work
    /// would be a heading for a group that is not a week. The second half is the
    /// one V-3 had to add: the labels are read off the rows *in order*, so a
    /// page sorted by priority or by modification would print `Next week` three
    /// times down the list. A group is only a group while the thing it groups by
    /// is what the rows are sorted on.
    pub(super) fn relayout(&mut self) {
        // **Completed offers no capture row**, and the empty state has always
        // said so — `states::empty`'s `action` is
        // `target != RailTarget::Bucket(Bucket::Completed)`. The list itself
        // disagreed with it and drew the row anyway on every target, which was
        // invisible until a capture started landing in the queue it was typed
        // into (finding 28): the seed comes from
        // `koru_core::tasks::first_day_in`, and Completed is one of the two
        // buckets no date reaches — `bucket` answers it from the *lifecycle*,
        // before it reads the due at all. So the row could take the words and
        // could not keep the promise, and the task appeared in Later.
        //
        // Completed is a log of what was finished, not a place work is added.
        // Making the two halves agree in the direction the empty state already
        // chose is one line; the reverse — capturing an already-completed task
        // — would be a new product decision about what typing there means.
        self.remeasure_row_chips();
        let mut lines = if self.target == RailTarget::Bucket(Bucket::Completed) {
            Vec::new()
        } else {
            vec![Line::Capture]
        };
        let grouped = self.target == RailTarget::Bucket(Bucket::Upcoming)
            && self.filters.sort == TaskSort::Due;
        let mut group: Option<Rule> = None;
        for (i, row) in self.rows.iter().enumerate() {
            if grouped && let Some(day) = row.due_day() {
                let rule = Rule::of(day, self.today);
                if group != Some(rule) {
                    group = Some(rule);
                    lines.push(Line::Rule(rule.label(self.today)));
                }
            }
            lines.push(Line::Task(i));
        }
        self.lines = lines;
    }

    // ---------------------------------------------------------- the header

    /// The colour square ahead of a list's name — `1d`'s header band.
    ///
    /// **The rail's swatch, at the heading's scale**, and the same accent token
    /// rather than a colour of its own: the square in the sidebar and the square
    /// on the page are the one list, and a list wearing two colours is a bug you
    /// find by looking. `Some(None)` is a list whose colour the table has
    /// forgotten, which draws the neutral square the rail draws; `None` is a
    /// queue, which has no square at all.
    pub fn title_swatch(&self) -> Option<Option<String>> {
        match self.target {
            RailTarget::Reminders => unreachable!(
                "the rail's Reminders row swaps the tab's view; a Tasks page never targets it"
            ),
            RailTarget::Bucket(_) => None,
            RailTarget::List(id) => Some(self.list_face(id).and_then(|face| face.color.clone())),
        }
    }

    /// Where the heading's title started when it was placed by hand: past the
    /// swatch on a list page, at [`PAD`] everywhere else. The heading's
    /// element row places it now; the suite holds the row to this.
    #[cfg(test)]
    pub(super) fn title_x(&self) -> f32 {
        match self.title_swatch() {
            Some(_) => PAD + HEADER_SWATCH + SWATCH_GAP,
            None => PAD,
        }
    }

    /// What the title reads. A bucket says its own name; a list says its own,
    /// falling back to the neutral word rather than to an id.
    pub fn title(&self) -> String {
        match self.target {
            RailTarget::Reminders => unreachable!(
                "the rail's Reminders row swaps the tab's view; a Tasks page never targets it"
            ),
            RailTarget::Bucket(bucket) => {
                crate::panes::explore::rail::bucket_label(bucket).to_owned()
            }
            RailTarget::List(id) => self
                .list_face(id)
                .map(|face| face.name.clone())
                .unwrap_or_else(|| "List".to_owned()),
        }
    }

    /// The date beside the title. **Today only** — everywhere else the date is
    /// the row's business, and a header date on Upcoming would name a day
    /// nothing in the list is due on.
    pub(super) fn title_date(&self) -> Option<String> {
        (self.target == RailTarget::Bucket(Bucket::Today))
            .then(|| koru_ui::dates::date_label(self.today))
    }

    /// The count beside the title, or `None` before the first answer — a `0`
    /// meaning *we have not looked yet* is a lie a reader cannot see through.
    pub(super) fn count_label(&self) -> Option<String> {
        if !self.panel.known() {
            return None;
        }
        let n = self.panel.open_count();
        Some(match self.target {
            // Completed is an archive; "12 open" would be a contradiction.
            RailTarget::Bucket(Bucket::Completed) => format!("{}", self.rows.len()),
            _ => format!("{n} open"),
        })
    }

    // ----------------------------------------------------------- the chips

    /// The `…` at the page's right-hand end. The Trash tab's header button, at
    /// the Trash tab's arithmetic.
    pub(super) fn header_dots(&self) -> Rect {
        Rect::new(
            self.viewport.0 - PAD - HEADER_DOTS,
            self.title_top(),
            HEADER_DOTS,
            HEADER_DOTS,
        )
    }

    // ---------------------------------------------------------- the states

    /// Which of the three empty states the page is in, or `None` when it has
    /// rows — or has not been answered yet.
    ///
    /// **The one gate**, asked by drawing and by the hit test, so a control
    /// cannot appear in a state that does not offer one.
    ///
    /// **Every argument is this page's own answer.** The rail's counts used to
    /// be a fourth, and were the only input here that described something other
    /// than the query this surface had asked — which is exactly how the page
    /// came to say *you have no tasks* beside a rail saying otherwise.
    pub(super) fn empty_state(&self) -> Option<Empty> {
        if !self.rows.is_empty() {
            return None;
        }
        states::empty_state(
            self.panel.known(),
            self.panel.updating(),
            self.panel.notice(),
        )
    }

    /// The band an empty state's row sits in: the line **under** the capture
    /// row, which is exactly where the first task would be. Nothing is centred
    /// in the page's dead space — the design's own sentence about this.
    pub(super) fn empty_rect(&self) -> Rect {
        let r = self.table().row_rect(&self.bands(), 0);
        Rect::new(r.x, r.y + r.h, r.w, self.row_h())
    }

    /// The chip that row is drawn on.
    ///
    /// **The table's own row fill, spelled from its metrics** rather than from
    /// numbers of its own: an empty state sits *in* the list, so its chip is the
    /// shape a row's fill would have had, to the pixel.
    pub(super) fn empty_chip(&self) -> Rect {
        let m = self.table().metrics;
        let r = self.empty_rect();
        let inset = m.pad - m.row_pad_x;
        Rect::new(
            r.x + inset,
            r.y + m.row_pad_y,
            (r.w - inset * 2.0).max(0.0),
            (r.h - m.row_pad_y * 2.0).max(0.0),
        )
    }

    /// The corner radius that chip carries — the table's, for the same reason.
    pub(super) fn empty_radius(&self) -> f32 {
        self.table().metrics.row_radius
    }

    // ------------------------------------------------------- the manual order

    /// Whether a press on a row arms a drag: every Tasks page. Where the
    /// rows can land is the page's question ([`manual_order`]) and the
    /// sidebar's (a list row).
    ///
    /// [`manual_order`]: Self::manual_order
    pub(super) fn drags_rows(&self) -> bool {
        !matches!(self.target, RailTarget::Reminders)
    }

    /// Whether rows are being carried past the threshold — what lets the
    /// sidebar offer itself as a target.
    pub(crate) fn carrying_rows(&self) -> bool {
        self.drag.as_ref().is_some_and(|drag| drag.active)
    }

    /// Whether the button is down on a row — the window keeps the pointer
    /// on this page until it comes up, wherever it goes.
    pub(crate) fn holding_rows(&self) -> bool {
        self.drag.is_some()
    }

    /// The carried rows are over a list in the sidebar (`true`) or back
    /// over the page. Answers whether that changed.
    pub(crate) fn set_rows_outside(&mut self, outside: bool) -> bool {
        let Some(drag) = self.drag.as_mut() else {
            return false;
        };
        let changed = drag.outside != outside;
        drag.outside = outside;
        if outside {
            drag.gap = None;
        }
        changed
    }

    /// **The carried rows dropped on a list in the sidebar**: they join it,
    /// one request per task through the list picker's own door — shown at
    /// once by their echoes. `false` when nothing was being carried.
    pub(crate) fn drop_rows_into_list(&mut self, list: ListId) -> bool {
        let Some(drag) = self.drag.take() else {
            return false;
        };
        if !drag.active {
            return false;
        }
        let mut requests: Vec<Request> = drag
            .moving
            .iter()
            .map(|&(document, node)| Request::SetList {
                document,
                node,
                list: Some(list),
            })
            .collect();
        self.pending = match requests.len() {
            0 => None,
            1 => requests.pop(),
            _ => Some(Request::Many(requests)),
        };
        true
    }

    /// Whether this page is ordered by hand — a list (by default) or a queue
    /// set to *Manual* (not Completed): where a row can be dragged, and where a
    /// typed task goes on top.
    pub(super) fn manual_order(&self) -> bool {
        match self.target {
            RailTarget::List(_) => self.filters.list_sort == TaskSort::Manual,
            RailTarget::Bucket(Bucket::Completed) | RailTarget::Reminders => false,
            RailTarget::Bucket(_) => self.filters.sort == TaskSort::Manual,
        }
    }

    /// A press on row `i` of a list ordered by hand: a drag is armed,
    /// carrying the selection when the row is part of it and the row alone
    /// (selected) otherwise. **No grip**: the row itself is what is held,
    /// the owner's ask — press, and moving starts the drag.
    pub(super) fn begin_drag(&mut self, i: usize, y: f32) -> bool {
        let narrow_to = if self.is_selected(i) {
            (self.selection().len() > 1).then_some(i)
        } else {
            self.select_row(i);
            None
        };
        self.drag = Some(RowDrag {
            moving: self.selected_tasks(),
            start_y: y,
            narrow_to,
            outside: false,
            active: false,
            gap: None,
        });
        true
    }

    /// The pointer moved with a row held: past a few pixels it is a
    /// drag, and the gap under the pointer is where the rows would land.
    /// Near the band's top or bottom edge the page scrolls under it.
    pub(super) fn drag_to(&mut self, y: f32) -> bool {
        const EDGE: f32 = 24.0;
        const STEP: f32 = 12.0;
        let Some(drag) = self.drag.as_ref() else {
            return false;
        };
        let active = drag.active || (y - drag.start_y).abs() > 3.0;
        let outside = drag.outside;
        // Over the sidebar the page is not what is being aimed at: it holds
        // still rather than scrolling under a pointer that left it.
        if active && !outside {
            let top = self.header_h;
            if y < top + EDGE {
                self.scroll -= STEP;
                self.clamp_scroll();
            } else if y > self.viewport.1 - EDGE {
                self.scroll += STEP;
                self.clamp_scroll();
            }
        }
        // A gap only where the page has an order of its own to put them in,
        // and never while the rows are over a list in the sidebar.
        let gap = (active && !outside && self.manual_order()).then(|| self.drop_gap(y));
        let Some(drag) = self.drag.as_mut() else {
            return false;
        };
        let changed = drag.active != active || drag.gap != gap;
        drag.active = active;
        drag.gap = gap;
        changed
    }

    /// The gap at height `y`: before the first row whose middle is below
    /// it, or the end.
    pub(super) fn drop_gap(&self, y: f32) -> usize {
        let bands = self.bands();
        let table = self.table();
        for (line, kind) in self.lines.iter().enumerate() {
            if let Line::Task(row) = kind {
                let r = table.row_rect(&bands, line);
                if y < r.y + r.h * 0.5 {
                    return *row;
                }
            }
        }
        self.rows.len()
    }

    /// Where the gap is drawn: a line between two rows, or under the last.
    pub(super) fn drop_line_y(&self, gap: usize) -> Option<f32> {
        let bands = self.bands();
        let table = self.table();
        let rect_of = |row: usize| {
            self.lines
                .iter()
                .position(|l| l == &Line::Task(row))
                .map(|line| table.row_rect(&bands, line))
        };
        match rect_of(gap) {
            Some(r) => Some(r.y),
            None => gap.checked_sub(1).and_then(rect_of).map(|r| r.y + r.h),
        }
    }

    /// The release: the moved rows take ranks that put them in the gap,
    /// written as one request — and drawn there at once, by their echoes.
    /// A release that never became a drag was a click on the row, which
    /// selected the row and does nothing more.
    pub(super) fn drop_drag(&mut self) -> bool {
        let Some(drag) = self.drag.take() else {
            return false;
        };
        let (true, Some(gap), true) = (drag.active, drag.gap, self.manual_order()) else {
            if let Some(i) = drag.narrow_to {
                self.select_row(i);
            }
            return true;
        };
        let moving: Vec<usize> = (0..self.rows.len())
            .filter(|&i| {
                let key = (self.rows[i].document, self.rows[i].node);
                drag.moving.contains(&key)
            })
            .collect();
        let ranks: Vec<Option<i64>> = self.rows.iter().map(|row| row.rank).collect();
        let mut requests: Vec<Request> = super::rank::assign(&ranks, &moving, gap)
            .into_iter()
            .map(|(i, rank)| Request::SetRank {
                document: self.rows[i].document,
                node: self.rows[i].node,
                rank: Some(rank),
            })
            .collect();
        self.pending = match requests.len() {
            0 => None,
            1 => requests.pop(),
            _ => Some(Request::Many(requests)),
        };
        true
    }

    // ------------------------------------------------------- the selection

    /// Where the keyboard is, as an index into [`rows`](Self::rows).
    pub(super) fn selected_row(&self) -> Option<usize> {
        let (document, node) = self.selected?;
        self.rows
            .iter()
            .position(|row| row.document == document && row.node == node)
    }

    pub(super) fn select_row(&mut self, i: usize) {
        self.selected = self.rows.get(i).map(|row| (row.document, row.node));
        self.also.clear();
    }

    /// No row selected.
    fn clear_selection(&mut self) {
        self.selected = None;
        self.also.clear();
    }

    /// Whether row `i` is part of the selection.
    pub(super) fn is_selected(&self, i: usize) -> bool {
        self.rows.get(i).is_some_and(|row| {
            let key = (row.document, row.node);
            self.selected == Some(key) || self.also.contains(&key)
        })
    }

    /// Every selected row's index, in the page's order.
    pub(super) fn selection(&self) -> Vec<usize> {
        (0..self.rows.len())
            .filter(|&i| self.is_selected(i))
            .collect()
    }

    /// The selected tasks, in the page's order.
    pub(super) fn selected_tasks(&self) -> Vec<(DocumentId, NodeId)> {
        self.selection()
            .into_iter()
            .map(|i| (self.rows[i].document, self.rows[i].node))
            .collect()
    }

    /// **Ctrl+click**: row `i` joins the selection, or leaves it. A row that
    /// joins becomes the focused one; the focused row leaving hands focus to
    /// another selected row.
    fn toggle_row(&mut self, i: usize) {
        let Some(key) = self.rows.get(i).map(|row| (row.document, row.node)) else {
            return;
        };
        if self.selected == Some(key) {
            self.selected = self.also.pop();
        } else if let Some(at) = self.also.iter().position(|k| *k == key) {
            self.also.remove(at);
        } else {
            if let Some(was) = self.selected.replace(key) {
                self.also.push(was);
            }
        }
    }

    /// **Shift+click**: every row from the focused one to row `i`, the
    /// focused row staying where the range started. With nothing focused it
    /// is a plain selection of `i`.
    fn extend_to(&mut self, i: usize) {
        let Some(from) = self.selected_row() else {
            self.select_row(i);
            return;
        };
        let (lo, hi) = (from.min(i), from.max(i));
        self.also = (lo..=hi.min(self.rows.len().saturating_sub(1)))
            .filter(|&j| j != from)
            .map(|j| (self.rows[j].document, self.rows[j].node))
            .collect();
    }

    /// Step the selection by rows, not by lines: a week rule is a label for what
    /// is under it and is never landed on.
    ///
    /// **From nothing, down selects the first and up the last** — the Trash
    /// tab's rule, so the two lists answer an arrow the same way.
    fn step(&mut self, delta: i32) -> bool {
        if self.rows.is_empty() {
            return false;
        }
        let next = match self.selected_row() {
            Some(i) => i
                .saturating_add_signed(delta as isize)
                .min(self.rows.len() - 1),
            None if delta > 0 => 0,
            None => self.rows.len() - 1,
        };
        self.select_row(next);
        self.reveal_selected();
        true
    }

    /// Bring the selected row into the band. A selection you cannot see is a
    /// keyboard that has left the page.
    fn reveal_selected(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        self.reveal_row(row);
    }

    /// Bring row `row` into the band, wherever the ask came from.
    ///
    /// The selection's own reveal above, and `Action::ScrollIntoView` from a
    /// client — *"a navigation request can reveal the target using existing
    /// scroll behaviour"*. One implementation, so a screen reader and an arrow
    /// key put a row in the same place.
    pub(super) fn reveal_row(&mut self, row: usize) {
        let Some(line) = self.lines.iter().position(|l| l == &Line::Task(row)) else {
            return;
        };
        let t = self.table();
        let r = t.row_rect(&self.bands(), line);
        let body = t.body();
        if r.y < body.y {
            self.scroll -= body.y - r.y;
        } else if r.y + r.h > body.y + body.h {
            self.scroll += (r.y + r.h) - (body.y + body.h);
        }
        self.clamp_scroll();
    }

    /// Put the rename field over row `i`'s title, holding its words with
    /// the caret at their end. Any other field or panel goes.
    pub(super) fn begin_rename(&mut self, i: usize) -> bool {
        let Some(row) = self.rows.get(i) else {
            return false;
        };
        let (document, node, title) = (row.document, row.node, row.title.clone());
        self.end_capture();
        self.picker = None;
        self.overflow = None;
        self.drag = None;
        self.select_row(i);
        let mut input = koru_editor::components::text_input::TextInput::new(
            &mut self.engine,
            theme::role::row(),
        );
        input.set_text(&mut self.engine, &title);
        input.move_to_end(&mut self.engine);
        self.rename = Some(Rename {
            document,
            node,
            before: title,
            input,
        });
        true
    }

    /// Keep the field's words: a request when they changed and are not
    /// empty — an empty name is a slip, not a rename — and the field goes
    /// either way. The row's emoji, which the page draws apart from the
    /// words, goes back in front of them.
    pub(super) fn commit_rename(&mut self) -> bool {
        let Some(rename) = self.rename.take() else {
            return false;
        };
        let words = rename.input.value().trim().to_owned();
        if words.is_empty() || words == rename.before {
            return true;
        }
        let icon = self
            .rows
            .iter()
            .find(|row| (row.document, row.node) == (rename.document, rename.node))
            .and_then(|row| row.icon.clone());
        let title = match icon {
            Some(icon) => format!("{icon} {words}"),
            None => words.clone(),
        };
        self.pending = Some(Request::Retitle {
            document: rename.document,
            node: rename.node,
            title,
        });
        true
    }

    fn rename_key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        match key {
            Key::Named(NamedKey::Enter) => self.commit_rename(),
            Key::Named(NamedKey::Escape) => {
                self.rename = None;
                true
            }
            _ => {
                if let Some(rename) = self.rename.as_mut() {
                    rename.input.key(&mut self.engine, key, text, mods);
                }
                true
            }
        }
    }

    /// Where the rename field stands: over the title of its row, between
    /// the kind glyph and the chips. `None` while nothing is renamed or its
    /// row is not on the page.
    pub(super) fn rename_field(&self) -> Option<Rect> {
        let rename = self.rename.as_ref()?;
        let idx = self
            .rows
            .iter()
            .position(|row| (row.document, row.node) == (rename.document, rename.node))?;
        let line = self.lines.iter().position(|l| l == &Line::Task(idx))?;
        let r = self.table().row_rect(&self.bands(), line);
        let row = &self.rows[idx];
        let has_icon = self.row_glyph(row).is_some() || row.icon.is_some();
        let widths = self.row_chip_ws.get(idx).cloned().unwrap_or_default();
        Some(super::row::title_field(r, has_icon, &widths))
    }

    /// Enter on the selected row: **the same request a click makes**, so the
    /// keyboard and the pointer cannot open two different things.
    pub(super) fn open_selected(&mut self, new_tab: bool) -> bool {
        let Some(row) = self.selected_row().and_then(|i| self.rows.get(i)) else {
            return false;
        };
        self.pending = Some(Request::Open {
            document: row.document,
            node: row.node,
            new_tab,
        });
        true
    }

    /// **Tick or untick row `i`** — the canonical command, whichever gesture
    /// asked for it.
    ///
    /// Lifted out of `press` when the accessibility projection became a second
    /// caller: the tick, the menu's *Complete* and a client's `Toggle()` are
    /// one operation, and three copies of the same three lines would be three
    /// places to disagree about which row was meant.
    pub(super) fn toggle_complete(&mut self, i: usize) -> bool {
        // The pointer moves the keyboard with it, so Enter and the menu key act
        // on the row somebody just touched rather than on one they left behind.
        self.select_row(i);
        let Some(row) = self.rows.get(i) else {
            return false;
        };
        self.pending = Some(Request::Complete {
            document: row.document,
            node: row.node,
            completed: !row.completed,
        });
        true
    }

    /// **Follow row `i`'s list chip.** The one chip that is a control; lifted
    /// out of `press` for [`toggle_complete`](Self::toggle_complete)'s reason.
    /// **The list chip pressed**: the list picker, to change it — at the
    /// pointer, or under the row's `…` when there is no pointer (a screen
    /// reader's click). A chip inside a selection of several decides for
    /// the whole selection, as the value chips do.
    pub(super) fn press_list_chip(&mut self, i: usize, at: Option<(f32, f32)>) -> bool {
        if !self.is_selected(i) {
            self.select_row(i);
        }
        let tasks = if self.selection().len() > 1 {
            self.selected_tasks()
        } else {
            self.rows
                .get(i)
                .map(|row| vec![(row.document, row.node)])
                .unwrap_or_default()
        };
        self.open_list_picker_at(&tasks, at)
    }
    /// The menu key over the selected row — anchored on that row's `…`, which
    /// is where the same menu opens from the pointer.
    pub(super) fn menu_for_selected(&mut self) -> bool {
        let Some(i) = self.selected_row() else {
            return false;
        };
        let Some(line) = self.lines.iter().position(|l| l == &Line::Task(i)) else {
            return false;
        };
        let d = self.parts(self.table().row_rect(&self.bands(), line)).dots;
        self.pending = self.row_menu_request(i, (d.x + d.w, d.y + d.h));
        self.pending.is_some()
    }

    /// **The menu for row `i`, opened at `at`** — the row's own, or, when `i`
    /// is one of several selected rows, the selection's. The one place either
    /// is built, for the `…`, a right-click and the menu key alike; it was
    /// written out three times.
    fn row_menu_request(&self, i: usize, at: (f32, f32)) -> Option<Request> {
        let selection = self.selection();
        if selection.len() > 1 && selection.contains(&i) {
            let rows: Vec<&TaskEntry> = selection.iter().map(|&j| &self.rows[j]).collect();
            return Some(Request::RowsMenu {
                tasks: rows.iter().map(|row| (row.document, row.node)).collect(),
                native: rows
                    .iter()
                    .filter(|row| self.task_documents.contains(&row.document))
                    .map(|row| (row.document, row.node))
                    .collect(),
                completed: rows.iter().all(|row| row.completed),
                at,
            });
        }
        let row = self.rows.get(i)?;
        Some(Request::RowMenu {
            document: row.document,
            node: row.node,
            completed: row.completed,
            pinned_today: row.pinned_today,
            native: self.task_documents.contains(&row.document),
            list: self.leads_to_list(row),
            at,
        })
    }

    /// Delete on the selection: ask the container to take it off Tasks.
    fn remove_selection(&mut self) -> bool {
        let tasks = self.selected_tasks();
        if tasks.is_empty() {
            return false;
        }
        self.pending = Some(Request::RemoveTasks { tasks });
        true
    }

    // -------------------------------------------------------- the geometry

    /// A row is two lines in Today and one everywhere else.
    ///
    /// The design's rule, in its own words: *away from Today, dates are cool
    /// grey and the context line folds inline — nothing here is urgent, so the
    /// row can be one line.* It is a property of the whole page rather than of
    /// a row, which is what lets it be a table metric instead of a second band.
    /// **One line, on every page** — the row component's height.
    pub(super) fn row_h(&self) -> f32 {
        super::row::row_h()
    }

    pub(super) fn table(&self) -> Table {
        Table::new(
            Rect::new(0.0, 0.0, self.viewport.0, self.viewport.1),
            Metrics::manager()
                .with_pad(PAD)
                .with_row_h(self.row_h())
                .with_header(self.rows_top())
                .with_footer(0.0)
                .roomy(),
            self.scroll,
        )
    }

    pub(super) fn bands(&self) -> Vec<Band> {
        self.lines
            .iter()
            .map(|line| match line {
                Line::Rule(_) => Band::Section,
                _ => Band::Row,
            })
            .collect()
    }

    /// Where the pieces of one row are, from its rectangle and nothing else.
    ///
    /// **The one place this arithmetic exists.** Drawing takes its rectangles
    /// from here and so does [`hit_at`](Self::hit_at), which is what
    /// `row_geometry_hits_back` proves: a control cannot be clickable where it
    /// is not drawn, and cannot be drawn where it is not clickable.
    pub(super) fn parts(&self, r: Rect) -> RowParts {
        super::row::parts(r)
    }

    fn measure_header(&mut self) {
        // The header is the heading's line: no page draws a filter row.
        self.header_h = self.title_top() + super::draw::TITLE.metrics.line_height + HEADER_GAP;
    }

    /// Where the title's layout box starts. **Less than [`PAD`], on purpose** —
    /// a text layout reserves half its leading above the glyphs, so a title
    /// drawn at `y = PAD` looks as though the page has more air on top than
    /// down the side. The Trash tab's rule, and the same arithmetic.
    pub(super) fn title_top(&self) -> f32 {
        let m = super::draw::TITLE.metrics;
        (PAD - (m.line_height - m.size) * 0.5).max(0.0)
    }

    pub(super) fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        // The header's own controls first: they sit above the row band, so no
        // row can claim them, but saying so here keeps the order of resolution
        // in one place rather than in the table's arithmetic.
        if self.header_dots().contains(x, y) {
            return Some(Hit::HeaderDots);
        }
        if self.swatch_rect().is_some_and(|r| r.contains(x, y)) {
            return Some(Hit::Swatch);
        }
        if let Some(hit) = self.band_hit(x, y) {
            return Some(hit);
        }
        let bands = self.bands();
        let i = self.table().row_at(&bands, x, y)?;
        let r = self.table().row_rect(&bands, i);
        match self.lines.get(i)? {
            Line::Capture => Some(Hit::Capture),
            // A rule is a label for what is under it, not a control.
            Line::Rule(_) => None,
            Line::Task(row) => {
                let p = self.parts(r);
                // Inside-out: the controls sit *in* the row, so they outrank it.
                if p.control.contains(x, y) {
                    return Some(Hit::Complete(*row));
                }
                if p.dots.contains(x, y) {
                    return Some(Hit::Dots(*row));
                }
                // **The chips that change something.** The list chip opens
                // its list, where it leads somewhere (finding 36); the due,
                // the priority and the colour open their picker. *In trash*
                // and a list chip on its own list's page stay labels.
                if let Some(entry) = self.rows.get(*row) {
                    let rects = self.row_chip_rects(r, *row);
                    let under = self
                        .row_chips(entry)
                        .into_iter()
                        .zip(rects)
                        .find(|(_, c)| c.contains(x, y))
                        .map(|(chip, _)| chip);
                    match under {
                        Some(chip) if chip.list.is_some() => {
                            return Some(Hit::ListChip(*row));
                        }
                        Some(chip)
                            if matches!(
                                chip.kind,
                                ChipKind::Due | ChipKind::Priority | ChipKind::Swatch
                            ) =>
                        {
                            return Some(Hit::ValueChip(*row, chip.kind));
                        }
                        _ => {}
                    }
                }
                Some(Hit::Row(*row))
            }
        }
    }

    /// Where line `i` is. Tests only — the container's translation test needs a
    /// control to press without reaching into the geometry line by line.
    #[cfg(test)]
    pub(super) fn line_rect_for_test(&self, i: usize) -> Rect {
        self.table().row_rect(&self.bands(), i)
    }

    /// The rest of the geometry seam the suite drives. **Test-only accessors
    /// rather than public methods**: what they expose is the arithmetic
    /// `hit_at` and `paint_row` already share, and widening it for a test would
    /// invite a third caller to compute a rectangle of its own.
    #[cfg(test)]
    pub(super) fn parts_for_test(&self, r: Rect) -> RowParts {
        self.parts(r)
    }

    #[cfg(test)]
    pub(super) fn hit_for_test(&self, x: f32, y: f32) -> Option<Hit> {
        self.hit_at(x, y)
    }

    #[cfg(test)]
    pub(super) fn visible_for_test(&self) -> Vec<usize> {
        self.table().visible(&self.bands())
    }

    #[cfg(test)]
    pub(super) fn row_h_for_test(&self) -> f32 {
        self.row_h()
    }

    #[cfg(test)]
    pub(super) fn line_of_task_for_test(&self, row: usize) -> Option<usize> {
        self.lines.iter().position(|l| l == &Line::Task(row))
    }

    /// Where task row `row` is drawn, in the view's own coordinates. Reached
    /// from the container's suite — which is why this one is `pub(crate)` where
    /// the rest of the seam is `pub(super)`: the drain a right-click owes runs
    /// in `MainView`, so the press that proves it has to be aimed from there.
    #[cfg(test)]
    pub(crate) fn task_row_rect_for_test(&self, row: usize) -> Option<Rect> {
        let line = self.lines.iter().position(|l| l == &Line::Task(row))?;
        Some(self.table().row_rect(&self.bands(), line))
    }

    #[cfg(test)]
    pub(super) fn rules_for_test(&self) -> Vec<String> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                Line::Rule(label) => Some(label.clone()),
                _ => None,
            })
            .collect()
    }

    /// The header band's three readings, as the suite asks them.
    #[cfg(test)]
    pub(super) fn title_for_test(&self) -> String {
        self.title()
    }

    #[cfg(test)]
    pub(super) fn title_swatch_for_test(&self) -> Option<Option<String>> {
        self.title_swatch()
    }

    #[cfg(test)]
    pub(super) fn title_x_for_test(&self) -> f32 {
        self.title_x()
    }

    #[cfg(test)]
    pub(super) fn count_label_for_test(&self) -> Option<String> {
        self.count_label()
    }

    #[cfg(test)]
    pub(super) fn picker_enabled_for_test(&self) -> Vec<bool> {
        self.picker_enabled()
    }

    /// What this tab makes of a list id — `pub(crate)` for the container's
    /// suite, which is where *the table reached the tab* is a fact about the
    /// push rather than about this view.
    #[cfg(test)]
    pub(crate) fn list_face_for_test(&self, id: ListId) -> Option<&ListFace> {
        self.list_face(id)
    }

    #[cfg(test)]
    pub(super) fn header_h_for_test(&self) -> f32 {
        self.header_h
    }

    /// Which empty state the page is in — the gate itself, so a test asserts on
    /// what is drawn rather than on a sentence it copied.
    #[cfg(test)]
    pub(super) fn empty_for_test(&self) -> Option<Empty> {
        self.empty_state()
    }

    #[cfg(test)]
    pub(super) fn selected_for_test(&self) -> Option<usize> {
        self.selected_row()
    }

    #[cfg(test)]
    pub(crate) fn capturing_for_test(&self) -> bool {
        self.capture.is_some()
    }

    /// What the capture row's field holds, while it is open.
    #[cfg(test)]
    pub(super) fn capture_value_for_test(&self) -> Option<String> {
        self.capture.as_ref().map(|capture| capture.value())
    }

    #[cfg(test)]
    pub(super) fn header_dots_for_test(&self) -> Rect {
        self.header_dots()
    }

    /// The open overflow menu's rows, or nothing when it is shut.
    #[cfg(test)]
    pub(super) fn overflow_for_test(&self) -> Option<Vec<OverflowItem>> {
        self.overflow.as_ref().map(|menu| menu.items().to_vec())
    }

    #[cfg(test)]
    pub(super) fn overflow_enabled_for_test(&self) -> Vec<bool> {
        self.overflow_enabled()
    }

    /// Where row `i` of the open menu is, so a test can press one without
    /// re-deriving the panel's arithmetic.
    #[cfg(test)]
    pub(super) fn overflow_row_rect_for_test(&self, i: usize) -> Option<Rect> {
        self.overflow
            .as_ref()
            .map(|menu| menu.item_rect(self.viewport, i))
    }

    /// The open membership picker's rows, or nothing while it is shut (or
    /// another picker is up).
    #[cfg(test)]
    pub(super) fn picker_for_test(&self) -> Option<Vec<ListPick>> {
        match &self.picker {
            Some(Picker::List(picker)) => Some(picker.menu.items().to_vec()),
            _ => None,
        }
    }

    /// The open colour picker's rows.
    #[cfg(test)]
    pub(super) fn color_picker_for_test(&self) -> Option<Vec<ColorPick>> {
        match &self.picker {
            Some(Picker::Color(picker)) => Some(picker.menu.items().to_vec()),
            _ => None,
        }
    }

    /// The open priority picker's rows.
    #[cfg(test)]
    pub(super) fn priority_picker_for_test(&self) -> Option<Vec<super::pickers::PriorityRow>> {
        match &self.picker {
            Some(Picker::Priority(picker)) => Some(picker.menu.items().to_vec()),
            _ => None,
        }
    }

    /// Whether the list picker is the panel standing.
    #[cfg(test)]
    pub(crate) fn list_picker_for_test(&self) -> Option<()> {
        matches!(self.picker, Some(Picker::List(_))).then_some(())
    }

    /// The open calendar's seeds — the day it is showing and whether it offers
    /// Clear — and where it is.
    #[cfg(test)]
    pub(crate) fn due_picker_for_test(&self) -> Option<(NaiveDate, bool, Rect)> {
        match &self.picker {
            Some(Picker::Due(pick)) => {
                Some((pick.grid.selected, pick.grid.offers_clear, pick.panel))
            }
            _ => None,
        }
    }

    /// Whether any picker is up.
    #[cfg(test)]
    pub(crate) fn has_picker_for_test(&self) -> bool {
        self.picker.is_some()
    }

    /// Where row `i` of the open picker is, so a test can press one without
    /// re-deriving the panel's arithmetic.
    ///
    /// **`pub(crate)`, unlike its neighbours**: the container's suite presses
    /// this panel *through* `MainView`, which is the only place the whole hop —
    /// menu row, picker, request, command — is one gesture.
    #[cfg(test)]
    pub(crate) fn picker_row_rect_for_test(&self, i: usize) -> Option<Rect> {
        match &self.picker {
            Some(Picker::List(picker)) => Some(picker.menu.item_rect(self.viewport, i)),
            Some(Picker::Priority(picker)) => Some(picker.menu.item_rect(self.viewport, i)),
            _ => None,
        }
    }

    /// What one row's chips say — the same list `paint_row` measures and draws.
    #[cfg(test)]
    pub(super) fn row_chips_for_test(&self, row: usize) -> Vec<RowChip> {
        self.rows
            .get(row)
            .map(|row| self.row_chips(row))
            .unwrap_or_default()
    }

    /// Leave a request the way a press would, for the container's tests.
    #[cfg(test)]
    pub(crate) fn request_for_test(&mut self, request: Request) {
        self.pending = Some(request);
    }

    /// Where one row's chips are, in the page's own coordinates.
    #[cfg(test)]
    pub(super) fn row_chip_rects_for_test(&self, row: usize) -> Vec<Rect> {
        let Some(line) = self.lines.iter().position(|l| l == &Line::Task(row)) else {
            return Vec::new();
        };
        let r = self.table().row_rect(&self.bands(), line);
        self.row_chip_rects(r, row)
    }

    /// Whether this page draws the capture row at all. Every target but
    /// Completed does.
    #[cfg(test)]
    pub(super) fn has_capture_line_for_test(&self) -> bool {
        self.lines.contains(&Line::Capture)
    }

    /// What the capture row's preview would commit as the title.
    #[cfg(test)]
    pub(super) fn capture_preview_for_test(&self) -> Option<String> {
        self.capture.as_ref().and_then(|c| c.word_head())
    }

    /// The capture row's picks, for the suite.
    #[cfg(test)]
    pub(super) fn capture_picks_for_test(
        &self,
    ) -> Option<(
        Option<koru_core::time::Temporal>,
        Option<Priority>,
        Option<ListId>,
    )> {
        self.capture
            .as_ref()
            .map(|c| (c.due.clone(), c.priority, c.list))
    }

    /// Whether the capture row's `/` menu is up, for the suite.
    #[cfg(test)]
    pub(super) fn capture_menu_open_for_test(&self) -> bool {
        self.capture.as_ref().is_some_and(|c| c.menu_open())
    }

    /// A picker's decision, applied as a press or a key would apply it.
    #[cfg(test)]
    pub(super) fn settle_for_test(&mut self, choice: Choice) -> bool {
        self.settle(choice)
    }

    /// A day pressed in the open calendar, through the calendar's own answer
    /// for whatever it was opened for.
    #[cfg(test)]
    pub(super) fn pick_day_for_test(&mut self, day: NaiveDate) -> bool {
        let Some(Picker::Due(pick)) = &self.picker else {
            return false;
        };
        let choice = pick.resolve(koru_ui::date_grid::Pick::Day(day));
        self.settle(choice)
    }

    /// Put a page in without a store. **The tests build what they assert on**,
    /// which is also what keeps this suite from dialling anything.
    ///
    /// `pub(crate)` for the reason
    /// [`task_row_rect_for_test`](Self::task_row_rect_for_test) is: the drains a
    /// gesture owes run in `MainView`, so the container's own suite has to be
    /// able to stand a page up before it can prove one.
    #[cfg(test)]
    pub(crate) fn seed_for_test(&mut self, rows: Vec<TaskEntry>, today: NaiveDate) {
        self.rows = rows;
        self.today = today;
        self.relayout();
        self.stale = false;
    }

    fn clamp_scroll(&mut self) {
        let max = self.table().max_scroll(&self.bands());
        self.scroll = self.scroll.clamp(0.0, max);
    }

    // --------------------------------------------------------- the capture

    pub(super) fn capture_rect(&self) -> Rect {
        let r = self.table().row_rect(&self.bands(), 0);
        self.parts(r).text
    }

    pub(crate) fn begin_capture(&mut self) {
        if self.capture.is_none() {
            let mut capture = Capture::new(&mut self.engine);
            capture.set_anchor(self.capture_anchor());
            self.capture = Some(capture);
        }
    }

    /// Where the capture row's `/` menu and pickers hang from: its `…`, the
    /// anchor every picker on this page uses.
    fn capture_anchor(&self) -> (f32, f32) {
        let r = self.table().row_rect(&self.bands(), 0);
        let d = self.parts(r).dots;
        (d.x + d.w, d.y + d.h)
    }

    fn end_capture(&mut self) {
        self.capture = None;
    }

    /// What the capture row would create, or `None` when there is no title.
    ///
    /// **The title is what is left after the date word**, which is what the
    /// preview chip has been saying since the word was typed — so committing
    /// produces the task the row was showing rather than a different one.
    fn capture_request(&self) -> Option<Request> {
        let capture = self.capture.as_ref()?;
        let title = capture.title();
        if title.is_empty() {
            return None;
        }
        let due = capture.typed_due();
        // **A task lands in the queue it was typed into.** With no date word
        // the capture used to commit `due: None`, and a dateless task is
        // *Later* — so typing three tasks into Today filed all three somewhere
        // the reader was not looking, and they read as tasks that were never
        // created. Membership in a *list* is a filter the capture sets; a
        // bucket is not a filter over anything, it is derived from the due, so
        // "not a place" turned into "not here".
        //
        // The date comes from `koru_core::tasks::first_day_in`, which is
        // `bucket`'s inverse and is written out of the same constants. A table
        // of day offsets here would be a second answer about the boundaries the
        // moment either moves.
        //
        // **A typed date word always wins** — it is a default, never an
        // override, so `Chase it friday` on the Today page is still due Friday.
        // *Later* seeds nothing, because a dateless task is already Later and a
        // date there would be a claim nobody made.
        let due = due.or_else(|| {
            let RailTarget::Bucket(bucket) = self.target else {
                return None;
            };
            koru_core::tasks::first_day_in(bucket, koru_core::time::DateOnly::new(self.today))
                .map(koru_core::time::Temporal::Date)
        });
        Some(Request::Capture {
            title,
            due,
            // **A task typed into a list carries that list** (SPEC §8) unless
            // `/task_list` named another. A task captured in a bucket belongs
            // to no list unless it did, and the view still shows it —
            // membership is a filter, never a place.
            list: capture.list.or(match self.target {
                RailTarget::List(id) => Some(id),
                RailTarget::Bucket(_) => None,
                RailTarget::Reminders => unreachable!(
                    "the rail's Reminders row swaps the tab's view; a Tasks page never targets it"
                ),
            }),
            priority: capture.priority,
            // **On top of a list ordered by hand**, where it was typed.
            rank: self.manual_order().then(|| {
                let ranks: Vec<Option<i64>> = self.rows.iter().map(|row| row.rank).collect();
                super::rank::top(&ranks)
            }),
        })
    }

    // -------------------------------------------------------- the overflow

    /// Open the overflow menu under the `…`.
    ///
    /// **Only opens.** Pressing the `…` while the panel is up never reaches
    /// here: an open menu takes the press first and a press outside it is a
    /// dismissal, which is what makes the button read as a toggle without a
    /// second piece of state deciding it.
    pub(super) fn open_overflow(&mut self) -> bool {
        let d = self.header_dots();
        let items = super::menu::overflow_items(&self.filters, self.target);
        // One opening, one number, never reused. See `panel_instance`.
        self.panel_instance += 1;
        // The button's bottom-**right**, so the panel hangs back under it rather
        // than out past the window's edge — the anchor every `…` in this window
        // uses.
        self.overflow = Some(Menu::aligned(
            &mut self.engine,
            (d.x + d.w, d.y + d.h),
            items,
            Anchor::ButtonRight,
        ));
        true
    }

    /// Which rows of the open menu may be chosen.
    ///
    /// The heading and the rule never; *Show completed inline* only where the
    /// store applies a lifecycle, which is the one place its own
    /// `disabled_reason` is drawn.
    pub(super) fn overflow_enabled(&self) -> Vec<bool> {
        // **Everywhere but Completed**, where every row is completed already
        // and the switch would be asking a page to show what it is made of.
        let recent = self.target != RailTarget::Bucket(Bucket::Completed);
        self.overflow
            .as_ref()
            .map(|menu| {
                menu.items()
                    .iter()
                    .map(|item| match item.row {
                        OverflowRow::Divider | OverflowRow::SortHeading => false,
                        OverflowRow::RecentlyCompleted => recent,
                        _ => true,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Choose one row of the overflow menu.
    ///
    /// **The menu stays open.** It is a setting menu, not a block menu: the
    /// check has to be seen to move, and two of these are usually changed
    /// together. Closing it is Escape, the `…` again, or a press anywhere else.
    pub(super) fn choose_overflow(&mut self, row: usize) -> bool {
        if self.overflow_enabled().get(row) != Some(&true) {
            return false;
        }
        let Some(item) = self
            .overflow
            .as_ref()
            .and_then(|menu| menu.items().get(row))
            .copied()
        else {
            return false;
        };
        let mut f = self.filters;
        match item.row {
            OverflowRow::RecentlyCompleted => f.recently_completed = !f.recently_completed,
            OverflowRow::Sort(sort) => f.set_sort_for(self.target, sort),
            OverflowRow::Divider | OverflowRow::SortHeading => return false,
        }
        self.set_filters(f);
        if let Some(menu) = &mut self.overflow {
            super::menu::sync(menu.items_mut(), &f, self.target);
        }
        true
    }

    // --------------------------------------------------- the membership picker

    /// Open the membership picker over one row, anchored where its `…` is.
    ///
    /// **Anchored at the row rather than at the pointer**, which is what lets
    /// the container ask for it without translating a coordinate: the menu row
    /// that asks for this was itself anchored on that `…`, so the picker lands
    /// where the menu it replaced was.
    ///
    /// Answers `false` for a row this page is not showing — the page it came
    /// off can be half a minute old, and a picker over nothing would write into
    /// a task the reader cannot see.
    pub fn open_list_picker(&mut self, document: DocumentId, node: NodeId) -> bool {
        self.open_list_picker_for(&[(document, node)])
    }

    /// The membership picker for **several rows** — a selection's *List* —
    /// over the first of them this page shows. A list is checked only when
    /// every one of them is in it, which is also what makes choosing it a
    /// no-op.
    pub fn open_list_picker_for(&mut self, tasks: &[(DocumentId, NodeId)]) -> bool {
        self.open_list_picker_at(tasks, None)
    }

    fn open_list_picker_at(
        &mut self,
        tasks: &[(DocumentId, NodeId)],
        at: Option<(f32, f32)>,
    ) -> bool {
        let Some((rows, d)) = self.rows_anchor(tasks) else {
            return false;
        };
        let (at, align) = menu_anchor(d, at);
        let current = common(rows.iter().map(|&i| self.rows[i].list)).flatten();
        let items = super::menu::list_picker_items(&self.lists, current);
        self.overflow = None;
        self.panel_instance += 1;
        self.picker = Some(Picker::List(ListPicker {
            target: self.rows_target(&rows),
            menu: Menu::aligned(&mut self.engine, at, items, align),
        }));
        true
    }

    /// The row a picker is asked for, and the `…` it hangs from — or nothing,
    /// for a row this page is not showing.
    fn row_anchor(&self, document: DocumentId, node: NodeId) -> Option<(usize, Rect)> {
        let i = self
            .rows
            .iter()
            .position(|row| row.document == document && row.node == node)?;
        let line = self.lines.iter().position(|l| l == &Line::Task(i))?;
        Some((
            i,
            self.parts(self.table().row_rect(&self.bands(), line)).dots,
        ))
    }

    /// The rows of `tasks` this page shows, in its order, and the `…` of the
    /// first — or nothing when it shows none of them. A task that left the
    /// page since its menu opened is left out rather than written blind.
    fn rows_anchor(&self, tasks: &[(DocumentId, NodeId)]) -> Option<(Vec<usize>, Rect)> {
        let mut rows: Vec<usize> = tasks
            .iter()
            .filter_map(|&(document, node)| {
                self.rows
                    .iter()
                    .position(|row| row.document == document && row.node == node)
            })
            .collect();
        rows.sort_unstable();
        rows.dedup();
        let first = &self.rows[*rows.first()?];
        let (_, d) = self.row_anchor(first.document, first.node)?;
        Some((rows, d))
    }

    /// The picker target for these rows.
    fn rows_target(&self, rows: &[usize]) -> Target {
        Target::Rows(
            rows.iter()
                .map(|&i| (self.rows[i].document, self.rows[i].node))
                .collect(),
        )
    }

    /// *Priority* over one row: the editor's four levels in the scale's own
    /// order, the row's current one checked, Clear while one is set. The same
    /// anchoring and the same refusal as [`open_list_picker`](Self::open_list_picker).
    pub fn open_priority_picker(&mut self, document: DocumentId, node: NodeId) -> bool {
        self.open_priority_picker_for(&[(document, node)])
    }

    /// *Priority* for several rows: a level is checked only when every one
    /// of them has it.
    pub fn open_priority_picker_for(&mut self, tasks: &[(DocumentId, NodeId)]) -> bool {
        self.open_priority_picker_at(tasks, None)
    }

    fn open_priority_picker_at(
        &mut self,
        tasks: &[(DocumentId, NodeId)],
        at: Option<(f32, f32)>,
    ) -> bool {
        let Some((rows, d)) = self.rows_anchor(tasks) else {
            return false;
        };
        let (at, align) = menu_anchor(d, at);
        let current = common(
            rows.iter()
                .map(|&i| self.rows[i].priority.as_deref().and_then(Priority::parse)),
        )
        .flatten();
        let items = priority_rows(current);
        self.overflow = None;
        self.panel_instance += 1;
        self.picker = Some(Picker::Priority(PriorityPick {
            target: self.rows_target(&rows),
            menu: Menu::aligned(&mut self.engine, at, items, align),
        }));
        true
    }

    /// *Due* over one row: the shared calendar, opened on the row's own day
    /// with Clear offered when it has one, else on today — the editor's rule
    /// for its picker, read off the row.
    ///
    /// **The one clock read on this path**, handed to the grid so its phrase
    /// field resolves against the same instant the panel opened on; the
    /// widget reads no clock of its own.
    pub fn open_due_picker(&mut self, document: DocumentId, node: NodeId) -> bool {
        self.open_due_picker_for(&[(document, node)])
    }

    /// *Due* for several rows: opened on their day when they share one, else
    /// on today, with Clear offered while any of them has a date.
    pub fn open_due_picker_for(&mut self, tasks: &[(DocumentId, NodeId)]) -> bool {
        self.open_due_picker_at(tasks, None)
    }

    fn open_due_picker_at(
        &mut self,
        tasks: &[(DocumentId, NodeId)],
        at: Option<(f32, f32)>,
    ) -> bool {
        let Some((rows, d)) = self.rows_anchor(tasks) else {
            return false;
        };
        let now = chrono::Local::now().naive_local();
        let day = common(rows.iter().map(|&i| self.rows[i].due_day())).flatten();
        let (selected, _) = due_seeds(day, self.today);
        let offers_clear = rows.iter().any(|&i| self.rows[i].due_day().is_some());
        let grid = DateGrid::with_phrase_field(&mut self.engine, selected, now, offers_clear);
        let panel = match at {
            Some((x, y)) => {
                DuePick::place_under(Rect::new(x, y, 0.0, 0.0), grid.size(), self.viewport)
            }
            None => DuePick::place(d, grid.size(), self.viewport),
        };
        self.overflow = None;
        self.panel_instance += 1;
        self.picker = Some(Picker::Due(DuePick {
            target: self.rows_target(&rows),
            grid,
            panel,
            clear_hovered: false,
        }));
        true
    }

    /// The colour picker for these rows, at `at` when a chip was pressed: the
    /// accent colours, the line's own checked while every row shares it.
    fn open_color_picker_at(
        &mut self,
        tasks: &[(DocumentId, NodeId)],
        at: Option<(f32, f32)>,
    ) -> bool {
        let Some((rows, d)) = self.rows_anchor(tasks) else {
            return false;
        };
        let (at, align) = menu_anchor(d, at);
        let current = common(rows.iter().map(|&i| self.rows[i].color.clone())).flatten();
        let items = super::menu::color_picker_items(current.as_deref());
        self.overflow = None;
        self.panel_instance += 1;
        self.picker = Some(Picker::Color(ColorPicker {
            target: self.rows_target(&rows),
            menu: Menu::aligned(&mut self.engine, at, items, align),
        }));
        true
    }

    /// **A value chip pressed** — its due, its priority or its colour: that
    /// value's picker, **at the pointer**. A chip on a row inside a selection
    /// of several decides for the whole selection, as the row's menu there
    /// does; any other row becomes the selection first.
    fn press_value_chip(&mut self, i: usize, kind: ChipKind, at: (f32, f32)) -> bool {
        if !self.is_selected(i) {
            self.select_row(i);
        }
        let selection = self.selection();
        let tasks: Vec<(DocumentId, NodeId)> = if selection.len() > 1 {
            self.selected_tasks()
        } else {
            self.rows
                .get(i)
                .map(|row| vec![(row.document, row.node)])
                .unwrap_or_default()
        };
        match kind {
            ChipKind::Due => self.open_due_picker_at(&tasks, Some(at)),
            ChipKind::Priority => self.open_priority_picker_at(&tasks, Some(at)),
            ChipKind::Swatch => self.open_color_picker_at(&tasks, Some(at)),
            ChipKind::Tag => false,
        }
    }

    /// A `/` row chosen on the capture: the same three pickers the row's `…`
    /// hangs, over the capture row, deciding for it rather than for a task.
    fn open_capture_picker(&mut self, row: SlashRow) -> bool {
        let Some(capture) = self.capture.as_ref() else {
            return false;
        };
        let r = self.table().row_rect(&self.bands(), 0);
        let d = self.parts(r).dots;
        // **Where the command was typed**, when the row knows: the picker
        // hangs under the `/`, top-left at it. Else the row's `…`, as a row's
        // own pickers do.
        let trigger = capture.trigger();
        let (at, align) = match trigger {
            Some(t) => ((t.x, t.y + t.h), Anchor::Cursor),
            None => ((d.x + d.w, d.y + d.h), Anchor::ButtonRight),
        };
        self.overflow = None;
        self.panel_instance += 1;
        match row {
            SlashRow::TaskList => {
                let current = capture.list.or(match self.target {
                    RailTarget::List(id) => Some(id),
                    RailTarget::Bucket(_) => None,
                    RailTarget::Reminders => unreachable!(
                        "the rail's Reminders row swaps the tab's view; a Tasks page never targets it"
                    ),
                });
                let items = super::menu::list_picker_items(&self.lists, current);
                self.picker = Some(Picker::List(ListPicker {
                    target: Target::Capture,
                    menu: Menu::aligned(&mut self.engine, at, items, align),
                }));
            }
            SlashRow::Priority => {
                let items = priority_rows(capture.priority);
                self.picker = Some(Picker::Priority(PriorityPick {
                    target: Target::Capture,
                    menu: Menu::aligned(&mut self.engine, at, items, align),
                }));
            }
            SlashRow::Due => {
                let now = chrono::Local::now().naive_local();
                let day =
                    capture
                        .typed_due()
                        .as_ref()
                        .map(echo::due_of)
                        .and_then(|due| match due {
                            Due::Day(day) => Some(day),
                            Due::Instant(ms) => chrono::DateTime::from_timestamp_millis(ms)
                                .map(|t| t.with_timezone(&chrono::Local).date_naive()),
                        });
                let (selected, offers_clear) = due_seeds(day, self.today);
                let grid =
                    DateGrid::with_phrase_field(&mut self.engine, selected, now, offers_clear);
                let panel = match trigger {
                    Some(t) => DuePick::place_under(t, grid.size(), self.viewport),
                    None => DuePick::place(d, grid.size(), self.viewport),
                };
                self.picker = Some(Picker::Due(DuePick {
                    target: Target::Capture,
                    grid,
                    panel,
                    clear_hovered: false,
                }));
            }
        }
        true
    }

    /// Which rows of the open picker may be chosen. The divider and the
    /// *no lists yet* line never; everything else always.
    pub(super) fn picker_enabled(&self) -> Vec<bool> {
        match &self.picker {
            Some(Picker::List(picker)) => picker
                .menu
                .items()
                .iter()
                .map(koru_ui::MenuItem::selectable)
                .collect(),
            Some(Picker::Priority(picker)) => picker
                .menu
                .items()
                .iter()
                .map(koru_ui::MenuItem::selectable)
                .collect(),
            Some(Picker::Color(picker)) => picker
                .menu
                .items()
                .iter()
                .map(koru_ui::MenuItem::selectable)
                .collect(),
            Some(Picker::Due(_)) | None => Vec::new(),
        }
    }

    /// Choose one row of the picker. **The panel closes**: unlike the overflow
    /// this is not a setting, and the answer is on the row's own chip a moment
    /// later.
    pub(super) fn choose_list(&mut self, row: usize) -> bool {
        let Some(Picker::List(picker)) = &self.picker else {
            return false;
        };
        let Some(item) = picker.menu.items().get(row).cloned() else {
            return false;
        };
        let list = match item {
            ListPick::List { checked: true, .. } => {
                // Choosing the list it is already in is a no-op, not a second
                // write — the guarded set would answer the same thing, and not
                // asking is cheaper than being told.
                self.picker = None;
                return true;
            }
            ListPick::List { id, .. } => Some(id),
            ListPick::Clear => None,
            ListPick::Divider | ListPick::Empty => return false,
        };
        match picker.target.request(|document, node| Request::SetList {
            document,
            node,
            list,
        }) {
            Some(request) => self.pending = Some(request),
            None => {
                if let Some(capture) = self.capture.as_mut() {
                    capture.list = list;
                }
            }
        }
        self.picker = None;
        true
    }

    /// Choose one row of the colour picker; the panel closes on a choice.
    /// The colour the rows already share is a no-op, as a list or a level is.
    pub(super) fn choose_color(&mut self, row: usize) -> bool {
        let Some(Picker::Color(picker)) = &self.picker else {
            return false;
        };
        let color = match picker.menu.items().get(row) {
            Some(ColorPick::Color { checked: true, .. }) => {
                self.picker = None;
                return true;
            }
            Some(ColorPick::Color { token, .. }) => Some((*token).to_owned()),
            Some(ColorPick::Clear) => None,
            Some(ColorPick::Divider) | None => return false,
        };
        let request = picker.target.request(|document, node| Request::SetColor {
            document,
            node,
            color: color.clone(),
        });
        self.pending = request;
        self.picker = None;
        true
    }

    /// Choose a row of whichever menu picker is up.
    fn choose_picked(&mut self, row: usize) -> bool {
        match self.picker {
            Some(Picker::List(_)) => self.choose_list(row),
            Some(Picker::Priority(_)) => self.choose_priority(row),
            Some(Picker::Color(_)) => self.choose_color(row),
            Some(Picker::Due(_)) | None => false,
        }
    }

    /// Choose one row of the priority picker; the panel closes on a choice.
    pub(super) fn choose_priority(&mut self, row: usize) -> bool {
        let Some(Picker::Priority(picker)) = &self.picker else {
            return false;
        };
        let choice = picker.choose(row);
        self.settle(choice)
    }

    /// What a picker decided, applied: a request leaves and the panel closes,
    /// a close closes, a stay stays.
    fn settle(&mut self, choice: Choice) -> bool {
        match choice {
            Choice::Request(request) => {
                self.pending = Some(request);
                self.picker = None;
                true
            }
            // Worn by the capture row until Enter, and re-read at once: a
            // picked due outranks the typed word, so the words may change.
            Choice::Picked(picked) => {
                let today = self.today;
                if let Some(capture) = self.capture.as_mut() {
                    match picked {
                        Picked::Due(due) => capture.due = due,
                        Picked::Priority(level) => capture.priority = level,
                    }
                    capture.reparse(today);
                }
                self.picker = None;
                true
            }
            Choice::Close => {
                self.picker = None;
                true
            }
            Choice::Stay => false,
        }
    }

    /// A press while the calendar is up. Inside the panel it is the grid's —
    /// the phrase field's band puts the caret, anything else is a cell or the
    /// Clear row — and outside it dismisses, the rule every menu follows.
    fn press_due(&mut self, x: f32, y: f32) {
        let Some(Picker::Due(pick)) = &mut self.picker else {
            return;
        };
        if !pick.panel.contains(x, y) {
            self.picker = None;
            return;
        }
        let (lx, ly) = (x - pick.panel.x, y - pick.panel.y);
        if ly < pick.grid.band() {
            pick.grid.click_field(&mut self.engine, lx, pick.panel.w);
            return;
        }
        let choice = pick.resolve(pick.grid.click(lx, ly));
        self.settle(choice);
    }

    /// A press while any picker is up. The two menus answer a row or nothing;
    /// the calendar answers for itself.
    fn press_picker(&mut self, x: f32, y: f32) -> bool {
        let viewport = self.viewport;
        let row = match &self.picker {
            Some(Picker::List(picker)) => Ok(picker.menu.item_at(viewport, x, y)),
            Some(Picker::Priority(picker)) => Ok(picker.menu.item_at(viewport, x, y)),
            Some(Picker::Color(picker)) => Ok(picker.menu.item_at(viewport, x, y)),
            Some(Picker::Due(_)) => Err(()),
            None => return false,
        };
        match row {
            Err(()) => self.press_due(x, y),
            // A press anywhere else dismisses it and does nothing more — the
            // rule every menu in this app follows.
            Ok(None) => self.picker = None,
            Ok(Some(row)) => {
                self.choose_picked(row);
            }
        }
        true
    }

    /// A key while any picker is up. The calendar takes the keyboard whole —
    /// arrows walk days, typing fills the phrase box, Enter picks — and the
    /// two menus take it the way the overflow does.
    fn key_picker(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        if let Some(Picker::Due(pick)) = &mut self.picker {
            if matches!(key, Key::Named(NamedKey::Escape)) {
                self.picker = None;
                return true;
            }
            let pick_out = pick.grid.key(&mut self.engine, key, text, mods);
            let choice = pick.resolve(pick_out);
            self.settle(choice);
            return true;
        }
        let enabled = self.picker_enabled();
        match key {
            Key::Named(NamedKey::Escape) => self.picker = None,
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::ArrowUp) => {
                let delta = if matches!(key, Key::Named(NamedKey::ArrowDown)) {
                    1
                } else {
                    -1
                };
                match &mut self.picker {
                    Some(Picker::List(picker)) => picker.menu.step(&enabled, delta),
                    Some(Picker::Priority(picker)) => picker.menu.step(&enabled, delta),
                    Some(Picker::Color(picker)) => picker.menu.step(&enabled, delta),
                    _ => {}
                }
            }
            Key::Named(NamedKey::Enter) => {
                let hover = match &self.picker {
                    Some(Picker::List(picker)) => picker.menu.hover,
                    Some(Picker::Priority(picker)) => picker.menu.hover,
                    Some(Picker::Color(picker)) => picker.menu.hover,
                    _ => None,
                };
                if let Some(row) = hover {
                    self.choose_picked(row);
                }
            }
            _ => {}
        }
        true
    }

    // ------------------------------------------------------------- input

    /// `asks_where` is Alt alone: on a row it asks where the task opens.
    /// `toggle` (Ctrl) and `extend` (Shift) grow the selection on a row
    /// instead of replacing it.
    fn press(&mut self, x: f32, y: f32, asks_where: bool, toggle: bool, extend: bool) -> bool {
        // One panel at a time, and the picker is the one that can be up over a
        // row: a press inside it chooses, a press outside dismisses it and does
        // nothing more — every menu in this app.
        if self.picker.is_some() {
            return self.press_picker(x, y);
        }
        // The open menu is above everything, including the row under it.
        if let Some(menu) = &self.overflow {
            let hit = menu.item_at(self.viewport, x, y);
            return match hit {
                // A press on a dimmed row is still the menu's — it changes
                // nothing and the panel stays where it is.
                Some(row) => {
                    self.choose_overflow(row);
                    true
                }
                // A press anywhere else dismisses it and does nothing more —
                // the rule every menu in this app follows, and the reason a
                // press on the `…` closes rather than reopening.
                None => {
                    self.overflow = None;
                    true
                }
            };
        }
        // The capture row's `/` menu is a menu like the others: a row chooses,
        // anywhere else closes it and does nothing more.
        if self.capture.as_ref().is_some_and(|c| c.menu_open()) {
            let (viewport, today) = (self.viewport, self.today);
            let typed = self
                .capture
                .as_mut()
                .and_then(|c| c.menu_press(&mut self.engine, viewport, x, y, today));
            if let Some(Typed::Open(row)) = typed {
                self.open_capture_picker(row);
            }
            return true;
        }
        // The rename field: a press in it moves the caret; anywhere else
        // keeps the words and lets the press land where it was aimed.
        if let Some(field) = self.rename_field() {
            if field.contains(x, y) {
                if let Some(rename) = self.rename.as_mut() {
                    rename.input.click(&mut self.engine, field, x, y);
                }
                return true;
            }
            self.commit_rename();
        }
        // A press outside the field commits it, the rule every field in this
        // app follows — and then the press still lands where it was aimed.
        if self.capture.is_some() && !self.capture_rect().contains(x, y) {
            let committed = self.commit_capture();
            if committed {
                return true;
            }
        }
        match self.hit_at(x, y) {
            Some(Hit::HeaderDots) => self.open_overflow(),
            // The colour square: the list's colour menu, at the press.
            Some(Hit::Swatch) => match self.target {
                RailTarget::List(list) => {
                    self.pending = Some(Request::ListColor { list, at: (x, y) });
                    true
                }
                _ => false,
            },
            // Both are the band's, taken in `mouse_down` before the note
            // could claim the press; named here so the match stays whole.
            Some(Hit::NoteOpen | Hit::NoteGrip) => false,
            Some(Hit::Capture) => {
                self.begin_capture();
                let rect = super::capture::input_rect(self.capture_rect());
                if let Some(capture) = self.capture.as_mut() {
                    capture.input.click(&mut self.engine, rect, x, y);
                }
                true
            }
            Some(Hit::Complete(i)) => self.toggle_complete(i),
            Some(Hit::Dots(i)) => {
                // A row inside the selection keeps it: the menu is the
                // selection's. Any other row becomes the selection.
                if !self.is_selected(i) {
                    self.select_row(i);
                }
                let bands = self.bands();
                let Some(line) = self.lines.iter().position(|l| l == &Line::Task(i)) else {
                    return false;
                };
                let d = self.parts(self.table().row_rect(&bands, line)).dots;
                // The button's bottom-right, so the menu hangs back under it
                // rather than out past the window's edge.
                self.pending = self.row_menu_request(i, (d.x + d.w, d.y + d.h));
                self.pending.is_some()
            }
            // **The list chip opens the list.** The owner's ask (finding 36),
            // and the one exception to the rule under `Hit::Row`: the chip
            // names a page, and a name you can see and cannot follow is the
            // dead end the rail's rows never are.
            Some(Hit::ListChip(i)) => self.press_list_chip(i, Some((x, y))),
            Some(Hit::ValueChip(i, kind)) => self.press_value_chip(i, kind, (x, y)),
            // **A click selects, and that is all it does.** The design's
            // sentence was *a whole-row click opens the node*, and it is the
            // reason this page has no inline affordances — but a list whose
            // every row is a trapdoor cannot be read. Skimming a queue, picking
            // a row to act on with the keyboard, or pressing near a row's edge
            // all threw the page away and replaced it with a document. Leaving
            // is now something asked for by name, on the row's own menu.
            //
            // **Alt alone asks where it opens** (the user's decision,
            // 2026-11-04; ADR-58): the place chooser at the press, for what
            // the row's open names — its node in its document. The row is
            // selected as any press selects it, so the chooser is plainly
            // about this one. The tick, the `…` and the list chip are above,
            // and keep their meaning under Alt.
            Some(Hit::Row(i)) if toggle => {
                self.toggle_row(i);
                true
            }
            Some(Hit::Row(i)) if extend => {
                self.extend_to(i);
                true
            }
            // **Every page's rows can be carried** — into a list in the
            // sidebar from any page, and into a new place on a page that is
            // ordered by hand. A release that never moved is the click this
            // arm has always been.
            Some(Hit::Row(i)) if !asks_where && self.drags_rows() => self.begin_drag(i, y),
            Some(Hit::Row(i)) => {
                self.select_row(i);
                if asks_where && let Some(row) = self.rows.get(i) {
                    self.pending = Some(Request::ChoosePlace {
                        document: row.document,
                        node: row.node,
                        at: (x, y),
                    });
                }
                true
            }
            // **A press on the page's empty space lets the selection go**
            // (the owner's ask, 2026-11-05): below the rows, beside the
            // heading, anywhere that is not a row or a control.
            None => {
                let had = self.selected.is_some() || !self.also.is_empty();
                self.clear_selection();
                had
            }
        }
    }

    /// Enter on the capture row. `true` when it produced a request.
    fn commit_capture(&mut self) -> bool {
        let request = self.capture_request();
        self.end_capture();
        match request {
            // **The row stays open for the next one**: a fresh, empty capture
            // with the caret in it, so a run of tasks is typed one Enter after
            // another rather than a click before each.
            Some(request) => {
                self.pending = Some(request);
                self.begin_capture();
                true
            }
            // Empty cancels — nothing typed is nothing meant, and a complaint
            // about a box you have not filled in is a scolding.
            None => true,
        }
    }
}

/// One chip on a row: what it is, what it says, and the accent it borrows.
///
/// A **reading**, never a control — the design's rule about this surface is
/// that a whole-row click opens the node and everything else lives in the menu,
/// so nothing here is pressable and the remove `×` is never inked.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct RowChip {
    pub kind: ChipKind,
    pub label: String,
    pub accent: Option<String>,
    /// The list this chip names — `Some` on the membership chip alone, on
    /// every page. Pressed, it opens the list picker to change it (the
    /// owner's ask, 2026-11-05: *show the list to change, not send me to
    /// the list*); going to the list is the row menu's *Go to list*.
    pub list: Option<ListId>,
}

impl RowChip {
    /// This chip as **the one chip component**: compact, in its accent's
    /// tone, and **with no remove lane** — a chip on this page is a reading
    /// of the row, and the control that changes it is the row's menu. It
    /// reserved the lane once anyway, which left 6 px on the left of every
    /// chip and 21 px on the right.
    pub(super) fn label_chip(&self) -> LabelChip<'_> {
        LabelChip::attribute(self.kind, &self.label, self.accent.as_deref())
    }

    fn tag(label: String, accent: Option<String>) -> Self {
        Self {
            kind: ChipKind::Tag,
            label,
            accent,
            list: None,
        }
    }
}

/// The rectangles one row is made of.
#[derive(Copy, Clone, PartialEq, Debug)]
pub(super) struct RowParts {
    /// The complete control's **target** — what a press is resolved against
    /// and what accessibility reports — a [`CONTROL_W`] square.
    pub control: Rect,
    /// The box **drawn** inside it, at the document's own size
    /// ([`theme::CHECKBOX_SIZE`]) and centred, so a task ticked here and in
    /// its document is one box at one size, and the target stays the size a
    /// pointer can hit.
    pub checkbox: Rect,
    pub dots: Rect,
    /// Everything between the control and the chips: the kind icon, the title
    /// and — in Today — the context line under it.
    pub text: Rect,
}

/// A parsed date word, in the shape the store keeps.
///
/// **The zone is added by the editor's one conversion, not here.** A wall
/// clock says nothing about where the person is; turning one into a shared
/// deadline is a decision, and `when::to_temporal` is where every surface
/// makes it — the editor's `/due`, the picker, this row — so one phrase stores
/// one value wherever it was typed.
pub(crate) fn temporal_of(parsed: Parsed) -> koru_core::time::Temporal {
    when::to_temporal(parsed)
}

/// Split a typed line into its title and the date word at its end.
///
/// The **longest** trailing phrase that parses wins, so `chase the sign-off
/// next tuesday` loses three words rather than one. A phrase that would leave
/// no title at all is refused: somebody typing only `tomorrow` means a task
/// called *tomorrow*, and turning it into an untitled task due tomorrow is the
/// parser deciding what they meant.
pub fn split_date(input: &str, today: NaiveDate) -> Option<(String, Parsed)> {
    let trimmed = input.trim_end();
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    let at = Reference::without_selection(today.and_hms_opt(12, 0, 0)?);
    for take in (1..=DATE_WORDS.min(words.len().saturating_sub(1))).rev() {
        let split = words.len() - take;
        let phrase = words[split..].join(" ");
        let Some(parsed) = when::parse(&phrase, at, when::DEFAULT_DATE_ORDER) else {
            continue;
        };
        let head = words[..split].join(" ");
        if head.is_empty() {
            continue;
        }
        return Some((head, parsed));
    }
    None
}

/// Which rule-label group a dated row falls under.
///
/// **Weeks near, months far**, which is the design's own pair of registers:
/// `Next week` names a week because next week is a thing a person plans, and
/// `Later in September` names a *month* because two weeks out nobody counts in
/// weeks any more. That difference is why this is a group key rather than the
/// Monday it started as: grouping by week and labelling by month printed *Later
/// in September* over every week of September separately, which is the same
/// heading three times.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Rule {
    ThisWeek,
    NextWeek,
    /// Everything further out, by the month it falls in.
    Month(i32, u32),
}

impl Rule {
    pub fn of(day: NaiveDate, today: NaiveDate) -> Self {
        use chrono::Datelike;
        let weeks = (week_start(day) - week_start(today)).num_days() / 7;
        match weeks {
            ..=0 => Self::ThisWeek,
            1 => Self::NextWeek,
            _ => Self::Month(day.year(), day.month()),
        }
    }

    /// Sentence case, and relative where a person counts that way.
    ///
    /// The year is added only when it is not this one: *Later in January* in
    /// December would name a month four weeks away and mean one thirteen months
    /// out, which is the only way this label can be read two ways.
    pub fn label(self, today: NaiveDate) -> String {
        use chrono::Datelike;
        match self {
            Self::ThisWeek => "This week".to_owned(),
            Self::NextWeek => "Next week".to_owned(),
            Self::Month(year, month) => {
                let name = month_name(month);
                if year == today.year() {
                    format!("Later in {name}")
                } else {
                    format!("Later in {name} {year}")
                }
            }
        }
    }
}

/// The month's full name. **Spelled here rather than abbreviated**, because a
/// rule-label is a sentence and `Later in Sep` is not one — `koru_ui::dates`
/// answers the short form for a chip, which is a different job.
fn month_name(month: u32) -> &'static str {
    const NAMES: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    NAMES[(month as usize).clamp(1, 12) - 1]
}

/// The Monday that starts `day`'s week. One function, so the row's week and the
/// rule's week cannot land on different Mondays.
pub fn week_start(day: NaiveDate) -> NaiveDate {
    use chrono::Datelike;
    day - chrono::Duration::days(day.weekday().num_days_from_monday() as i64)
}

/// The clause the E-3 confirm puts after the quoted title.
///
/// **A sentence, not a chip.** The confirm is prose and reads as prose; the
/// same date rendered as `Aug 30` in a lozenge reads as ` is due Aug 30.` here.
/// An undated task earns no clause at all rather than an awkward one about the
/// date it does not have.
pub fn due_sentence(row: &TaskEntry, today: NaiveDate) -> String {
    match row.due.and_then(Due::local_day) {
        Some(day) => format!(
            " is due {}.",
            koru_editor::utils::dates::due_date_label(day, today)
        ),
        None => " is on your Tasks list.".to_owned(),
    }
}

/// The chip text for a due value, or the en dash a dated-nothing wears.
pub(super) fn due_label(due: Option<Due>, today: NaiveDate) -> String {
    match due.and_then(Due::local_day) {
        Some(day) => koru_editor::utils::dates::due_date_label(day, today),
        None => koru_editor::chips::DUE_NONE.to_owned(),
    }
}

impl View for TasksView {
    fn key(
        &mut self,
        ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        // The heading's rename field has the keyboard while it stands:
        // Enter keeps the words, Escape puts the old ones back.
        if self.heading_rename.is_some() {
            return self.heading_rename_key(key, text, mods);
        }
        // The note has the keyboard while it is focused and the document
        // handed in is its own; Escape hands the keyboard back to the table.
        if let Some(body) = &mut self.body
            && body.focused
        {
            if !body.is_in(ws) {
                body.blur();
            } else if matches!(key, Key::Named(NamedKey::Escape)) {
                body.key(ws, key, text, mods);
                body.blur();
                return true;
            } else {
                let took = body.key(ws, key, text, mods);
                let on_task = took && body.caret_on_task(ws);
                if on_task {
                    self.note_task_touch(ws);
                }
                return took;
            }
        }
        // The rename field has the keyboard while it stands: Enter keeps
        // the words, Escape puts the old ones back.
        if self.rename.is_some() {
            return self.rename_key(key, text, mods);
        }
        // F2 renames the selected row, the platform's rename key.
        if matches!(key, Key::Named(NamedKey::F2))
            && self.capture.is_none()
            && let Some(i) = self.selected_row()
        {
            return self.begin_rename(i);
        }
        // **Escape takes a drag back**: the rows stay where they were.
        if self.drag.is_some() && matches!(key, Key::Named(NamedKey::Escape)) {
            self.drag = None;
            return true;
        }
        // The picker takes the keyboard the same way the overflow does, and for
        // the same reason: Escape closes the panel rather than the field under
        // it, and the arrows step rows rather than scroll the page.
        if self.picker.is_some() {
            return self.key_picker(key, text, mods);
        }
        // The open menu takes the keyboard, so Escape closes the panel rather
        // than the field under it and the arrows step rows rather than scroll.
        if self.overflow.is_some() {
            let enabled = self.overflow_enabled();
            match key {
                Key::Named(NamedKey::Escape) => {
                    self.overflow = None;
                    return true;
                }
                Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::ArrowUp) => {
                    let delta = if matches!(key, Key::Named(NamedKey::ArrowDown)) {
                        1
                    } else {
                        -1
                    };
                    if let Some(menu) = self.overflow.as_mut() {
                        menu.step(&enabled, delta);
                    }
                    return true;
                }
                Key::Named(NamedKey::Enter) => {
                    if let Some(row) = self.overflow.as_ref().and_then(|menu| menu.hover) {
                        self.choose_overflow(row);
                    }
                    return true;
                }
                _ => return true,
            }
        }
        if let Some(capture) = self.capture.as_mut() {
            let today = self.today;
            return match capture.key(&mut self.engine, key, text, mods, today) {
                Typed::Commit => self.commit_capture(),
                Typed::Cancel => {
                    self.end_capture();
                    true
                }
                Typed::Open(row) => self.open_capture_picker(row),
                Typed::Consumed => true,
            };
        }
        // The list's own keyboard, **on the Trash tab's conventions** — the two
        // pages in this window that are lists of rows answer an arrow, an Enter
        // and an Escape the same way. What differs is what Enter *does*, and
        // that follows each page's own whole-row gesture: Trash restores,
        // because that is what its row control does, and this opens, because a
        // whole-row click here opens the node.
        //
        // There is **no chord here and none in the registry**: these are the
        // plain navigation keys a list owns while it has the keyboard, not
        // shortcuts, which is why the Trash tab registers none either.
        match key {
            Key::Named(NamedKey::ArrowDown) => self.step(1),
            // **Up from the first row lands in the capture line.** It is the
            // list's own first row — `Line::Capture` says so in as many words —
            // and it was the one gesture on this page a pointer could reach and
            // a keyboard could not, which made *the* way to create a task here
            // unreachable without a mouse. Nothing new is invented: this is the
            // `begin_capture` a press on that row already makes, reached by the
            // arrow that was walking the list anyway. Escape leaves it again.
            Key::Named(NamedKey::ArrowUp)
                if self.selected_row() == Some(0) || self.rows.is_empty() =>
            {
                self.clear_selection();
                self.begin_capture();
                true
            }
            Key::Named(NamedKey::ArrowUp) => self.step(-1),
            // **Here, not in a new tab.** `Enter` is the keyboard's whole-row
            // gesture and it goes where the menu's first row goes; the second
            // window is the double click's, which is the gesture that has a
            // pointer already resting on the page it would otherwise replace.
            Key::Named(NamedKey::Enter) => self.open_selected(false),
            // Shift+F10 arrives as this on every platform winit supports, and
            // it is the row's `…` by another name.
            Key::Named(NamedKey::ContextMenu) => self.menu_for_selected(),
            Key::Named(NamedKey::Escape) => {
                let had = self.selected.is_some();
                self.clear_selection();
                had
            }
            // **Delete takes the selection off Tasks**, once asked — the
            // same *Remove from Tasks* as the menu row, for every selected
            // task. A line in a document stays; nothing is deleted from one.
            Key::Named(NamedKey::Delete) => self.remove_selection(),
            _ => false,
        }
    }

    fn mouse_down(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        self.pointer = (x, y);
        // The heading's rename field: a press in it moves the caret;
        // anywhere else keeps the words and lets the press land where it
        // was aimed — the rule every field in this app follows.
        if self.heading_rename.is_some() {
            let field = self.heading_field();
            if field.contains(x, y) {
                if let Some(input) = self.heading_rename.as_mut() {
                    input.click(&mut self.engine, field, x, y);
                }
                return true;
            }
            self.commit_heading_rename();
        }
        if let Some((_, retry)) = self.notice
            && retry.contains(x, y)
        {
            self.wants_retry = true;
            return true;
        }
        if let Some((keep, recover)) = self.conflict_actions {
            if keep.contains(x, y) {
                self.wants_decision = Some(koru_workspace::pages::PageDecision::Keep);
                return true;
            }
            if recover.contains(x, y) {
                self.wants_decision = Some(koru_workspace::pages::PageDecision::Recover);
                return true;
            }
        }
        // The note's open picker is above everything on the page: the press
        // is its to answer wherever it lands.
        if let Some(body) = &mut self.body
            && body.overlay_open()
            && body.is_in(ws)
        {
            return body.mouse_down_on_overlay(ws, x, y, ctrl);
        }
        // The band's own controls, ahead of the note they sit on: the grip
        // along its bottom edge and the corner's *open in a new tab*.
        if let Some(body) = &self.body
            && body.is_in(ws)
        {
            if body.grip_rect().contains(x, y) {
                self.band_resize = Some(body.band.y + body.band.h - y);
                return true;
            }
            if body.open_rect().contains(x, y) {
                self.pending = Some(Request::OpenNote {
                    document: body.document,
                });
                return true;
            }
        }
        if self
            .body
            .as_ref()
            .is_some_and(|body| body.contains(x, y) && body.is_in(ws))
        {
            // A press in the note commits whatever field was open below it,
            // the rule every field in this app follows, and takes the caret.
            if self.rename.is_some() {
                self.commit_rename();
            }
            if self.capture.is_some() {
                self.commit_capture();
                self.end_capture();
            }
            self.clear_selection();
            let body = self.body.as_mut().expect("checked above");
            body.focus();
            body.mouse_down(ws, x, y, ctrl);
            return true;
        }
        // A press on the table hands the keyboard back.
        if let Some(body) = &mut self.body
            && body.focused
        {
            body.blur();
        }
        // Alt with nothing else is the place chooser's press (ADR-58); Alt
        // with Ctrl or Shift is that key's press, as in a document.
        let asks_where = self.mods.alt_key()
            && !ctrl
            && !self.mods.control_key()
            && !self.mods.shift_key()
            && !self.mods.super_key();
        let toggle = ctrl || self.mods.control_key() || self.mods.super_key();
        let extend = !toggle && self.mods.shift_key();
        self.press(x, y, asks_where, toggle, extend)
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.pointer = (x, y);
        if self.drag.is_some() {
            return self.drag_to(y);
        }
        // The grip's drag: the band's bottom edge follows the pointer, the
        // offset taken at the press kept. Laid out on the next frame.
        if let Some(offset) = self.band_resize {
            self.band_fixed_h = Some(y + offset - self.header_h);
            if let Some(body) = &mut self.body {
                body.mark_dirty();
            }
            return true;
        }
        let heading_field = self.heading_field();
        if let Some(input) = self.heading_rename.as_mut()
            && input.drag(&mut self.engine, heading_field, x)
        {
            return true;
        }
        if let Some(body) = &mut self.body
            && (body.is_dragging() || body.overlay_open() || body.contains(x, y))
        {
            let moved = body.mouse_move(x, y);
            let was = self.hover.take();
            // The band's own controls light under the pointer, as the
            // header's `…` does.
            self.hover = self.band_hit(x, y);
            return moved || was != self.hover;
        }
        let viewport = self.viewport;
        if let Some(picker) = &mut self.picker {
            return match picker {
                Picker::List(picker) => {
                    let was = picker.menu.hover;
                    picker.menu.hover = picker.menu.item_at(viewport, x, y);
                    was != picker.menu.hover
                }
                Picker::Priority(picker) => {
                    let was = picker.menu.hover;
                    picker.menu.hover = picker.menu.item_at(viewport, x, y);
                    was != picker.menu.hover
                }
                Picker::Color(picker) => {
                    let was = picker.menu.hover;
                    picker.menu.hover = picker.menu.item_at(viewport, x, y);
                    was != picker.menu.hover
                }
                // The grid has no rows to hover; the Clear row is the one
                // thing in the panel that lights under the pointer.
                Picker::Due(pick) => {
                    let was = pick.clear_hovered;
                    let ly = y - pick.panel.y;
                    pick.clear_hovered = pick.panel.contains(x, y)
                        && pick
                            .grid
                            .clear_row_top()
                            .is_some_and(|top| ly >= top && ly < top + theme::ROW_H);
                    was != pick.clear_hovered
                }
            };
        }
        if let Some(menu) = &mut self.overflow {
            let was = menu.hover;
            menu.hover = menu.item_at((self.viewport.0, self.viewport.1), x, y);
            return was != menu.hover;
        }
        if let Some(capture) = self.capture.as_mut()
            && capture.menu_open()
        {
            return capture.menu_hover(self.viewport, x, y);
        }
        let was = self.hover;
        self.hover = self.hit_at(x, y);
        if self.capture.is_some() {
            let rect = super::capture::input_rect(self.capture_rect());
            let engine = &mut self.engine;
            if let Some(capture) = self.capture.as_mut()
                && capture.input.drag(engine, rect, x)
            {
                return true;
            }
        }
        was != self.hover
    }

    fn mouse_up(&mut self, ws: &mut WorkspaceDoc) -> bool {
        if self.band_resize.take().is_some() {
            return true;
        }
        if let Some(input) = self.heading_rename.as_mut() {
            input.release();
        }
        if let Some(body) = &mut self.body
            && body.focused
        {
            let took = body.mouse_up(ws);
            let on_task = took && body.caret_on_task(ws);
            if on_task {
                self.note_task_touch(ws);
            }
            return took;
        }
        if self.drag.is_some() {
            return self.drop_drag();
        }
        if let Some(rename) = self.rename.as_mut() {
            rename.input.release();
        }
        if let Some(capture) = self.capture.as_mut() {
            capture.input.release();
        }
        false
    }

    /// **Go to the origin, in a new tab.**
    ///
    /// A single click selects, so the ordinary select-then-act idiom leaves the
    /// second click free and this is what it is for. The new tab is the owner's
    /// call and it is the right one for a reason worth writing down: going back
    /// does not return you to the queue and the row you were on (the Tasks page
    /// is a tab rather than a history entry), so a gesture that replaced this
    /// page would be a one-way door. A second tab leaves the page where it is
    /// and the way back is the tab you came from.
    ///
    /// The first click of the pair has already selected the row, which is why
    /// this acts on the selection rather than hit-testing again: a double click
    /// that landed a pixel off the row it opened would be its own bug.
    ///
    /// **It renames** (the owner's ask, 2026-11-05): a page-made task lives
    /// in the hidden tasks document, and opening that is a look at storage,
    /// not at the task. Opening is the row menu's *Open document*.
    fn mouse_double_click(&mut self, ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        // A double click on the grip gives the band back to its content —
        // the way back from a size given by hand.
        if self
            .body
            .as_ref()
            .is_some_and(|body| body.is_in(ws) && body.grip_rect().contains(x, y))
        {
            self.band_fixed_h = None;
            self.band_resize = None;
            if let Some(body) = &mut self.body {
                body.mark_dirty();
            }
            return true;
        }
        if let Some(body) = &mut self.body
            && body.contains(x, y)
            && body.is_in(ws)
        {
            return body.mouse_double_click(ws, x, y);
        }
        // A double click on a list's title renames the list in place, as
        // one on a row's words renames the task.
        if self.title_rect().is_some_and(|r| r.contains(x, y)) {
            return self.begin_heading_rename();
        }
        let Some(Hit::Row(i)) = self.hit_at(x, y) else {
            return false;
        };
        self.begin_rename(i)
    }

    fn context_menu(&mut self, _ws: &mut WorkspaceDoc, x: f32, y: f32) -> bool {
        // One panel at a time: a right-click while a panel is up dismisses it,
        // the way a press anywhere else does, rather than stacking a second
        // menu over the first.
        if self.overflow.is_some() || self.picker.is_some() {
            self.overflow = None;
            self.picker = None;
            return true;
        }
        let Some(
            Hit::Row(i) | Hit::Complete(i) | Hit::Dots(i) | Hit::ListChip(i) | Hit::ValueChip(i, _),
        ) = self.hit_at(x, y)
        else {
            return false;
        };
        if !self.is_selected(i) {
            self.select_row(i);
        }
        self.pending = self.row_menu_request(i, (x, y));
        self.pending.is_some()
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        if let Some(body) = &mut self.body
            && body.contains(self.pointer.0, self.pointer.1)
        {
            body.scroll_by(delta);
            return;
        }
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * self.row_h(),
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        self.scroll -= dy;
        self.clamp_scroll();
    }

    fn set_viewport(&mut self, w: f32, h: f32) {
        self.viewport = (w, h);
        // Also the moment the font role or the density could have moved under
        // us, which is what the heading's height depends on.
        self.measure_header();
        if let Some(body) = &mut self.body {
            body.mark_dirty();
        }
        self.clamp_scroll();
    }

    fn set_mods(&mut self, mods: ModifiersState) {
        self.mods = mods;
        if let Some(body) = &mut self.body {
            body.set_mods(mods);
        }
    }

    /// **Every hit is [`PRESS`], which is the plain arrow** ([ADR-215]): a
    /// control is identified by its own appearance, and each of these has one —
    /// the chip's lozenge inverts, the `…` lights, a row fills.
    ///
    /// [PRESS]: koru_ui::cursor::PRESS
    /// [ADR-215]: ../../../docs/decisions/ADR-201-225.md#adr-215-nohandcursor--koru-does-not-use-the-hand-cursor-a-press-target-is-identified-by-its-own-appearance
    fn cursor_icon(&self) -> CursorIcon {
        // The grip is the one control with a cursor of its own: a drag
        // along an edge is what the resize arrows say, as on a pane divider.
        if self.band_resize.is_some() || self.hover == Some(Hit::NoteGrip) {
            return CursorIcon::RowResize;
        }
        if self.hover == Some(Hit::NoteOpen) {
            return koru_ui::cursor::PRESS;
        }
        if let Some(body) = &self.body
            && body.contains(self.pointer.0, self.pointer.1)
        {
            return body.cursor_icon();
        }
        match self.hover {
            Some(
                Hit::Complete(_)
                | Hit::Dots(_)
                | Hit::Row(_)
                | Hit::ListChip(_)
                | Hit::ValueChip(..)
                | Hit::Capture
                | Hit::HeaderDots
                | Hit::Swatch
                | Hit::NoteOpen,
            ) => koru_ui::cursor::PRESS,
            Some(Hit::NoteGrip) => CursorIcon::RowResize,
            None => koru_ui::cursor::IDLE,
        }
    }

    /// Only a flag. The page is read in `catch_up`, which runs for the tab
    /// about to be drawn and not for the ones that are not.
    fn adopt(&mut self, ws: &WorkspaceDoc) {
        self.stale = true;
        self.adopt_body(ws);
    }

    fn reseat(&mut self, _ws: &WorkspaceDoc) {
        self.stale = true;
        self.scroll = 0.0;
        self.end_capture();
        self.overflow = None;
        self.picker = None;
    }

    fn invalidate(&mut self) {}

    /// Only while a revealed row is still lit; the deadline turns it off.
    fn animating(&self) -> bool {
        self.flash
            .is_some_and(|(_, until)| std::time::Instant::now() < until)
            || self.body.as_ref().is_some_and(PageBody::is_animating)
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        workspace: &koru_workspace::Workspace,
        images: &koru_draw::ImageCache,
        root: Affine,
        caret_on: bool,
        pointer: (f32, f32),
    ) {
        self.layout_band();
        self.paint_under(scene, root);
        if let Some(body) = &mut self.body {
            body.draw(scene, workspace, images, root, caret_on, pointer);
        }
        self.paint_band_chrome(scene, root);
        self.paint_notice(scene, root);
        self.paint_over(scene, root, caret_on);
        // The note's picker floats over the rows and the page's own panels.
        if let Some(body) = &mut self.body {
            body.draw_panels(scene, root);
        }
    }

    fn restore(&mut self, _ws: &koru_workspace::Workspace, _settings: &Settings) {}

    fn capture(&self, _settings: &mut Settings) {}
}

/// The value every item shares, or `None` when they differ or there are
/// none — what a picker opened for a selection shows as current.
fn common<T: PartialEq>(mut values: impl Iterator<Item = T>) -> Option<T> {
    let first = values.next()?;
    values.all(|value| value == first).then_some(first)
}

/// Where a menu picker hangs: **at the pointer**, top-left there, when a chip
/// was pressed; else under the row's `…`, right edge to it, as the row's
/// menu that asked for it was.
fn menu_anchor(dots: Rect, at: Option<(f32, f32)>) -> ((f32, f32), Anchor) {
    match at {
        Some(point) => (point, Anchor::Cursor),
        None => ((dots.x + dots.w, dots.y + dots.h), Anchor::ButtonRight),
    }
}

// ------------------------------------------------------------ the note

impl TasksView {
    /// Which page this view is, as the system document keys it.
    pub fn page_key(&self) -> Option<PageKey> {
        match self.target {
            RailTarget::Bucket(bucket) => Some(PageKey::Bucket(bucket)),
            RailTarget::List(id) => Some(PageKey::List(id)),
            RailTarget::Reminders => None,
        }
    }

    /// The page's document, once the shell has said which it is — what this
    /// tab edits (`Tab::edits`, D7).
    pub fn page_document(&self) -> Option<DocumentId> {
        self.page.as_ref().map(|page| page.document)
    }

    /// Whether the page's document exists in this process to be activated:
    /// every phase but one still being opened.
    pub fn page_is_local(&self) -> bool {
        self.page
            .as_ref()
            .is_some_and(|page| page.phase != PagePhase::Loading)
    }

    /// Where the page's document stands, from the shell. Answers whether
    /// anything on screen changed.
    pub fn set_page_state(&mut self, state: &PageState) -> bool {
        if Some(state.page) != self.page_key() || self.page.as_ref() == Some(state) {
            return false;
        }
        let other_document = self.page_document() != Some(state.document);
        self.page = Some(state.clone());
        if other_document {
            self.body = None;
            self.band_h = 0.0;
        }
        self.notice = None;
        self.conflict_actions = None;
        true
    }

    pub fn take_wants_retry(&mut self) -> bool {
        std::mem::take(&mut self.wants_retry)
    }

    /// Whether this page's note is in a deletion conflict — the band with
    /// *Keep deleted* / *Recover* (plan §6.2).
    fn in_conflict(&self) -> bool {
        self.page.as_ref().is_some_and(|page| page.conflict)
    }

    pub fn take_wants_decision(&mut self) -> Option<koru_workspace::pages::PageDecision> {
        self.wants_decision.take()
    }

    /// The conflict band's (Keep, Recover) rects, for a test to press.
    #[cfg(test)]
    pub(super) fn conflict_actions_for_test(&self) -> Option<(Rect, Rect)> {
        self.conflict_actions
    }

    /// The note's leaf, while the note has the keyboard and `ws` is its
    /// document — where a paste or a dictation lands then.
    pub fn body_leaf_in(&mut self, ws: &WorkspaceDoc) -> Option<&mut Leaf> {
        let body = self.body.as_mut()?;
        (body.focused && body.is_in(ws)).then(|| body.leaf_mut())
    }

    /// The note's leaf, whatever has the keyboard — for an image import the
    /// note itself asked for.
    pub fn body_leaf_mut(&mut self) -> Option<&mut Leaf> {
        self.body.as_mut().map(PageBody::leaf_mut)
    }

    /// The note's leaf when it was just built and has not been fed the
    /// container's caches yet — once per body.
    pub fn fresh_body_leaf(&mut self) -> Option<&mut Leaf> {
        let body = self.body.as_mut()?;
        body.take_fresh().then(|| body.leaf_mut())
    }

    /// The note's leaf while the note has the keyboard — what dictation and
    /// the editable-target routing ask about.
    pub fn focused_body_leaf(&self) -> Option<&Leaf> {
        self.body
            .as_ref()
            .filter(|body| body.focused)
            .map(PageBody::leaf)
    }

    /// Whether the note has the keyboard and `ws` is its document.
    pub fn body_is_focused_in(&self, ws: &WorkspaceDoc) -> bool {
        self.body
            .as_ref()
            .is_some_and(|body| body.focused && body.is_in(ws))
    }

    /// *Write about {page}…*, in the reader's language.
    fn placeholder(&self) -> String {
        fill(
            t(crate::format::lang(), koru_i18n::Key::PagePlaceholder),
            &[&self.title()],
        )
    }

    /// Build or settle the note over `ws`, when `ws` is the page's document.
    /// Answers whether it changed.
    fn settle_body(&mut self, ws: &WorkspaceDoc) -> bool {
        let Some(document) = self.page_document() else {
            return false;
        };
        if let Some(body) = &mut self.body
            && body.document == document
        {
            return body.catch_up(ws);
        }
        if ws.own_document_id() != Some(document) {
            return false;
        }
        let placeholder = self.placeholder();
        let default_list = match self.target {
            RailTarget::List(list) => Some(list),
            _ => None,
        };
        match PageBody::open(ws, document, placeholder, default_list) {
            Some(body) => {
                self.body = Some(body);
                true
            }
            None => false,
        }
    }

    /// Typing in the note touched a task: the rows below are asked again
    /// once the typing settles (plan §6.9, *rows right, page fast*). Prose
    /// never comes here, so the table is not remeasured for it.
    fn note_task_touch(&mut self, ws: &WorkspaceDoc) {
        if let Some(requirement) = super::query::WriteRequirement::of_document(ws) {
            self.task_touch = Some((std::time::Instant::now(), requirement));
        }
    }

    /// When the coalesced re-ask is due, if typing touched a task.
    pub fn task_touch_deadline(&self) -> Option<std::time::Instant> {
        self.task_touch
            .as_ref()
            .map(|(at, _)| *at + TASK_TOUCH_SETTLE)
    }

    /// Ask the rows for the touched task's write, once the typing settled.
    /// Answers whether it asked.
    pub fn flush_task_touch(&mut self, now: std::time::Instant) -> bool {
        let due = self
            .task_touch
            .as_ref()
            .is_some_and(|(at, _)| now >= *at + TASK_TOUCH_SETTLE);
        if !due {
            return false;
        }
        let Some((_, requirement)) = self.task_touch.take() else {
            return false;
        };
        self.panel.require(requirement);
        self.mark_stale();
        true
    }

    #[cfg(test)]
    pub(super) fn is_stale(&self) -> bool {
        self.stale
    }

    pub(super) fn adopt_body(&mut self, ws: &WorkspaceDoc) {
        if let Some(body) = &mut self.body {
            body.adopt(ws);
        }
    }

    /// The failure the band shows, if the note failed to register or open.
    fn failure(&self) -> Option<bool> {
        match self.page.as_ref().map(|page| &page.phase) {
            Some(PagePhase::Failed { opening, .. }) => Some(*opening),
            _ => None,
        }
    }

    fn notice_h(&self) -> f32 {
        let mut h = 0.0;
        if self.in_conflict() {
            h += self.row_h();
        }
        if self.failure().is_some() {
            h += self.row_h();
        }
        h
    }

    /// Where the rows start: under the heading, the note's band and its
    /// notice when there is one.
    pub(super) fn rows_top(&self) -> f32 {
        self.header_h + self.band_h + self.notice_h()
    }

    /// Lay the band out for this frame: the note's content up to its share
    /// of the page (D8). The rows move down by it.
    pub(super) fn layout_band(&mut self) {
        let before = self.band_h;
        let (header_h, w, h) = (self.header_h, self.viewport.0, self.viewport.1);
        let fixed = self.band_fixed_h;
        self.band_h = match &mut self.body {
            Some(body) => body.layout(PAD, header_h, w, h, fixed),
            None => 0.0,
        };
        if (before - self.band_h).abs() > 0.5 {
            self.clamp_scroll();
        }
    }

    /// The band's own controls under the pointer: the corner's *open in a
    /// new tab*, or the grip along the bottom edge. The grip is asked last,
    /// since its lane straddles the edge the corner control sits above.
    fn band_hit(&self, x: f32, y: f32) -> Option<Hit> {
        let body = self.body.as_ref()?;
        if body.band.h <= 0.0 {
            return None;
        }
        if body.open_rect().contains(x, y) {
            return Some(Hit::NoteOpen);
        }
        if body.grip_rect().contains(x, y) {
            return Some(Hit::NoteGrip);
        }
        None
    }

    /// The band's height as a person left it, for the suite.
    #[cfg(test)]
    pub(super) fn band_fixed_h_for_test(&self) -> Option<f32> {
        self.band_fixed_h
    }

    /// The *open in a new tab* control and the grip's corner mark, over the
    /// note and under its notice. Nothing while there is no band.
    pub(super) fn paint_band_chrome(&mut self, scene: &mut Scene, root: Affine) {
        let Some(body) = self.body.as_ref() else {
            return;
        };
        if body.band.h <= 0.0 {
            return;
        }
        let (open, mark) = (body.open_rect(), body.grip_mark());
        let p = theme::p();
        // **The icon button every corner control in the app is.** The hover
        // state is the whole affordance (ADR-215).
        koru_ui::icon_button::draw_with(
            scene,
            root,
            &mut self.engine,
            open,
            koru_draw::icons::POP_OUT,
            koru_ui::icon_button::State {
                hovered: self.hover == Some(Hit::NoteOpen),
                active: false,
                disabled: false,
            },
            koru_ui::icon_button::Metrics::INLINE,
        );
        // The corner mark an HTML text area wears: two short diagonals in
        // the bottom-right, in the dim ink, so the band says it can be
        // dragged without a track or a rule across the page.
        let lit = self.band_resize.is_some() || self.hover == Some(Hit::NoteGrip);
        let ink = if lit { p.text } else { p.text_dim };
        let (x1, y1) = (mark.x + mark.w, mark.y + mark.h);
        for inset in [0.0_f32, mark.w * 0.5] {
            let line = vello::kurbo::Line::new(
                ((mark.x + inset) as f64, y1 as f64),
                (x1 as f64, (mark.y + inset) as f64),
            );
            koru_draw::scene::stroke(scene, root, ink, 1.0, &line);
        }
    }

    // ---------------------------------------------------- the heading's own

    /// The list's colour square ahead of the heading, as a press target:
    /// the heading's whole line height, so a press beside the square is
    /// still the square's. `None` off a list page.
    pub(super) fn swatch_rect(&self) -> Option<Rect> {
        self.title_swatch().map(|_| {
            Rect::new(
                PAD,
                self.title_top(),
                HEADER_SWATCH + SWATCH_GAP * 0.5,
                super::draw::TITLE.metrics.line_height,
            )
        })
    }

    /// Where the title's words are, on a list page — what a double click
    /// renames. `None` on a queue, whose name is nobody's to change.
    pub(super) fn title_rect(&mut self) -> Option<Rect> {
        let RailTarget::List(_) = self.target else {
            return None;
        };
        let x = match self.title_swatch() {
            Some(_) => PAD + HEADER_SWATCH + SWATCH_GAP,
            None => PAD,
        };
        // Measured, not estimated: a list's name may carry an emoji.
        let title = self.title();
        let width = self
            .engine
            .simple_layout(&title, super::draw::TITLE)
            .width()
            .min((self.header_dots().x - CONTROL_GAP - x).max(0.0));
        Some(Rect::new(
            x,
            self.title_top(),
            width,
            super::draw::TITLE.metrics.line_height,
        ))
    }

    /// Where the rename field stands: over the title, out to the header's
    /// own controls, so the new words have the line to grow into.
    pub(super) fn heading_field(&self) -> Rect {
        let x = match self.title_swatch() {
            Some(_) => PAD + HEADER_SWATCH + SWATCH_GAP,
            None => PAD,
        };
        Rect::new(
            x,
            self.title_top(),
            (self.header_dots().x - CONTROL_GAP - x).max(0.0),
            super::draw::TITLE.metrics.line_height,
        )
    }

    /// Put the rename field over a list page's title, holding the list's
    /// name selected whole — a double click means *change this*. Any other
    /// field or panel goes. Nothing on a queue.
    pub(super) fn begin_heading_rename(&mut self) -> bool {
        let RailTarget::List(id) = self.target else {
            return false;
        };
        let Some(name) = self.list_face(id).map(|face| face.name.clone()) else {
            return false;
        };
        self.end_capture();
        self.commit_rename();
        self.picker = None;
        self.overflow = None;
        self.drag = None;
        if let Some(body) = &mut self.body
            && body.focused
        {
            body.blur();
        }
        let mut input = koru_editor::components::text_input::TextInput::new(
            &mut self.engine,
            super::draw::TITLE,
        );
        input.set_text(&mut self.engine, &name);
        input.select_all(&mut self.engine);
        self.heading_rename = Some(input);
        true
    }

    /// Keep the field's words: a request when they changed, are not empty
    /// and name no other list — an empty name is a slip, a sibling's name a
    /// collision the rail's field refuses too — and the field goes either
    /// way.
    pub(super) fn commit_heading_rename(&mut self) -> bool {
        let Some(input) = self.heading_rename.take() else {
            return false;
        };
        let RailTarget::List(list) = self.target else {
            return true;
        };
        let name = input.value().trim().to_owned();
        let before = self.list_face(list).map(|face| face.name.as_str());
        if name.is_empty() || Some(name.as_str()) == before {
            return true;
        }
        if self
            .lists
            .iter()
            .any(|face| face.id != list && face.name == name)
        {
            return true;
        }
        self.pending = Some(Request::RenameList { list, name });
        true
    }

    fn heading_rename_key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        match key {
            Key::Named(NamedKey::Enter) => self.commit_heading_rename(),
            Key::Named(NamedKey::Escape) => {
                self.heading_rename = None;
                true
            }
            _ => {
                if let Some(input) = self.heading_rename.as_mut() {
                    input.key(&mut self.engine, key, text, mods);
                }
                true
            }
        }
    }

    /// The heading's rename field, with the caret — drawn with the rows'
    /// panels so it blinks with them.
    pub(super) fn paint_heading_rename(&mut self, scene: &mut Scene, root: Affine, caret_on: bool) {
        let field = self.heading_field();
        if let Some(input) = self.heading_rename.as_mut() {
            input.draw(
                scene,
                root,
                &mut self.engine,
                super::capture::input_rect(field),
                koru_editor::components::text_input::InputStyle::bare(super::draw::TITLE),
                None,
                caret_on,
            );
        }
    }

    /// The band's notice when the note could not be saved or opened, and
    /// *Try again* beside it (plan Q8).
    pub(super) fn paint_notice(&mut self, scene: &mut Scene, root: Affine) {
        self.notice = None;
        self.conflict_actions = None;
        let lang = crate::format::lang();
        let p = theme::p();
        let mut top = self.header_h + self.band_h;
        // The deletion conflict first: its own row, with two answers.
        if self.in_conflict() {
            let message = t(lang, koru_i18n::Key::PageDeletedElsewhere);
            let keep = t(lang, koru_i18n::Key::PageKeepDeleted);
            let recover = t(lang, koru_i18n::Key::PageRecover);
            let line = Rect::new(
                PAD,
                top,
                (self.viewport.0 - 2.0 * PAD).max(0.0),
                self.row_h(),
            );
            let layout = self.engine.simple_layout(message, theme::role::row());
            let y = koru_draw::scene::centre_y_in(&layout, line);
            koru_draw::scene::draw_layout_first_line(
                scene,
                root,
                &layout,
                PAD,
                y,
                Some(p.text_dim),
            );
            let mut x = PAD + layout.width() + CONTROL_GAP * 2.0;
            let recover_layout = self.engine.simple_layout(recover, theme::role::row());
            koru_draw::scene::draw_layout_first_line(
                scene,
                root,
                &recover_layout,
                x,
                y,
                Some(p.accent),
            );
            let recover_rect = Rect::new(x, top, recover_layout.width(), self.row_h());
            x += recover_layout.width() + CONTROL_GAP * 2.0;
            let keep_layout = self.engine.simple_layout(keep, theme::role::row());
            koru_draw::scene::draw_layout_first_line(
                scene,
                root,
                &keep_layout,
                x,
                y,
                Some(p.text_dim),
            );
            let keep_rect = Rect::new(x, top, keep_layout.width(), self.row_h());
            self.conflict_actions = Some((keep_rect, recover_rect));
            top += self.row_h();
        }
        let Some(opening) = self.failure() else {
            return;
        };
        let text = t(
            lang,
            if opening {
                koru_i18n::Key::PageOpenFailed
            } else {
                koru_i18n::Key::PageSaveFailed
            },
        );
        let retry = t(lang, koru_i18n::Key::TryAgain);
        let line = Rect::new(
            PAD,
            top,
            (self.viewport.0 - 2.0 * PAD).max(0.0),
            self.row_h(),
        );
        let layout = self.engine.simple_layout(text, theme::role::row());
        let y = koru_draw::scene::centre_y_in(&layout, line);
        koru_draw::scene::draw_layout_first_line(scene, root, &layout, PAD, y, Some(p.text_dim));
        let x = PAD + layout.width() + CONTROL_GAP * 2.0;
        let action = self.engine.simple_layout(retry, theme::role::row());
        koru_draw::scene::draw_layout_first_line(scene, root, &action, x, y, Some(p.accent));
        self.notice = Some((line, Rect::new(x, top, action.width(), self.row_h())));
    }
}
