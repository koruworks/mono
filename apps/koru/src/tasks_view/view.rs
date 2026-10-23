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
use koru_core::ids::ListId;
use koru_core::schema::WorkspaceDoc;
use koru_core::{DocumentId, NodeId, WorkspaceId};
use koru_draw::chip::ChipKind;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme;
use koru_editor::utils::when::{self, Parsed, Reference};
use koru_store_wire::tasks::{LifecycleFilter, TaskSort};
use koru_ui::date_grid::DateGrid;
use koru_ui::menu::{Anchor, Menu};
use koru_ui::table::{Band, Metrics, Table};
use koru_workspace_host::{Settings, View};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

use super::capture::{Capture, SlashRow, Typed};
use super::echo::{self, Echo, Patch};
use super::menu::{ListPick, OverflowItem, OverflowRow};
use super::pickers::{
    Choice, DuePick, ListPicker, Picked, Picker, PriorityPick, Target, due_seeds, priority_rows,
};
use super::query::{Due, DueWindow, Filters, TaskEntry, TaskListPanel};
use super::states::{self, Empty};

/// The page's own margin — the Trash tab's, because this is the same kind of
/// page: a list read on a wide sheet, which pinned to the window's edges reads
/// as a spreadsheet.
pub(super) const PAD: f32 = 40.0;
/// Between the title and the row area.
pub(super) const HEADER_GAP: f32 = 22.0;
/// The complete control's square, at the head of every row.
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
/// Between that square and the name.
pub(super) const SWATCH_GAP: f32 = 10.0;
/// Between the heading's baseline block and the chips row under it.
pub(super) const CHIPS_TOP_GAP: f32 = 14.0;
/// Air either side of a filter chip's label — **the component's**, so the
/// empty state's action lines up with the chips over it.
pub(super) const FILTER_PAD_X: f32 = koru_ui::filter_chip::PAD_X;
/// Between one filter chip and the next.
pub(super) const FILTER_GAP: f32 = 8.0;
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
    /// A filter chip, by its index in [`TasksView::chips`].
    Chip(usize),
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
    /// **Add a task**, at the right end of an empty bucket's row. The one
    /// control any of the four empty states carries; what it does is put the
    /// caret in the capture row that is already there, rather than opening
    /// anything of its own.
    EmptyAction,
    /// **The list chip on a row**, which is the one chip that is a control:
    /// pressing it opens the list's own page. Resolved ahead of the row it
    /// sits in, as the tick and the `…` are.
    ListChip(usize),
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

/// Which dimension a chip is a value of. **Three dimensions, four chips** — the
/// due window has two, because *Overdue* and *This week* are two answers to one
/// question and pressing either clears the other.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(super) enum ChipPress {
    MinHigh,
    Lifecycle,
    /// The list page's one due chip: a selector over the dimension, so it steps
    /// to the next value rather than toggling one of them. There is no
    /// per-value press any more — the two value chips were withdrawn from the
    /// queues, which were the only pages that drew them.
    WindowNext,
}

/// One chip of the filter row, as both drawing and hit-testing see it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct FilterChip {
    pub label: String,
    /// **This dimension is set**, which is what inverts the chip.
    pub active: bool,
    /// A selector: its label is its current value, and it wears a caret.
    pub caret: bool,
    /// It can be switched off again, and wears the `×` that says so.
    pub clear: bool,
    pub press: ChipPress,
}

impl FilterChip {
    /// What this chip's trailing mark says — **the component's vocabulary**,
    /// so measuring and drawing ask one question rather than two booleans.
    pub(super) fn mark(&self) -> koru_ui::filter_chip::Mark {
        use koru_ui::filter_chip::Mark;
        if self.caret {
            Mark::Caret
        } else if self.clear {
            Mark::Clear
        } else {
            Mark::None
        }
    }
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
    /// The row the keyboard is on, **by identity rather than by index**: the
    /// page refreshes underneath it every half minute, and an index would move
    /// the selection onto whichever task had slid into that position.
    pub(super) selected: Option<(DocumentId, NodeId)>,
    /// A row somebody was sent to — *View task*, *View in Tasks* — lit for a
    /// moment so the eye finds it, as the document flashes a search hit.
    /// The deadline is what drives the frames; it clears itself.
    pub(super) flash: Option<((DocumentId, NodeId), std::time::Instant)>,
    /// The *Add a task…* row while it has the caret — the words, the date
    /// word, and what `/due`, `/priority` and `/task_list` picked.
    pub(super) capture: Option<Capture>,
    /// What the chips row and the overflow menu are set to. **The query's own
    /// fields and nothing else** — see [`Filters`].
    pub(super) filters: Filters,
    /// The chips' widths, in [`chips`](TasksView::chips) order, measured when
    /// the row was last built. Kept beside the chips for the reason a menu's
    /// width is fixed at open time: measuring during a hit test would need a
    /// `&mut TextEngine` nobody there has, and re-measuring per frame would let
    /// a chip move under the pointer because the density changed.
    pub(super) chip_ws: Vec<f32>,
    /// Each row's chips, measured, parallel to [`rows`](Self::rows) — kept
    /// for the same reason `chip_ws` is: a press on a chip is resolved from
    /// the widths, and a hit test never needs an engine.
    pub(super) row_chip_ws: Vec<Vec<f32>>,
    /// The width of the **Add a task** action, measured with the chips and for
    /// their reason: a hit test holds no `&mut TextEngine`, and re-measuring per
    /// frame would let the control move under the pointer when the density did.
    pub(super) action_w: f32,
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
}

impl TasksView {
    pub fn new(target: RailTarget) -> Self {
        let today = chrono::Local::now().date_naive();
        Self {
            viewport: (900.0, 600.0),
            engine: TextEngine::new(),
            panel: TaskListPanel::default(),
            target,
            rows: Vec::new(),
            lines: vec![Line::Capture],
            lists: Vec::new(),
            task_documents: Vec::new(),
            today,
            now_ms: chrono::Utc::now().timestamp_millis(),
            hover: None,
            selected: None,
            flash: None,
            capture: None,
            filters: Filters::default(),
            chip_ws: Vec::new(),
            row_chip_ws: Vec::new(),
            action_w: 0.0,
            overflow: None,
            picker: None,
            panel_instance: 0,
            header_h: HEADER_H_GUESS,
            scroll: 0.0,
            mods: ModifiersState::empty(),
            stale: true,
            pending: None,
            echoes: Vec::new(),
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
        self.selected = None;
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
        // The chip row is not the same row on the new page: a queue names its
        // own lifecycle and a list does not, so one chip comes and goes.
        self.remeasure_chips();
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
        self.remeasure_chips();
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
        let (document, node, patch) = match request {
            Request::Capture {
                title,
                due,
                list,
                priority,
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
            Request::Open { .. } | Request::RowMenu { .. } | Request::OpenList { .. } => return,
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
                list: self.leads_to_list(row),
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
                accent: None,
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
                    .map(|chip| {
                        self.engine.measure_attr_chip(
                            chip.kind,
                            &chip.label,
                            chip.accent.as_deref(),
                            None,
                            false,
                        ) + koru_draw::chip::extra_w(chip.kind)
                    })
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
        if !std::mem::take(&mut self.stale) {
            return false;
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
            self.selected = None;
        }
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

    /// Where the heading's text starts. Past the swatch on a list page, at
    /// [`PAD`] everywhere else — **one function**, because the title, the trail
    /// beside it and the swatch itself are three things that have to agree
    /// about one edge.
    pub(super) fn title_x(&self) -> f32 {
        match self.title_swatch() {
            Some(_) => PAD + HEADER_SWATCH + SWATCH_GAP,
            None => PAD,
        }
    }

    /// The swatch's square, on the heading's own line.
    pub(super) fn swatch_rect(&self) -> Rect {
        let m = super::draw::TITLE.metrics;
        Rect::new(
            PAD,
            self.title_top() + (m.line_height - HEADER_SWATCH) * 0.5,
            HEADER_SWATCH,
            HEADER_SWATCH,
        )
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

    /// The chips this page offers, in the design's order.
    ///
    /// **Only the ones the store will apply, and only where they narrow
    /// anything.** *Priority ≥ High* is ANDed into every query and is offered
    /// everywhere — a filter whose answer is empty is a filter doing its job.
    /// The other two are each withheld from the pages they cannot mean
    /// anything on, and for the same reason wearing two different faces.
    ///
    /// The lifecycle chip: on a queue page the store *discards* the field,
    /// because a bucket names its own lifecycle, so the chip would not narrow
    /// the page, it would be ignored. A control the answer cannot hear is not
    /// offered; the overflow menu says why, where there is room to say it.
    ///
    /// **The due dimension: on a list only.** The design gave a queue the two
    /// value chips and a list the selector, and using it showed the queue half
    /// was wrong: a bucket *is* a due window, so a second one over it is
    /// redundant where it agrees and empty where it does not. *This week* over
    /// Next asks for this week's work inside this week's work; *Overdue* over
    /// Next, Upcoming or Later asks for work due before today inside a page
    /// that starts at today, which answers nothing and can never answer
    /// anything. A list is the one page with no time in it, which is the one
    /// page where *when* is a cut worth offering — as the selector, so the
    /// value that narrows nothing still has a word.
    pub(super) fn chips(&self) -> Vec<FilterChip> {
        let f = self.filters;
        let mut chips = vec![FilterChip {
            label: format!(
                "Priority \u{2265} {}",
                koru_editor::chips::priority_label(super::query::CHIP_MIN_PRIORITY)
            ),
            active: f.min_high,
            caret: false,
            clear: f.min_high,
            press: ChipPress::MinHigh,
        }];
        if Filters::lifecycle_applies(self.target) {
            chips.push(FilterChip {
                label: lifecycle_label(f.lifecycle).to_owned(),
                active: f.lifecycle != LifecycleFilter::Open,
                // A selector: the label *is* the value, so the caret says there
                // are others rather than that it is switched on.
                caret: true,
                clear: false,
                press: ChipPress::Lifecycle,
            });
        }
        if Filters::due_selects(self.target) {
            // One chip, labelled with the value it is on — *Any date* included,
            // which is what `1d` draws on a list page.
            chips.push(FilterChip {
                label: f.window.selector_label().to_owned(),
                active: f.window != DueWindow::Any,
                caret: true,
                clear: false,
                press: ChipPress::WindowNext,
            });
        }
        chips
    }

    /// A chip's height — the component's, derived from the label's own line.
    pub(super) fn chip_h(&self) -> f32 {
        koru_ui::filter_chip::height()
    }

    pub(super) fn chips_top(&self) -> f32 {
        let m = super::draw::TITLE.metrics;
        self.title_top() + m.line_height + CHIPS_TOP_GAP
    }

    /// Re-measure the chip row. Called wherever a label or the set of chips can
    /// have moved: a new page, a new filter, a new font or density.
    pub(super) fn remeasure_chips(&mut self) {
        let chips = self.chips();
        let mut widths = Vec::with_capacity(chips.len());
        for chip in &chips {
            widths.push(koru_ui::filter_chip::width(
                &mut self.engine,
                &chip.label,
                chip.mark(),
            ));
        }
        self.chip_ws = widths;
    }

    /// Where the chips are, from their measured widths and nothing else.
    ///
    /// **The one place this arithmetic exists**, like [`parts`](Self::parts) for
    /// a row: `paint_chips` draws into these rectangles and `hit_at` resolves a
    /// press against them, which is what `the_chips_row_hits_back` proves.
    pub(super) fn chip_rects(&self) -> Vec<Rect> {
        let y = self.chips_top();
        let h = self.chip_h();
        let mut x = PAD;
        self.chip_ws
            .iter()
            .map(|w| {
                let r = Rect::new(x, y, *w, h);
                x += w + FILTER_GAP;
                r
            })
            .collect()
    }

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
            self.target,
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

    /// Where **Add a task** is, or `None` in a state that offers no action.
    ///
    /// Right-aligned in the chip, which is what puts the fact and the thing to
    /// do about it on one line rather than in a panel.
    pub(super) fn empty_action_rect(&self) -> Option<Rect> {
        self.empty_state()?.action()?;
        let chip = self.empty_chip();
        Some(Rect::new(
            chip.x + chip.w - FILTER_PAD_X - self.action_w,
            chip.y,
            self.action_w,
            chip.h,
        ))
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
    pub(super) fn open_row_list(&mut self, i: usize) -> bool {
        self.select_row(i);
        let Some(list) = self.rows.get(i).and_then(|row| self.leads_to_list(row)) else {
            return false;
        };
        self.pending = Some(Request::OpenList { list });
        true
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
        let Some(row) = self.rows.get(i) else {
            return false;
        };
        self.pending = Some(Request::RowMenu {
            document: row.document,
            node: row.node,
            completed: row.completed,
            pinned_today: row.pinned_today,
            native: self.task_documents.contains(&row.document),
            list: self.leads_to_list(row),
            at: (d.x + d.w, d.y + d.h),
        });
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
                .with_header(self.header_h)
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
        self.remeasure_chips();
        // Measured here rather than where it is drawn, with the chips and for
        // the chips' reason — see [`action_w`](Self::action_w).
        self.action_w = self
            .engine
            .simple_layout(states::ADD_A_TASK, theme::role::row())
            .width();
        self.header_h = self.chips_top() + self.chip_h() + HEADER_GAP;
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
        if let Some(i) = self.chip_rects().iter().position(|r| r.contains(x, y)) {
            return Some(Hit::Chip(i));
        }
        // The empty state's action sits below the capture row, where the table
        // has no band at all — so it is resolved here, beside the header's own
        // controls, rather than as a row that does not exist.
        if self.empty_action_rect().is_some_and(|r| r.contains(x, y)) {
            return Some(Hit::EmptyAction);
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
                // The list chip, where it leads somewhere: the one chip that
                // is a control (finding 36). The others stay labels.
                if let Some(entry) = self.rows.get(*row)
                    && let Some(i) = self
                        .row_chips(entry)
                        .iter()
                        .position(|chip| chip.list.is_some())
                    && self
                        .row_chip_rects(r, *row)
                        .get(i)
                        .is_some_and(|c| c.contains(x, y))
                {
                    return Some(Hit::ListChip(*row));
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

    #[cfg(test)]
    pub(super) fn chips_for_test(&self) -> Vec<FilterChip> {
        self.chips()
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
    pub(super) fn chip_rects_for_test(&self) -> Vec<Rect> {
        self.chip_rects()
    }

    /// Which empty state the page is in — the gate itself, so a test asserts on
    /// what is drawn rather than on a sentence it copied.
    #[cfg(test)]
    pub(super) fn empty_for_test(&self) -> Option<Empty> {
        self.empty_state()
    }

    #[cfg(test)]
    pub(super) fn empty_action_rect_for_test(&self) -> Option<Rect> {
        self.empty_action_rect()
    }

    #[cfg(test)]
    pub(super) fn selected_for_test(&self) -> Option<usize> {
        self.selected_row()
    }

    #[cfg(test)]
    pub(super) fn capturing_for_test(&self) -> bool {
        self.capture.is_some()
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

    /// The open priority picker's rows.
    #[cfg(test)]
    pub(super) fn priority_picker_for_test(&self) -> Option<Vec<super::pickers::PriorityRow>> {
        match &self.picker {
            Some(Picker::Priority(picker)) => Some(picker.menu.items().to_vec()),
            _ => None,
        }
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
        let items = super::menu::overflow_items(&self.filters);
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
            OverflowRow::Sort(sort) => f.sort = sort,
            OverflowRow::Divider | OverflowRow::SortHeading => return false,
        }
        self.set_filters(f);
        if let Some(menu) = &mut self.overflow {
            super::menu::sync(menu.items_mut(), &f);
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
        let Some((i, d)) = self.row_anchor(document, node) else {
            return false;
        };
        let items = super::menu::list_picker_items(&self.lists, self.rows[i].list);
        self.overflow = None;
        self.panel_instance += 1;
        self.picker = Some(Picker::List(ListPicker {
            target: Target::row(document, node),
            menu: Menu::aligned(
                &mut self.engine,
                (d.x + d.w, d.y + d.h),
                items,
                Anchor::ButtonRight,
            ),
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

    /// *Priority…* over one row: the editor's four levels in the scale's own
    /// order, the row's current one checked, Clear while one is set. The same
    /// anchoring and the same refusal as [`open_list_picker`](Self::open_list_picker).
    pub fn open_priority_picker(&mut self, document: DocumentId, node: NodeId) -> bool {
        let Some((i, d)) = self.row_anchor(document, node) else {
            return false;
        };
        let current = self.rows[i].priority.as_deref().and_then(Priority::parse);
        let items = priority_rows(current);
        self.overflow = None;
        self.panel_instance += 1;
        self.picker = Some(Picker::Priority(PriorityPick {
            target: Target::row(document, node),
            menu: Menu::aligned(
                &mut self.engine,
                (d.x + d.w, d.y + d.h),
                items,
                Anchor::ButtonRight,
            ),
        }));
        true
    }

    /// *Due…* over one row: the shared calendar, opened on the row's own day
    /// with Clear offered when it has one, else on today — the editor's rule
    /// for its picker, read off the row.
    ///
    /// **The one clock read on this path**, handed to the grid so its phrase
    /// field resolves against the same instant the panel opened on; the
    /// widget reads no clock of its own.
    pub fn open_due_picker(&mut self, document: DocumentId, node: NodeId) -> bool {
        let Some((i, d)) = self.row_anchor(document, node) else {
            return false;
        };
        let now = chrono::Local::now().naive_local();
        let (selected, offers_clear) = due_seeds(self.rows[i].due_day(), self.today);
        let grid = DateGrid::with_phrase_field(&mut self.engine, selected, now, offers_clear);
        let panel = DuePick::place(d, grid.size(), self.viewport);
        self.overflow = None;
        self.panel_instance += 1;
        self.picker = Some(Picker::Due(DuePick {
            target: Target::row(document, node),
            grid,
            panel,
            clear_hovered: false,
        }));
        true
    }

    /// A `/` row chosen on the capture: the same three pickers the row's `…`
    /// hangs, over the capture row, deciding for it rather than for a task.
    fn open_capture_picker(&mut self, row: SlashRow) -> bool {
        let Some(capture) = self.capture.as_ref() else {
            return false;
        };
        let r = self.table().row_rect(&self.bands(), 0);
        let d = self.parts(r).dots;
        let at = (d.x + d.w, d.y + d.h);
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
                    menu: Menu::aligned(&mut self.engine, at, items, Anchor::ButtonRight),
                }));
            }
            SlashRow::Priority => {
                let items = priority_rows(capture.priority);
                self.picker = Some(Picker::Priority(PriorityPick {
                    target: Target::Capture,
                    menu: Menu::aligned(&mut self.engine, at, items, Anchor::ButtonRight),
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
                let panel = DuePick::place(d, grid.size(), self.viewport);
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
        match picker.target {
            Target::Row { document, node } => {
                self.pending = Some(Request::SetList {
                    document,
                    node,
                    list,
                });
            }
            Target::Capture => {
                if let Some(capture) = self.capture.as_mut() {
                    capture.list = list;
                }
            }
        }
        self.picker = None;
        true
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
            Some(Picker::Due(_)) => Err(()),
            None => return false,
        };
        match row {
            Err(()) => self.press_due(x, y),
            // A press anywhere else dismisses it and does nothing more — the
            // rule every menu in this app follows.
            Ok(None) => self.picker = None,
            Ok(Some(row)) => {
                if matches!(self.picker, Some(Picker::List(_))) {
                    self.choose_list(row);
                } else {
                    self.choose_priority(row);
                }
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
                    _ => {}
                }
            }
            Key::Named(NamedKey::Enter) => {
                let hover = match &self.picker {
                    Some(Picker::List(picker)) => picker.menu.hover,
                    Some(Picker::Priority(picker)) => picker.menu.hover,
                    _ => None,
                };
                if let Some(row) = hover {
                    if matches!(self.picker, Some(Picker::List(_))) {
                        self.choose_list(row);
                    } else {
                        self.choose_priority(row);
                    }
                }
            }
            _ => {}
        }
        true
    }

    // ------------------------------------------------------------- input

    fn press(&mut self, x: f32, y: f32) -> bool {
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
            // **The action opens nothing of its own.** The capture row is
            // already the list's first row; this puts the caret in it, which is
            // what "Add a task" has meant on this page since V-2.
            Some(Hit::EmptyAction) => {
                self.begin_capture();
                true
            }
            Some(Hit::Chip(i)) => {
                let Some(chip) = self.chips().get(i).cloned() else {
                    return false;
                };
                self.press_chip(chip.press)
            }
            Some(Hit::Capture) => {
                self.begin_capture();
                let rect = self.capture_rect();
                if let Some(capture) = self.capture.as_mut() {
                    capture.input.click(&mut self.engine, rect, x, y);
                }
                true
            }
            Some(Hit::Complete(i)) => self.toggle_complete(i),
            Some(Hit::Dots(i)) => {
                self.select_row(i);
                let bands = self.bands();
                let Some(line) = self.lines.iter().position(|l| l == &Line::Task(i)) else {
                    return false;
                };
                let d = self.parts(self.table().row_rect(&bands, line)).dots;
                let Some(row) = self.rows.get(i) else {
                    return false;
                };
                self.pending = Some(Request::RowMenu {
                    document: row.document,
                    node: row.node,
                    completed: row.completed,
                    pinned_today: row.pinned_today,
                    native: self.task_documents.contains(&row.document),
                    list: self.leads_to_list(row),
                    // The button's bottom-right, so the menu hangs back under
                    // it rather than out past the window's edge.
                    at: (d.x + d.w, d.y + d.h),
                });
                true
            }
            // **The list chip opens the list.** The owner's ask (finding 36),
            // and the one exception to the rule under `Hit::Row`: the chip
            // names a page, and a name you can see and cannot follow is the
            // dead end the rail's rows never are.
            Some(Hit::ListChip(i)) => self.open_row_list(i),
            // **A click selects, and that is all it does.** The design's
            // sentence was *a whole-row click opens the node*, and it is the
            // reason this page has no inline affordances — but a list whose
            // every row is a trapdoor cannot be read. Skimming a queue, picking
            // a row to act on with the keyboard, or pressing near a row's edge
            // all threw the page away and replaced it with a document. Leaving
            // is now something asked for by name, on the row's own menu.
            Some(Hit::Row(i)) => {
                self.select_row(i);
                true
            }
            None => false,
        }
    }

    /// Press one chip.
    ///
    /// **A dimension, not a chip.** Each arm moves one dimension and never
    /// combines two: the due selector steps to its next value rather than
    /// ANDing windows that exclude each other.
    pub(super) fn press_chip(&mut self, which: ChipPress) -> bool {
        let mut f = self.filters;
        match which {
            ChipPress::MinHigh => f.min_high = !f.min_high,
            ChipPress::Lifecycle => f.lifecycle = next_lifecycle(f.lifecycle),
            ChipPress::WindowNext => f.window = f.window.next(),
        }
        self.set_filters(f)
    }

    /// Enter on the capture row. `true` when it produced a request.
    fn commit_capture(&mut self) -> bool {
        let request = self.capture_request();
        self.end_capture();
        match request {
            Some(request) => {
                self.pending = Some(request);
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
    /// The list this chip opens when pressed — `Some` on the list chip alone,
    /// and only where the page is not already that list's own. Every other
    /// chip is a label.
    pub list: Option<ListId>,
}

impl RowChip {
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
    pub control: Rect,
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

/// What the lifecycle selector's own label says. **Its value, not its name** —
/// the chip is a selector, so the word on it is what the page is showing.
pub fn lifecycle_label(lifecycle: LifecycleFilter) -> &'static str {
    match lifecycle {
        LifecycleFilter::Open => "Open",
        LifecycleFilter::Completed => "Completed",
        LifecycleFilter::All => "All",
    }
}

/// The next value the selector offers. Three values, so it cycles rather than
/// toggling; *Open* is the default and therefore the one it comes back to.
pub fn next_lifecycle(lifecycle: LifecycleFilter) -> LifecycleFilter {
    match lifecycle {
        LifecycleFilter::Open => LifecycleFilter::Completed,
        LifecycleFilter::Completed => LifecycleFilter::All,
        LifecycleFilter::All => LifecycleFilter::Open,
    }
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
        _ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
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
                self.selected = None;
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
                self.selected = None;
                had
            }
            _ => false,
        }
    }

    fn mouse_down(&mut self, _ws: &mut WorkspaceDoc, x: f32, y: f32, _ctrl: bool) -> bool {
        self.press(x, y)
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
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
            let rect = self.capture_rect();
            let engine = &mut self.engine;
            if let Some(capture) = self.capture.as_mut()
                && capture.input.drag(engine, rect, x)
            {
                return true;
            }
        }
        was != self.hover
    }

    fn mouse_up(&mut self, _ws: &mut WorkspaceDoc) -> bool {
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
    fn mouse_double_click(&mut self, _ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        if !matches!(self.hit_at(x, y), Some(Hit::Row(_))) {
            return false;
        }
        self.open_selected(true)
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
        let Some(Hit::Row(i) | Hit::Complete(i) | Hit::Dots(i) | Hit::ListChip(i)) =
            self.hit_at(x, y)
        else {
            return false;
        };
        let Some(row) = self.rows.get(i) else {
            return false;
        };
        self.pending = Some(Request::RowMenu {
            document: row.document,
            node: row.node,
            completed: row.completed,
            pinned_today: row.pinned_today,
            native: self.task_documents.contains(&row.document),
            list: self.leads_to_list(row),
            at: (x, y),
        });
        true
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
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
        self.clamp_scroll();
    }

    fn set_mods(&mut self, mods: ModifiersState) {
        self.mods = mods;
    }

    /// **Every hit is [`PRESS`], which is the plain arrow** ([ADR-215]): a
    /// control is identified by its own appearance, and each of these has one —
    /// the chip's lozenge inverts, the `…` lights, a row fills.
    ///
    /// [PRESS]: koru_ui::cursor::PRESS
    /// [ADR-215]: ../../../docs/decisions/ADR-201-225.md#adr-215-nohandcursor--koru-does-not-use-the-hand-cursor-a-press-target-is-identified-by-its-own-appearance
    fn cursor_icon(&self) -> CursorIcon {
        match self.hover {
            Some(
                Hit::Complete(_)
                | Hit::Dots(_)
                | Hit::Row(_)
                | Hit::ListChip(_)
                | Hit::Capture
                | Hit::Chip(_)
                | Hit::HeaderDots
                | Hit::EmptyAction,
            ) => koru_ui::cursor::PRESS,
            None => koru_ui::cursor::IDLE,
        }
    }

    /// Only a flag. The page is read in `catch_up`, which runs for the tab
    /// about to be drawn and not for the ones that are not.
    fn adopt(&mut self, _ws: &WorkspaceDoc) {
        self.stale = true;
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
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        _workspace: &koru_workspace::Workspace,
        _images: &koru_draw::ImageCache,
        root: Affine,
        caret_on: bool,
        _pointer: (f32, f32),
    ) {
        self.paint(scene, root, caret_on);
    }

    fn restore(&mut self, _ws: &koru_workspace::Workspace, _settings: &Settings) {}

    fn capture(&self, _settings: &mut Settings) {}
}
