// SPDX-License-Identifier: MPL-2.0

//! The context column: what hangs off whatever is selected.
//!
//! **Three of the five sections have a model behind them now.** History has had
//! one since versions shipped; Tasks has one as of V-5 — the tasks *in the
//! document this column is about*, which is a question the open document can
//! answer on its own — and Reminders has one of its own. Comments and Assets
//! stay **parked** (owner decision, 2026-08-31): no comment nodes, no
//! attachment kind, no thread — and a convincing-looking empty panel reads as
//! broken rather than unbuilt. The seam stays: the variants, their labels and
//! the tab-row machinery remain, and a section returns by adding it to
//! [`Section::VISIBLE`]. While one section is visible the tab row is not drawn
//! at all — a single tab is a button that does nothing.
//!
//! **Tasks is hidden for now** (`koru_features::CONTEXT_TASKS_SHOWN`, the
//! user, 2026-11-03): it stays in `VISIBLE` with its model and its rows, and
//! [`Section::shown_in_product`] keeps it off the screen. In a build that also
//! hides Reminders and Cloud sync that leaves History alone, so the column
//! opens on it with no tab row — the lone-tab rule above, not a new one.
//!
//! **Chrome still holds no model.** The filled sections are handed rows of
//! pre-formatted strings by the container and report a press back out; nothing
//! here reads a document, formats a date or knows what a lifecycle is. That is
//! why the tasks rows carry a `due` that is already a word and not a temporal:
//! this code has no timezone, no locale and no opinion about either.

use koru_core::NodeId;
use koru_core::schema::WorkspaceDoc;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme;
use koru_draw::{icons, scene};
use koru_ui::scrollbar::{self, ScrollbarGeom};
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Fill;
use winit::window::CursorIcon;

use super::Outcome;
use koru_shell_chrome::tabs::draw_icon;

/// What the column can show about the current subject.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum Section {
    Reminders,
    #[allow(dead_code)] // Parked by product decision; retained as the reactivation seam.
    Comments,
    #[allow(dead_code)] // Parked by product decision; retained as the reactivation seam.
    Assets,
    #[allow(dead_code)] // Parked by product decision; retained as the reactivation seam.
    Tasks,
    #[default]
    History,
    /// **What Koru Cloud has done to the document in front of you.** Added so
    /// the answer to "did this reach the other machine, and when" is on the
    /// screen rather than in a gateway log beside a Docker log.
    CloudSync,
}

impl Section {
    /// Every section the column knows, shown or parked. Tests keep the parked
    /// labels and empty states ready for a section to rejoin [`Self::VISIBLE`].
    #[cfg(test)]
    pub const ALL: [Section; 6] = [
        Section::Reminders,
        Section::Comments,
        Section::Assets,
        Section::Tasks,
        Section::History,
        Section::CloudSync,
    ];

    /// The sections the column offers today. The default section must be in
    /// this list.
    pub const VISIBLE: &[Section] = &[
        Section::Tasks,
        Section::Reminders,
        Section::History,
        Section::CloudSync,
    ];

    /// Whether this build shows the section (`koru_features`,
    /// guide/contributing/FEATURE-FLAGS.md): Tasks with tasks, Reminders with
    /// reminders, Cloud sync with Koru Cloud. History always.
    ///
    /// **Tasks is also hidden for now**, by a constant rather than a flag
    /// (`koru_features::CONTEXT_TASKS_SHOWN`): it stays in [`Self::VISIBLE`]
    /// and keeps its code, and this is the one answer every way in asks —
    /// the tab row, a press, [`ContextView::select`] and so `Ctrl+Alt+J`.
    pub fn shown_in_product(self) -> bool {
        match self {
            Self::Tasks => koru_features::tasks() && koru_features::context_tasks(),
            Self::Reminders => koru_features::reminders(),
            Self::CloudSync => koru_features::cloud_sync(),
            Self::Comments | Self::Assets | Self::History => true,
        }
    }

    /// [`VISIBLE`](Self::VISIBLE) minus what this build hides, in the same
    /// order — the tabs the column draws and the keyboard steps through.
    pub fn visible() -> Vec<Section> {
        Self::VISIBLE
            .iter()
            .copied()
            .filter(|section| section.shown_in_product())
            .collect()
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Reminders => "Reminders",
            Self::Comments => "Comments",
            Self::Assets => "Assets",
            Self::Tasks => "Tasks",
            Self::History => "History",
            Self::CloudSync => "Cloud sync",
        }
    }

    fn icon(self) -> char {
        match self {
            Self::Reminders => icons::BELL,
            Self::Comments => icons::CHAT_CIRCLE,
            Self::Assets => icons::PAPERCLIP,
            Self::Tasks => icons::LIST_CHECKS,
            Self::History => icons::CLOCK,
            Self::CloudSync => icons::CLOUD,
        }
    }

    /// What an empty section says.
    ///
    /// Two registers, and the difference is deliberate. Comments and Assets name
    /// a thing that does not exist *yet* — the column reads as unbuilt rather
    /// than broken. **Tasks no longer does**: it has a model behind it, so an
    /// empty section is a fact about this document rather than about the
    /// feature, and *No tasks reference this yet* would now be untrue in the
    /// ordinary way as well as the old one — a task in this document does not
    /// "reference" it.
    fn empty_line(self) -> &'static str {
        match self {
            Self::Reminders => "",
            Self::Comments => "No comments on this yet.",
            Self::Assets => "Nothing attached to this yet.",
            Self::Tasks => "No tasks in this document.",
            Self::History => "No saved versions yet.",
            // Like Tasks and unlike Comments, this is a fact about *this
            // document* rather than about an unbuilt feature — so it is
            // replaced by the localized line the container injects the moment
            // there is one, and this stands only until then.
            Self::CloudSync => "No Cloud pass has run for this document yet.",
        }
    }
}

#[derive(Default)]
pub struct ContextView {
    section: Section,
    hover: Option<Section>,
    history: Vec<HistoryRow>,
    hovered_history: Option<usize>,
    selected_history: Vec<String>,
    history_status: Option<String>,
    /// How far the version list is scrolled. Clamped wherever it changes and
    /// again at draw, because the list and the column both resize under it.
    history_scroll: f32,
    /// Offset from the thumb top while the list's scrollbar is grabbed.
    history_drag: Option<f32>,
    tasks: Vec<TaskRow>,
    hovered_task: Option<usize>,
    reminder_label: String,
    cloud_label: String,
    /// The Cloud section's lines, already written in the reader's language by
    /// the container. Chrome carries strings — see this module's header.
    cloud: Vec<CloudLine>,
    /// The one-line footnote under the lines: the images limitation, or the
    /// acknowledgement that *Copy details* did something.
    cloud_note: Option<String>,
    cloud_copy_label: String,
    cloud_copy_hovered: bool,
}

/// One line of the Cloud section.
///
/// Two strings and no model: `label` is the reading ("Last pass"), `value` is
/// the fact ("12:36:30 · settled 9, uploaded 1, downloaded 0"). Split so the
/// label can be dimmed against the fact, which is the reading every other row
/// in this column uses; joined by the container would lose that.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CloudLine {
    pub label: String,
    pub value: String,
    /// The same line with its exact moment spelled out, for *Copy details*.
    ///
    /// **The column shows "1h" and the clipboard says the hour.** A dense
    /// column reads better relative — it is the form History's own rows use —
    /// and a message saying "it last uploaded 1h ago" is useless by the time
    /// it is read. Empty means the line has no moment in it and the value
    /// already says everything.
    pub detail: String,
}

/// One task of the document this column is about.
///
/// **Strings and one id.** The title, the due and the lifecycle are all decided
/// by the container before they get here — a due is a *word* rather than a
/// temporal, for the reason this module's header gives — and the id is carried
/// only so a press can name the row it landed on. Nothing here reads a document.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TaskRow {
    pub node: NodeId,
    pub title: String,
    /// The due as a person reads it, or `None` for a task that has none. A task
    /// can be a task without one — the marker is the other half of the
    /// derivation — so the absence is normal and draws nothing.
    pub due: Option<String>,
    pub completed: bool,
    /// Due and already past. **Not derived here**: what counts as past depends
    /// on the reader's zone, which is exactly the kind of thing this code does
    /// not know.
    pub overdue: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HistoryRow {
    pub key: String,
    pub label: String,
    /// What the row's tooltip says — the exact moment behind the relative
    /// time in `label`. Pre-formatted by the app: chrome carries strings and
    /// stays ignorant of timestamps and languages alike.
    pub detail: String,
}

/// Height of the row of section buttons, when more than one section is
/// visible.
const TABS_H: f32 = 30.0;
/// One version row's pitch and height.
const ROW_PITCH: f32 = 29.0;
const ROW_H: f32 = 27.0;

/// The History section's fixed chrome and its scrollable list.
struct HistoryLayout {
    save: Rect,
    compare: Rect,
    /// Where the one-line comparison summary sits — fixed under the buttons,
    /// so it cannot scroll away with the rows it summarises.
    status_y: f32,
    /// The rows' viewport. Everything in `rows` is clipped to this.
    viewport: Rect,
    /// One rect per row, already offset by the scroll.
    rows: Vec<Rect>,
    content_h: f32,
}

/// One Cloud line's band, and the step between two. A line is a label and a
/// value stacked, so it is taller than a task row by design.
const CLOUD_LINE_H: f32 = 30.0;
const CLOUD_LINE_STEP: f32 = 32.0;

/// The Cloud section's geometry: where each line sits, where the footnote
/// goes, and where the one action is.
///
/// **One function, three callers** — drawing, the hover and the press — the
/// same rule `task_rects` follows, and the reason a row cannot be pressable
/// somewhere other than where it is drawn.
struct CloudLayout {
    lines: Vec<Rect>,
    note_y: f32,
    copy: Rect,
}

impl HistoryLayout {
    fn max_scroll(&self) -> f32 {
        (self.content_h - self.viewport.h).max(0.0)
    }

    fn scrollbar(&self, scroll: f32) -> Option<ScrollbarGeom> {
        ScrollbarGeom::for_content(self.viewport, self.content_h, scroll)
    }
}

impl ContextView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn section(&self) -> Section {
        self.section
    }

    /// Show a section without a press — the keyboard's door into this column,
    /// driven by `MainView::next_context_section`.
    pub fn select(&mut self, section: Section) -> bool {
        if self.section == section || !section.shown_in_product() {
            return false;
        }
        self.section = section;
        true
    }

    /// Show `section` even while this build hides it — for the suite alone,
    /// so a section hidden for now (Tasks, `koru_features::CONTEXT_TASKS_SHOWN`)
    /// keeps its tests rather than losing them with its tab.
    #[cfg(test)]
    pub fn show_hidden_section(&mut self, section: Section) {
        self.section = section;
    }

    /// Localized reminder copy is injected by the app during maintenance so
    /// chrome remains a geometry host rather than a locale owner.
    pub fn set_reminder_label(&mut self, section: String) -> bool {
        if self.reminder_label == section {
            return false;
        }
        self.reminder_label = section;
        true
    }

    /// Localized Cloud copy is injected the same way Reminders' label is, and
    /// for the same reason: chrome stays a geometry host rather than a locale
    /// owner. `lines` is already in reading order and already written; the
    /// column neither formats a time nor knows what a refusal code means.
    ///
    /// Answers whether anything moved, so the container can skip a redraw —
    /// the contract every push into chrome keeps.
    pub fn set_cloud(
        &mut self,
        label: String,
        lines: Vec<CloudLine>,
        note: Option<String>,
        copy_label: String,
    ) -> bool {
        if self.cloud_label == label
            && self.cloud == lines
            && self.cloud_note == note
            && self.cloud_copy_label == copy_label
        {
            return false;
        }
        self.cloud_label = label;
        self.cloud = lines;
        self.cloud_note = note;
        self.cloud_copy_label = copy_label;
        true
    }

    /// Whether the Cloud section has anything to copy. The container asks
    /// before it offers the action from the keyboard, so a chord over an empty
    /// section copies nothing rather than an empty string.
    pub fn has_cloud_details(&self) -> bool {
        !self.cloud.is_empty()
    }

    /// The Cloud section's facts as plain text, one line each — what *Copy
    /// details* puts on the clipboard, and what a keyboard press copies.
    ///
    /// Built here rather than in the container because this is exactly what is
    /// on the screen: a copy that was assembled somewhere else can disagree
    /// with what the person is looking at, which is the one thing a "paste me
    /// what it says" action must not do.
    pub fn cloud_details(&self) -> String {
        let mut out = String::new();
        for line in &self.cloud {
            out.push_str(&line.label);
            out.push_str(": ");
            out.push_str(if line.detail.is_empty() {
                &line.value
            } else {
                &line.detail
            });
            out.push('\n');
        }
        if let Some(note) = &self.cloud_note {
            out.push_str(note);
            out.push('\n');
        }
        out
    }

    fn section_label(&self, section: Section) -> &str {
        match section {
            Section::Reminders if !self.reminder_label.is_empty() => &self.reminder_label,
            Section::CloudSync if !self.cloud_label.is_empty() => &self.cloud_label,
            _ => section.label(),
        }
    }

    /// The tab row's extra height — zero while only one section is visible,
    /// because a lone tab is a button that does nothing.
    fn tabs_extra() -> f32 {
        if Section::visible().len() > 1 {
            TABS_H + theme::NAV_PAD
        } else {
            0.0
        }
    }

    /// The part below the generic section tabs. An embedding domain owns what
    /// appears there; chrome owns only the stable column geometry.
    // Unconstructed since the ADR-262 move made dead code visible; a later wave of
    // the app-shell migration deletes or revives it.
    #[allow(dead_code)]
    pub fn body_rect(area: Rect) -> Rect {
        let y = area.y + theme::NAV_PAD + Self::tabs_extra();
        Rect::new(area.x, y, area.w, (area.y + area.h - y).max(0.0))
    }

    /// The visible section buttons. Empty while the tab row is not drawn.
    /// One function, both paths — the rule every piece of chrome in this
    /// crate follows.
    pub fn section_rects(area: Rect) -> Vec<(Section, Rect)> {
        let visible = Section::visible();
        if visible.len() <= 1 {
            return Vec::new();
        }
        let inner = (area.w - theme::NAV_PAD * 2.0).max(0.0);
        let n = visible.len() as f32;
        visible
            .iter()
            .enumerate()
            .map(|(i, &s)| {
                // Derive both edges from the same division. Repeatedly adding
                // a fractional width can round one edge over its neighbour.
                let left = area.x + theme::NAV_PAD + inner * i as f32 / n;
                let right = area.x + theme::NAV_PAD + inner * (i as f32 + 1.0) / n;
                (
                    s,
                    Rect::new(left, area.y + theme::NAV_PAD, right - left, TABS_H),
                )
            })
            .collect()
    }

    fn section_at(area: Rect, x: f32, y: f32) -> Option<Section> {
        if area.w <= 0.0 {
            return None;
        }
        Self::section_rects(area)
            .into_iter()
            .find(|(_, r)| r.w > 0.0 && r.contains(x, y))
            .map(|(s, _)| s)
    }

    fn history_layout(&self, area: Rect) -> HistoryLayout {
        let top = area.y + theme::NAV_PAD + Self::tabs_extra() + 26.0;
        let x = area.x + theme::NAV_PAD * 2.0;
        let w = (area.w - theme::NAV_PAD * 4.0).max(1.0);
        let save = Rect::new(x, top, w, 28.0);
        let compare = Rect::new(x, top + 34.0, w, 28.0);
        let status_y = top + 70.0;
        let list_top = if self.history_status.is_some() {
            status_y + 24.0
        } else {
            status_y
        };
        let bottom = (area.y + area.h - theme::NAV_PAD).max(list_top);
        let viewport = Rect::new(x, list_top, w, bottom - list_top);
        let content_h = (self.history.len() as f32 * ROW_PITCH - (ROW_PITCH - ROW_H)).max(0.0);
        let scroll = self
            .history_scroll
            .clamp(0.0, (content_h - viewport.h).max(0.0));
        let rows = (0..self.history.len())
            .map(|i| Rect::new(x, list_top + i as f32 * ROW_PITCH - scroll, w, ROW_H))
            .collect();
        HistoryLayout {
            save,
            compare,
            status_y,
            viewport,
            rows,
            content_h,
        }
    }

    fn cloud_layout(&self, area: Rect) -> CloudLayout {
        let top = area.y + theme::NAV_PAD + Self::tabs_extra() + 26.0;
        let x = area.x + theme::NAV_PAD * 2.0;
        let w = (area.w - theme::NAV_PAD * 4.0).max(1.0);
        let lines = (0..self.cloud.len())
            .map(|i| Rect::new(x, top + i as f32 * CLOUD_LINE_STEP, w, CLOUD_LINE_H))
            .collect::<Vec<_>>();
        // The footnote sits under the last line whether or not there is one,
        // so the action does not jump when a note appears and disappears.
        let note_y = top + self.cloud.len() as f32 * CLOUD_LINE_STEP + 4.0;
        let copy = Rect::new(x, note_y + 34.0, w, 28.0);
        CloudLayout {
            lines,
            note_y,
            copy,
        }
    }

    fn clamp_history_scroll(&mut self, layout: &HistoryLayout) {
        self.history_scroll = self.history_scroll.clamp(0.0, layout.max_scroll());
    }

    /// The wheel over the column. Only the version list scrolls; everything
    /// else in the column is fixed chrome.
    pub fn scroll_by(&mut self, area: Rect, dy: f32) -> bool {
        if self.section != Section::History {
            return false;
        }
        let layout = self.history_layout(area);
        let before = self.history_scroll;
        self.history_scroll -= dy;
        self.clamp_history_scroll(&layout);
        self.history_scroll != before
    }

    /// Window coordinates, not panel-local: this is chrome, and the container
    /// hands it the rect it lives in.
    pub fn mouse_down(&mut self, area: Rect, x: f32, y: f32) -> Outcome {
        if let Some(s) = Self::section_at(area, x, y) {
            self.section = s;
            return Outcome::Consumed;
        }
        // **A task row reveals; it never opens a tab and never writes.** Read
        // only this release, and the same gesture History's rows have: a press
        // takes you to the thing the row is about, in the document already in
        // front of you.
        if self.section == Section::Tasks
            && let Some(index) = task_rects(area, self.tasks.len())
                .iter()
                .position(|r| r.contains(x, y))
            && let Some(row) = self.tasks.get(index)
        {
            return Outcome::RevealTask(row.node);
        }
        // **The one action this section has**, and only while the section is
        // showing — the rule `a_task_row_is_only_pressable_while_its_section_is_showing`
        // states for Tasks.
        if self.section == Section::CloudSync && self.has_cloud_details() {
            let layout = self.cloud_layout(area);
            if layout.copy.contains(x, y) {
                return Outcome::CopyCloudDetails;
            }
        }
        if self.section == Section::History {
            let layout = self.history_layout(area);
            // The scrollbar's wider gutter outranks the rows under it, the
            // rule the document and explore scrollbars already follow.
            if let Some(geom) = layout.scrollbar(self.history_scroll) {
                if geom.on_thumb(x, y) {
                    self.history_drag = Some(y - geom.thumb_top);
                    return Outcome::Consumed;
                }
                if geom.in_gutter(x, y) {
                    self.history_scroll = geom.scroll_for_pointer(y, geom.thumb_h / 2.0);
                    self.clamp_history_scroll(&layout);
                    self.history_drag = Some(geom.thumb_h / 2.0);
                    return Outcome::Consumed;
                }
            }
            if layout.save.contains(x, y) {
                return Outcome::SaveVersion;
            }
            if layout.compare.contains(x, y) {
                match self.selected_history.as_slice() {
                    // One selection compares against the document as it is
                    // now; two compare each other.
                    [only] => return Outcome::CompareWithCurrent(only.clone()),
                    [left, right] => {
                        return Outcome::CompareVersions(left.clone(), right.clone());
                    }
                    _ => {}
                }
            }
            if layout.viewport.contains(x, y)
                && let Some((index, _)) = layout
                    .rows
                    .iter()
                    .enumerate()
                    .find(|(_, row)| row.contains(x, y))
                && let Some(item) = self.history.get(index)
            {
                if let Some(at) = self
                    .selected_history
                    .iter()
                    .position(|key| key == &item.key)
                {
                    self.selected_history.remove(at);
                } else {
                    if self.selected_history.len() == 2 {
                        self.selected_history.remove(0);
                    }
                    self.selected_history.push(item.key.clone());
                }
            }
        }

        // Either way it was ours — a press on the column's background must not
        // fall through to whatever is behind it.
        Outcome::Consumed
    }

    /// The tasks of the document this column is about, in document order.
    ///
    /// Answers whether anything moved, so the container can skip a redraw — the
    /// same contract every push into chrome keeps.
    pub fn set_tasks(&mut self, rows: Vec<TaskRow>) -> bool {
        if self.tasks == rows {
            return false;
        }
        self.tasks = rows;
        // A hover keyed by index over a list that has just changed length is a
        // hover on whatever slid into that place.
        self.hovered_task = None;
        true
    }

    /// What the Tasks section is showing. Read by the container's suite, which
    /// is where *the document reached the column* is a fact about the push —
    /// and by nothing else, which is what the gate says out loud. Drawing reads
    /// the field directly; a reader that exists only for a test and is not
    /// marked as one is a warning on every build until somebody deletes it.
    #[cfg(test)]
    pub fn tasks(&self) -> &[TaskRow] {
        &self.tasks
    }

    pub fn set_history(&mut self, rows: Vec<HistoryRow>, status: Option<String>) {
        self.history = rows;
        self.history_status = status;
        self.selected_history
            .retain(|key| self.history.iter().any(|row| &row.key == key));
    }

    pub fn mouse_move(&mut self, area: Rect, x: f32, y: f32) -> bool {
        if let Some(grab) = self.history_drag {
            let layout = self.history_layout(area);
            if let Some(geom) = layout.scrollbar(self.history_scroll) {
                self.history_scroll = geom.scroll_for_pointer(y, grab);
                self.clamp_history_scroll(&layout);
            }
            return true;
        }
        let hover = Self::section_at(area, x, y);
        let moved = hover != self.hover;
        self.hover = hover;

        let hovered_history = (self.section == Section::History)
            .then(|| {
                let layout = self.history_layout(area);
                layout
                    .viewport
                    .contains(x, y)
                    .then(|| layout.rows.iter().position(|row| row.contains(x, y)))
            })
            .flatten()
            .flatten();
        let history_moved = hovered_history != self.hovered_history;
        self.hovered_history = hovered_history;
        let hovered_task = (self.section == Section::Tasks)
            .then(|| {
                task_rects(area, self.tasks.len())
                    .iter()
                    .position(|r| r.contains(x, y))
            })
            .flatten();
        let task_moved = hovered_task != self.hovered_task;
        self.hovered_task = hovered_task;
        let cloud = (self.section == Section::CloudSync && self.has_cloud_details())
            .then(|| self.cloud_layout(area));
        let copy_hovered = cloud
            .as_ref()
            .is_some_and(|layout| layout.copy.contains(x, y));
        let copy_moved = copy_hovered != self.cloud_copy_hovered;
        self.cloud_copy_hovered = copy_hovered;
        moved || history_moved || task_moved || copy_moved
    }

    /// A released button ends a scrollbar drag. `true` if one was live.
    pub fn mouse_up(&mut self) -> bool {
        self.history_drag.take().is_some()
    }

    /// Whether the list's scrollbar is being dragged — the one drag this
    /// column owns, reported so the container keeps forwarding pointer moves.
    pub fn is_dragging(&self) -> bool {
        self.history_drag.is_some()
    }

    pub fn clear_hover(&mut self) -> bool {
        let had = self.hover.is_some()
            || self.hovered_history.is_some()
            || self.hovered_task.is_some()
            || self.cloud_copy_hovered;
        self.hover = None;
        self.hovered_history = None;
        self.hovered_task = None;
        self.cloud_copy_hovered = false;
        had
    }

    /// The hovered history row's index, anchor rect and tooltip text, for the
    /// container to hand to its tooltip. Chrome owns the hover and geometry;
    /// what the tooltip *is* stays with the container, like every tooltip in
    /// the app.
    pub fn history_tooltip(&self, area: Rect) -> Option<(usize, Rect, &str)> {
        let index = self.hovered_history?;
        let row = self.history.get(index)?;
        if row.detail.is_empty() {
            return None;
        }
        let layout = self.history_layout(area);
        let anchor = *layout.rows.get(index)?;
        // A row scrolled out from under the pointer anchors nothing.
        if anchor.y + anchor.h < layout.viewport.y
            || anchor.y > layout.viewport.y + layout.viewport.h
        {
            return None;
        }
        Some((index, anchor, &row.detail))
    }

    /// Both arms are the plain arrow — Koru does not use the hand cursor
    /// (`docs/features/design/DESIGN.md`) — and the hit test is kept because it is the record
    /// of which half of the column is pressable.
    pub fn cursor_icon(&self) -> CursorIcon {
        if self.hover.is_some() || self.hovered_task.is_some() || self.cloud_copy_hovered {
            koru_ui::cursor::PRESS
        } else {
            koru_ui::cursor::IDLE
        }
    }

    /// `subject` is the node the active tab is showing, or `None` when it is
    /// showing something that is not a node. Already the right thing to hang
    /// content off once there is content to hang.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
        subject: Option<NodeId>,
        ws: &WorkspaceDoc,
    ) {
        let p = theme::p();
        scene::fill(
            scene,
            root,
            p.pane_bg,
            &scene::rect(area.x, area.y, area.w, area.h),
        );
        scene::fill(
            scene,
            root,
            p.panel_border,
            &scene::rect(area.x, area.y, 1.0, area.h),
        );

        for (s, r) in Self::section_rects(area) {
            if r.w <= 0.0 {
                continue;
            }
            let selected = s == self.section;
            if selected || self.hover == Some(s) {
                let fill = if selected {
                    p.row_selected
                } else {
                    p.hover_tint
                };
                scene::fill(
                    scene,
                    root,
                    fill,
                    &scene::rrect(r.x + 1.0, r.y, r.w - 2.0, r.h, 5.0),
                );
            }
            let colour = if selected { p.text } else { p.text_dim };
            draw_icon(scene, root, engine, s.icon(), r, colour);
        }

        // What the column is *about*, so it is obvious the subject follows the
        // active tab rather than being global.
        let body_y = area.y + theme::NAV_PAD + Self::tabs_extra();
        let inner_w = (area.w - theme::NAV_PAD * 4.0).max(1.0);
        let subject_line = match subject {
            Some(id) => {
                let name = ws
                    .read(id)
                    .ok()
                    .and_then(|n| n.content.as_text().map(str::to_owned))
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "Untitled".into());
                format!("{} · {}", self.section_label(self.section), name)
            }
            None => self.section_label(self.section).to_string(),
        };
        let head = engine.simple_layout_wrapped(&subject_line, theme::role::meta(), inner_w);
        scene::draw_layout_first_line(
            scene,
            root,
            &head,
            area.x + theme::NAV_PAD * 2.0,
            body_y,
            Some(p.row_hint),
        );

        if self.section == Section::History {
            let layout = self.history_layout(area);
            self.clamp_history_scroll(&layout);
            for (rect, text, enabled) in [
                (layout.save, "Save version", true),
                (
                    layout.compare,
                    if self.selected_history.len() == 1 {
                        "Compare with current"
                    } else {
                        "Compare selected"
                    },
                    matches!(self.selected_history.len(), 1 | 2),
                ),
            ] {
                scene::fill(
                    scene,
                    root,
                    if enabled {
                        p.row_selected
                    } else {
                        p.hover_tint
                    },
                    &scene::rrect(rect.x, rect.y, rect.w, rect.h, 5.0),
                );
                let line = engine.simple_layout_wrapped(text, theme::role::row(), rect.w - 12.0);
                scene::draw_layout_first_line(
                    scene,
                    root,
                    &line,
                    rect.x + 6.0,
                    rect.y + 6.0,
                    Some(if enabled { p.text } else { p.text_dim }),
                );
            }
            if let Some(status) = &self.history_status {
                let line = engine.simple_layout_wrapped(status, theme::role::meta(), inner_w);
                scene::draw_layout_first_line(
                    scene,
                    root,
                    &line,
                    area.x + theme::NAV_PAD * 2.0,
                    layout.status_y,
                    Some(p.text_dim),
                );
            }
            // The rows live in a clipped viewport so a scrolled list cannot
            // bleed into the fixed chrome above it.
            let v = &layout.viewport;
            scene.push_clip_layer(Fill::NonZero, root, &scene::rect(v.x, v.y, v.w, v.h));
            for (item, rect) in self.history.iter().zip(layout.rows.iter().copied()) {
                if rect.y + rect.h < v.y || rect.y > v.y + v.h {
                    continue;
                }
                if self.selected_history.contains(&item.key) {
                    scene::fill(
                        scene,
                        root,
                        p.row_selected,
                        &scene::rrect(rect.x, rect.y, rect.w, rect.h, 4.0),
                    );
                }
                let line =
                    engine.simple_layout_wrapped(&item.label, theme::role::meta(), rect.w - 10.0);
                scene::draw_layout_first_line(
                    scene,
                    root,
                    &line,
                    rect.x + 5.0,
                    rect.y + 5.0,
                    Some(p.text),
                );
            }
            scene.pop_layer();
            if let Some(geom) = layout.scrollbar(self.history_scroll) {
                scrollbar::draw(scene, root, geom, self.history_drag.is_some());
            }
            if self.history.is_empty() {
                let empty = engine.simple_layout_wrapped(
                    self.section.empty_line(),
                    theme::role::meta(),
                    inner_w,
                );
                scene::draw_layout(
                    scene,
                    root,
                    &empty,
                    area.x + theme::NAV_PAD * 2.0,
                    layout.viewport.y,
                    Some(p.text_dim),
                );
            }
        } else if self.section == Section::CloudSync {
            // **A reading and a fact, stacked.** The label is dim and small,
            // the fact is the text colour — the same weighting the task row
            // gives a title against its due, turned through ninety degrees
            // because a Cloud fact is longer than a date and must wrap rather
            // than elide.
            let layout = self.cloud_layout(area);
            for (line, rect) in self.cloud.iter().zip(layout.lines.iter().copied()) {
                let label = engine.simple_layout(&line.label, theme::role::hint());
                scene::draw_layout_first_line(
                    scene,
                    root,
                    &label,
                    rect.x,
                    rect.y,
                    Some(p.text_dim),
                );
                let value = engine.simple_layout_wrapped(&line.value, theme::role::meta(), rect.w);
                scene::draw_layout_first_line(
                    scene,
                    root,
                    &value,
                    rect.x,
                    rect.y + 13.0,
                    Some(p.text),
                );
            }
            // The footnote carries two different things and never both: the
            // images limitation while there is one, and the acknowledgement
            // that *Copy details* did something once it has. When there are no
            // facts at all it is the empty line, localized — and the hardcoded
            // one below it stands only until the container has spoken.
            let note = self
                .cloud_note
                .as_deref()
                .unwrap_or(if self.cloud.is_empty() {
                    self.section.empty_line()
                } else {
                    ""
                });
            if !note.is_empty() {
                let line = engine.simple_layout_wrapped(note, theme::role::hint(), inner_w);
                scene::draw_layout(
                    scene,
                    root,
                    &line,
                    area.x + theme::NAV_PAD * 2.0,
                    layout.note_y,
                    Some(p.text_dim),
                );
            }
            // **Nothing to copy, no button.** An action that puts an empty
            // string on the clipboard is worse than an absent one: it looks
            // like it worked.
            if self.has_cloud_details() {
                // **The one action this section has.** The section is about
                // one document, so nothing here acts on a whole Workspace —
                // those controls belong where the Workspace is the subject.
                draw_cloud_chip(
                    scene,
                    root,
                    engine,
                    layout.copy,
                    &self.cloud_copy_label,
                    self.cloud_copy_hovered,
                    p,
                );
            }
        } else if self.section == Section::Tasks && !self.tasks.is_empty() {
            // **One row per task, in document order** — the section's own
            // version of the row idiom History uses: a hover fill on the same
            // chip, the label at the left, and the one piece of metadata that
            // matters at the right. There is no control on the row, because
            // this section is read-only for this release: a press reveals the
            // task where it lives, and every way of *changing* one is already
            // in the document under the caret or on the Tasks tab.
            for (i, (row, rect)) in self
                .tasks
                .iter()
                .zip(task_rects(area, self.tasks.len()))
                .enumerate()
            {
                if self.hovered_task == Some(i) {
                    scene::fill(
                        scene,
                        root,
                        p.hover_tint,
                        &scene::rrect(rect.x, rect.y, rect.w, rect.h, 4.0),
                    );
                }
                // The due first, from the right, so the title's cell is what is
                // left — a long title elides rather than running under a date.
                let mut right = rect.x + rect.w;
                if let Some(due) = &row.due {
                    let label = engine.simple_layout(due, theme::role::hint());
                    right -= label.width() + TASK_PAD;
                    scene::draw_layout_first_line(
                        scene,
                        root,
                        &label,
                        right,
                        scene::centre_y_in(&label, rect),
                        // The one warm reading in this column, and the same
                        // accent token every overdue reading in the app asks
                        // for by name.
                        Some(if row.overdue {
                            overdue_ink()
                        } else {
                            p.text_dim
                        }),
                    );
                }
                let ink = if row.completed { p.placeholder } else { p.text };
                let title = engine.simple_layout_wrapped(
                    &row.title,
                    theme::role::meta(),
                    (right - rect.x - TASK_PAD * 2.0).max(1.0),
                );
                scene::draw_layout_first_line(
                    scene,
                    root,
                    &title,
                    rect.x + TASK_PAD,
                    scene::centre_y_in(&title, rect),
                    Some(ink),
                );
                // Completed is dimmed **and struck**, the reading the Tasks tab
                // gives it — an archive rather than a deletion.
                if row.completed {
                    let y = (rect.y + rect.h * 0.5).round();
                    scene::fill(
                        scene,
                        root,
                        ink,
                        &scene::rect(rect.x + TASK_PAD, y, title.width(), 1.0),
                    );
                }
            }
        } else {
            let empty = engine.simple_layout_wrapped(
                self.section.empty_line(),
                theme::role::row(),
                inner_w,
            );
            scene::draw_layout(
                scene,
                root,
                &empty,
                area.x + theme::NAV_PAD * 2.0,
                body_y + 26.0,
                Some(p.text_dim),
            );
        }
    }
}

/// Air either side of a task row's text.
const TASK_PAD: f32 = 5.0;
/// One task row's band, and the step between two.
const TASK_H: f32 = 26.0;
const TASK_STEP: f32 = 28.0;

/// One chip in the Cloud section's footer.
///
/// **One function, two buttons**, for the reason `task_rects` gives one below:
/// two hand-written copies of a chip is two places for a hover fill and a
/// label inset to drift apart.
#[allow(clippy::too_many_arguments)]
fn draw_cloud_chip(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    rect: Rect,
    label: &str,
    hovered: bool,
    p: &theme::Palette,
) {
    scene::fill(
        scene,
        root,
        if hovered {
            p.hover_tint
        } else {
            p.row_selected
        },
        &scene::rrect(rect.x, rect.y, rect.w, rect.h, 5.0),
    );
    let layout = engine.simple_layout_wrapped(label, theme::role::row(), rect.w - 12.0);
    scene::draw_layout_first_line(
        scene,
        root,
        &layout,
        rect.x + 6.0,
        rect.y + 6.0,
        Some(p.text),
    );
}

/// Where the Tasks section's rows are.
///
/// **One function, three callers** — drawing, the hover and the press — which is
/// the first rule for chrome: a row cannot be pressable somewhere other than
/// where it is drawn. It starts where History's own list does, from the same
/// `tabs_extra` base, so the two sections do not sit at different heights under
/// the same heading.
fn task_rects(area: Rect, count: usize) -> Vec<Rect> {
    let top = area.y + theme::NAV_PAD + ContextView::tabs_extra() + 26.0;
    let x = area.x + theme::NAV_PAD * 2.0;
    let w = (area.w - theme::NAV_PAD * 4.0).max(1.0);
    (0..count)
        .map(|i| Rect::new(x, top + i as f32 * TASK_STEP, w, TASK_H))
        .collect()
}

/// The warm ink an overdue reading wears — `accent_tone`, by name, never a new
/// `Palette` field. The rail's split count and the Tasks tab's late due chip ask
/// for the same one.
fn overdue_ink() -> vello::peniko::Color {
    theme::accent_tone("red")
        .map(|tone| tone.ink)
        .unwrap_or(theme::p().text)
}

#[cfg(test)]
mod tests;
