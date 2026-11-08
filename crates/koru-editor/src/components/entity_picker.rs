// SPDX-License-Identifier: MPL-2.0

//! The `@` entity picker.
//!
//! The query remains in the document. This component owns only presentation,
//! selection and workspace scope; the container answers its search requests
//! through Koru's shared store-backed search service.

use koru_core::{DocumentId, NodeId, ProfileId, WorkspaceId};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::Outcome;
use super::text_field::TextField;
use crate::trigger::{DocumentTarget, PillSpec, Trigger};

pub const HEADER_H: f32 = 38.0;
pub const MAX_ROWS: usize = 7;
/// The height of the own-query band a move-mode picker adds above the header.
pub const FIELD_H: f32 = 36.0;

/// The band's rect, panel-local. Mirrors `emoji_picker::search_rect`'s shape:
/// inset by the panel padding, a little shorter than its band.
pub fn field_rect(panel_w: f32) -> Rect {
    let pad = koru_draw::theme::PANEL_PAD;
    Rect::new(pad, pad, panel_w - pad * 2.0, FIELD_H - 8.0)
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorkspaceChoice {
    pub profile: ProfileId,
    pub profile_name: String,
    pub id: WorkspaceId,
    pub name: String,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DocumentChoice {
    pub target: DocumentTarget,
    pub title: String,
    pub breadcrumb: String,
    /// True when the hit is a workspace file rather than a page.
    pub is_file: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EntitySearchRequest {
    pub query: String,
    pub workspaces: Vec<WorkspaceId>,
}

/// Which list the picker is showing.
///
/// An enum rather than a set of booleans because the row arithmetic — how
/// many rows there are, which one a click landed on, what scrolls, what a
/// selection commits — branches on it in eight places. Two independent flags
/// would make four states of which half are meaningless.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Stage {
    /// Matching documents, plus the pinned "new page" row.
    Documents,
    /// Headings of one document, entered by typing `#`.
    Headings(Headings),
}

/// The heading stage's own state.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Headings {
    /// The document whose headings these are, snapshotted when `#` was typed.
    ///
    /// **Snapshotted, never re-read.** A document search answer can still be in
    /// flight when the stage opens, and letting a late answer re-point `sel`
    /// would move the link somewhere the person never chose.
    pub anchor: DocumentChoice,
    /// Where the anchor sat in `hits`, so backspacing over the `#` puts the
    /// highlight back on the row it came from.
    pub anchor_row: usize,
    /// Every heading the document has, in reading order.
    pub all: Vec<HeadingChoice>,
    /// Those matching the text after the `#`.
    pub shown: Vec<HeadingChoice>,
    /// Whether the outline has arrived.
    pub loading: bool,
    /// Whether the read model has this document at all. An empty `all` with
    /// this false means "still indexing", which is a different sentence.
    pub projected: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HeadingChoice {
    pub node: NodeId,
    pub level: u8,
    pub text: String,
}

pub struct EntityPicker {
    pub trigger: Trigger,
    pub query: String,
    pub hits: Vec<DocumentChoice>,
    pub workspaces: Vec<WorkspaceChoice>,
    pub selected_workspaces: Vec<WorkspaceId>,
    pub current_workspace: Option<WorkspaceId>,
    /// Ctrl+Space widens the current-workspace search for this picker only.
    pub widened_scope: bool,
    pub sel: usize,
    pub first: usize,
    pub stage: Stage,
    /// The part of `query` before the first `#` — what the document search is
    /// asked for. Frozen while the heading stage is open, so typing a heading
    /// filter never re-runs the document search.
    pub document_query: String,
    /// The current document, offered as the anchor for a bare `@#`. A page
    /// links to a heading of its own more often than to anything else.
    pub current_document: Option<DocumentChoice>,
    /// An outline is owed for this document.
    pub(super) headings_dirty: Option<DocumentId>,
    pub loading: bool,
    pub unavailable: bool,
    pub(super) dirty: bool,
    pub(super) scrollbar_drag: Option<f32>,
    /// This picker chooses where a selection **moves**, not what a pill
    /// mentions: a commit is routed to the move machinery instead of
    /// `commit_pill`, and the query lives in [`own_query`](Self::own_query).
    pub move_mode: bool,
    /// The panel's own query field. `Some` only in move mode — the mention
    /// picker's query is the `@…` text in the document, but a move opens over
    /// a selection, so there is no place in the document to type into.
    pub own_query: Option<TextField>,
    /// The commit that just happened was Ctrl+Enter or Shift+Enter: perform
    /// the move but stay here — no tab for the new page, no navigation. Read
    /// by the move routing; meaningless for the mention picker.
    pub stay_on_commit: bool,
    /// The commit was Shift+Enter: the created document becomes a **child**
    /// of the one the selection left, not its sibling.
    pub child_on_commit: bool,
}

/// Manual because a [`TextField`] is not `Clone` — it owns a parley layout.
/// The one cloner is `Overlay::refresh`, which only ever runs for the
/// document-driven mention picker; a move-mode picker (`Anchor::Insert` or
/// `Anchor::Selection`) never reaches it, so dropping the field here loses
/// nothing that can exist on the path that clones.
impl Clone for EntityPicker {
    fn clone(&self) -> Self {
        debug_assert!(
            self.own_query.is_none(),
            "a move-mode picker is never document-driven, so nothing should clone it"
        );
        Self {
            trigger: self.trigger.clone(),
            query: self.query.clone(),
            hits: self.hits.clone(),
            workspaces: self.workspaces.clone(),
            selected_workspaces: self.selected_workspaces.clone(),
            current_workspace: self.current_workspace,
            widened_scope: self.widened_scope,
            sel: self.sel,
            first: self.first,
            stage: self.stage.clone(),
            document_query: self.document_query.clone(),
            current_document: self.current_document.clone(),
            headings_dirty: self.headings_dirty,
            loading: self.loading,
            unavailable: self.unavailable,
            dirty: self.dirty,
            scrollbar_drag: self.scrollbar_drag,
            move_mode: self.move_mode,
            own_query: None,
            stay_on_commit: self.stay_on_commit,
            child_on_commit: self.child_on_commit,
        }
    }
}

impl EntityPicker {
    pub fn new(trigger: Trigger) -> Self {
        Self {
            trigger,
            query: String::new(),
            hits: Vec::new(),
            workspaces: Vec::new(),
            selected_workspaces: Vec::new(),
            current_workspace: None,
            widened_scope: false,
            sel: 0,
            first: 0,
            stage: Stage::Documents,
            document_query: String::new(),
            current_document: None,
            headings_dirty: None,
            loading: true,
            unavailable: false,
            dirty: true,
            scrollbar_drag: None,
            move_mode: false,
            own_query: None,
            stay_on_commit: false,
            child_on_commit: false,
        }
    }

    /// The picker in move mode, with its own query field focused from the
    /// first keystroke. Every stage is kept: the workspace scope is what makes
    /// a cross-workspace move reachable, and `#` still drills into headings —
    /// there it chooses where *under* a document the selection lands.
    ///
    /// `prefill` seeds the field — the text the selection started as, so an
    /// existing document with that title surfaces as a hit at once and "Add
    /// new page" is already titled. The caret lands at its end (what
    /// `set_text` does); typing appends, backspacing edits it away.
    pub fn for_move(engine: &mut TextEngine, trigger: Trigger, prefill: &str) -> Self {
        let mut picker = Self::new(trigger.clone());
        picker.move_mode = true;
        let mut field = TextField::new(engine, koru_draw::theme::role::ui());
        if !prefill.is_empty() {
            field.set_text(engine, prefill);
        }
        picker.own_query = Some(field);
        if !prefill.is_empty() {
            // Through the same door typing uses, so the stage derives and the
            // search request is owed exactly as if it had been typed.
            picker.refilter(trigger, prefill);
        }
        picker
    }

    /// The extra height the own-query band adds, zero for the mention picker.
    pub fn field_band(&self) -> f32 {
        if self.own_query.is_some() {
            FIELD_H
        } else {
            0.0
        }
    }

    pub fn click_field(&mut self, engine: &mut TextEngine, x: f32, panel_w: f32) {
        let rect = field_rect(panel_w);
        if let Some(field) = &mut self.own_query {
            field.click(engine, x - rect.x + field.scroll());
        }
    }

    /// One key, own-query flavour: list navigation stays the list's, and
    /// everything else types into the field and re-filters from it.
    pub fn key_full(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Outcome {
        if koru_keys::registry::REGISTRY
            .iter()
            .find(|shortcut| shortcut.id == koru_keys::ShortcutId::WidenEntityScope)
            .is_some_and(|shortcut| shortcut.chord.matches(key, text, mods))
        {
            self.widen_scope();
            return Outcome::Consumed;
        }
        let named = match key {
            Key::Named(n) => Some(*n),
            _ => None,
        };
        if self.own_query.is_none()
            || matches!(
                named,
                Some(NamedKey::ArrowDown | NamedKey::ArrowUp | NamedKey::Enter | NamedKey::Tab)
            )
        {
            // Modified commits, recorded here where the modifiers are still
            // in hand and read by the move routing off the outcome this
            // produces. Ctrl+Enter means commit *without following*;
            // Shift+Enter additionally makes the created document a **child**
            // of the one the selection left, and stays too — a sub-page you
            // meant to keep working past.
            if matches!(named, Some(NamedKey::Enter | NamedKey::Tab)) {
                self.child_on_commit = mods.shift_key();
                self.stay_on_commit = mods.control_key() || mods.super_key() || mods.shift_key();
            }
            return self.key(named);
        }
        let Some(field) = &mut self.own_query else {
            return self.key(named);
        };
        field.key(engine, key, text, mods);
        let query = field.raw_text().to_owned();
        let trigger = self.trigger.clone();
        self.refilter(trigger, &query);
        Outcome::Consumed
    }

    pub fn row_count(&self) -> usize {
        self.total_count().max(1).min(MAX_ROWS)
    }

    fn total_count(&self) -> usize {
        match &self.stage {
            // Always at least one row: with no headings it is the row that
            // says so, and choosing it commits the document-level pill. That
            // is what keeps the empty case out of the key handling entirely.
            Stage::Headings(headings) => headings.shown.len().max(1),
            // A create row remains available even with no matches.
            Stage::Documents => self.hits.len() + 1,
        }
    }

    /// The headings on offer, if that is the visible list.
    pub fn headings(&self) -> Option<&Headings> {
        match &self.stage {
            Stage::Headings(headings) => Some(headings),
            _ => None,
        }
    }

    pub fn height(&self) -> f32 {
        koru_draw::theme::PANEL_PAD * 2.0
            + self.field_band()
            + HEADER_H
            + self.row_count() as f32 * koru_draw::theme::ROW_H
    }

    pub fn set_context(
        &mut self,
        current: WorkspaceId,
        mut workspaces: Vec<WorkspaceChoice>,
    ) -> bool {
        // A move never crosses a profile — the shell refuses it at commit, so
        // offering another profile's documents is offering a dialog. The
        // mention picker keeps the whole catalog: a *link* may point anywhere.
        if self.move_mode
            && let Some(profile) = workspaces
                .iter()
                .find(|workspace| workspace.id == current)
                .map(|workspace| workspace.profile)
        {
            workspaces.retain(|workspace| workspace.profile == profile);
        }
        let mut changed = self.current_workspace != Some(current) || self.workspaces != workspaces;
        let selected_before = self.selected_workspaces.clone();
        self.current_workspace = Some(current);
        self.workspaces = workspaces;
        if self.widened_scope && !self.workspaces.is_empty() {
            self.selected_workspaces = self
                .workspaces
                .iter()
                .map(|workspace| workspace.id)
                .collect();
        } else {
            self.selected_workspaces = vec![current];
        }
        if self.selected_workspaces != selected_before {
            self.loading = true;
            self.dirty = true;
            changed = true;
        }
        changed
    }

    pub fn widen_scope(&mut self) -> bool {
        if self.widened_scope || self.headings().is_some() {
            return false;
        }
        self.widened_scope = true;
        if let Some(current) = self.current_workspace {
            let workspaces = self.workspaces.clone();
            self.set_context(current, workspaces);
        }
        true
    }

    /// Take the query as it now reads in the document, and follow it.
    ///
    /// **The stage is derived here and nowhere else.** The query is not a
    /// buffer — it is recomputed from the row's text every frame — so a stage
    /// set by a `#` *keystroke* would outlive the `#` itself: backspace over it
    /// and the picker would still be listing headings for a query that no
    /// longer asks for any.
    pub fn refilter(&mut self, trigger: Trigger, query: &str) {
        self.trigger = trigger;
        if self.query == query {
            return;
        }
        self.query.clear();
        self.query.push_str(query);
        match query.split_once('#') {
            // `split_once`, so a heading whose own text contains a `#` is still
            // reachable: everything after the first one is the filter.
            Some((documents, headings)) => self.enter_headings(documents, headings),
            None => self.leave_headings(query),
        }
    }

    /// Switch to — or filter within — the headings of the highlighted document.
    fn enter_headings(&mut self, documents: &str, filter: &str) {
        if let Stage::Headings(headings) = &mut self.stage
            && self.document_query == documents
        {
            // Still the same document: this is a keystroke in the filter, and
            // the outline is already in hand. No request, no round trip.
            headings.shown = Self::matching(&headings.all, filter);
            self.sel = 0;
            self.first = 0;
            return;
        }
        // A bare `@#` means this page's own headings; otherwise it is whatever
        // row is highlighted. The pinned "new page" row is not a document, so
        // there is nothing to look inside and the `#` stays ordinary text.
        let anchor = if documents.is_empty() {
            self.current_document.clone()
        } else {
            self.hits.get(self.sel).cloned()
        };
        let Some(anchor) = anchor else {
            self.stage = Stage::Documents;
            self.document_query = documents.to_owned();
            self.sel = 0;
            self.first = 0;
            self.loading = true;
            self.dirty = true;
            return;
        };
        self.headings_dirty = Some(anchor.target.document);
        self.document_query = documents.to_owned();
        self.stage = Stage::Headings(Headings {
            anchor,
            anchor_row: self.sel,
            all: Vec::new(),
            shown: Vec::new(),
            loading: true,
            projected: false,
        });
        self.sel = 0;
        self.first = 0;
    }

    /// Back to the document list — the `#` is gone.
    fn leave_headings(&mut self, query: &str) {
        let was = std::mem::replace(&mut self.stage, Stage::Documents);
        self.document_query = query.to_owned();
        if let Stage::Headings(headings) = was {
            // Put the highlight back where the person left it, by identity
            // rather than by index: the hits may have been replaced while the
            // heading list was up.
            self.sel = self
                .hits
                .iter()
                .position(|hit| hit.target.document == headings.anchor.target.document)
                .unwrap_or_else(|| headings.anchor_row.min(self.hits.len()));
            self.first = 0;
            self.ensure_selected_visible();
            return;
        }
        self.stage = Stage::Documents;
        self.sel = 0;
        self.first = 0;
        self.loading = true;
        self.dirty = true;
    }

    fn matching(all: &[HeadingChoice], filter: &str) -> Vec<HeadingChoice> {
        let filter = filter.trim().to_lowercase();
        if filter.is_empty() {
            return all.to_vec();
        }
        all.iter()
            .filter(|heading| heading.text.to_lowercase().contains(&filter))
            .cloned()
            .collect()
    }

    /// The document whose outline is owed, if any.
    pub fn take_heading_request(&mut self) -> Option<DocumentId> {
        self.headings_dirty.take()
    }

    /// File an outline. Ignored unless it is the one the stage is waiting for:
    /// an answer for a document the person has moved off must not appear.
    pub fn set_headings(
        &mut self,
        document: DocumentId,
        headings: Vec<HeadingChoice>,
        projected: bool,
    ) -> bool {
        let filter = self
            .query
            .split_once('#')
            .map(|(_, tail)| tail.to_owned())
            .unwrap_or_default();
        let Stage::Headings(stage) = &mut self.stage else {
            return false;
        };
        if stage.anchor.target.document != document {
            return false;
        }
        stage.shown = Self::matching(&headings, &filter);
        stage.all = headings;
        stage.loading = false;
        stage.projected = projected;
        self.sel = 0;
        self.first = 0;
        true
    }

    pub fn take_request(&mut self) -> Option<EntitySearchRequest> {
        if !std::mem::take(&mut self.dirty) || self.selected_workspaces.is_empty() {
            return None;
        }
        Some(EntitySearchRequest {
            // The document half, never the whole query: once a `#` is typed,
            // searching for "Design#Roll" would answer nothing and clear the
            // hits the anchor was taken from.
            query: self.document_query.clone(),
            workspaces: self.selected_workspaces.clone(),
        })
    }

    /// File the container-fed recents for an empty move-mode query.
    ///
    /// **Compares first.** This is called from the per-gesture sync, and a
    /// feed that answered "changed" for an identical list would invalidate,
    /// redraw, sync, and feed again — a redraw loop pinned at full speed,
    /// which is what "@ blocks the UI" turned out to be.
    pub fn set_recent_results(&mut self, hits: Vec<DocumentChoice>) -> bool {
        if !self.document_query.trim().is_empty() {
            return false;
        }
        if self.hits == hits && !self.loading {
            return false;
        }
        self.set_results(hits, false);
        true
    }

    pub fn set_results(&mut self, hits: Vec<DocumentChoice>, unavailable: bool) {
        self.hits = hits;
        self.unavailable = unavailable;
        self.loading = false;
        self.sel = self.sel.min(self.total_count().saturating_sub(1));
        self.ensure_selected_visible();
    }

    pub fn key(&mut self, named: Option<NamedKey>) -> Outcome {
        let len = self.total_count();
        match named {
            Some(NamedKey::ArrowDown) if len > 0 => {
                self.sel = (self.sel + 1) % len;
                self.ensure_selected_visible();
                Outcome::Consumed
            }
            Some(NamedKey::ArrowUp) if len > 0 => {
                self.sel = (self.sel + len - 1) % len;
                self.ensure_selected_visible();
                Outcome::Consumed
            }
            Some(NamedKey::Enter) | Some(NamedKey::Tab) => self.choose(self.sel),
            _ => Outcome::Ignored,
        }
    }

    pub fn click(&mut self, _local_x: f32, local_y: f32, _panel_w: f32) -> Outcome {
        // The own-query band sits above everything; clicks in it are routed to
        // `click_field` by the overlay before this is asked, so the geometry
        // below only has to look past it.
        let local_y = local_y - self.field_band();
        let pad = koru_draw::theme::PANEL_PAD;
        if local_y >= pad && local_y < pad + HEADER_H {
            return Outcome::Consumed;
        }
        let row = ((local_y - pad - HEADER_H) / koru_draw::theme::ROW_H).floor();
        if row < 0.0 {
            return Outcome::Consumed;
        }
        let Some(item) = self.visible_item(row as usize) else {
            return Outcome::Consumed;
        };
        self.choose(item)
    }

    fn choose(&mut self, i: usize) -> Outcome {
        if let Stage::Headings(headings) = &self.stage {
            let Some(heading) = headings.shown.get(i) else {
                // The "no headings" row. An ordinary document pill is the
                // honest outcome of asking a document with none: nothing is
                // lost by having tried.
                return Outcome::Commit(PillSpec::document(
                    headings.anchor.title.clone(),
                    headings.anchor.target,
                ));
            };
            return Outcome::Commit(PillSpec::document(
                format!("{}#{}", headings.anchor.title, heading.text),
                DocumentTarget {
                    document: headings.anchor.target.document,
                    node: Some(heading.node),
                },
            ));
        }
        if let Some(hit) = self.hits.get(i) {
            return Outcome::Commit(if hit.is_file {
                PillSpec::file(hit.title.clone(), hit.target)
            } else {
                PillSpec::document(hit.title.clone(), hit.target)
            });
        }
        if i == self.hits.len() {
            return Outcome::NewPage(self.query.trim().to_owned());
        }
        Outcome::Consumed
    }

    pub fn scope_label(&self) -> String {
        if self.widened_scope {
            "All workspaces".into()
        } else {
            self.current_workspace
                .and_then(|only| {
                    self.workspaces
                        .iter()
                        .find(|w| w.id == only)
                        .map(|w| w.name.clone())
                })
                .unwrap_or_else(|| "This workspace".into())
        }
    }

    pub fn scroll_rows(&mut self, delta: isize) {
        self.first = (self.first as isize + delta).clamp(0, self.max_first() as isize) as usize;
    }

    fn scroll_len(&self) -> usize {
        match &self.stage {
            Stage::Headings(headings) => headings.shown.len(),
            Stage::Documents => self.hits.len(),
        }
    }

    fn visible_scroll_rows(&self) -> usize {
        let capacity = match self.stage {
            // Neither list pins a row beneath it.
            Stage::Headings(_) => MAX_ROWS,
            // Add new page is pinned below the scrolling result rows.
            Stage::Documents => MAX_ROWS.saturating_sub(1),
        };
        self.scroll_len().min(capacity)
    }

    fn max_first(&self) -> usize {
        self.scroll_len().saturating_sub(self.visible_scroll_rows())
    }

    pub fn scrolls(&self) -> bool {
        self.max_first() > 0
    }

    pub fn scrollbar_track(&self, panel_w: f32) -> Rect {
        let (x, _) = super::command_menu::scrollbar_column(panel_w);
        Rect::new(
            x + 4.0,
            koru_draw::theme::PANEL_PAD + self.field_band() + HEADER_H,
            super::command_menu::SCROLLBAR_W,
            self.visible_scroll_rows() as f32 * koru_draw::theme::ROW_H,
        )
    }

    pub fn scrollbar_thumb(&self, panel_w: f32) -> Rect {
        let track = self.scrollbar_track(panel_w);
        let shown = self.visible_scroll_rows() as f32;
        let h = (track.h * shown / self.scroll_len().max(1) as f32).clamp(20.0, track.h);
        let travel = (track.h - h).max(0.0);
        let t = self.first as f32 / self.max_first().max(1) as f32;
        Rect::new(track.x, track.y + travel * t, track.w, h)
    }

    pub fn press_scrollbar(&mut self, panel_w: f32, local_y: f32) {
        if !self.scrolls() {
            return;
        }
        let thumb = self.scrollbar_thumb(panel_w);
        if local_y >= thumb.y && local_y <= thumb.y + thumb.h {
            self.scrollbar_drag = Some(local_y - thumb.y);
            return;
        }
        self.scrollbar_drag = Some(thumb.h * 0.5);
        self.drag_scrollbar(panel_w, local_y);
    }

    pub fn is_dragging_scrollbar(&self) -> bool {
        self.scrollbar_drag.is_some()
    }

    pub fn drag_scrollbar(&mut self, panel_w: f32, local_y: f32) {
        let Some(grab) = self.scrollbar_drag else {
            return;
        };
        let track = self.scrollbar_track(panel_w);
        let thumb = self.scrollbar_thumb(panel_w);
        let travel = (track.h - thumb.h).max(0.0);
        let top = (local_y - track.y - grab).clamp(0.0, travel);
        self.first = if travel > 0.0 {
            (top / travel * self.max_first() as f32).round() as usize
        } else {
            0
        };
    }

    pub fn stop_scrollbar_drag(&mut self) -> bool {
        self.scrollbar_drag.take().is_some()
    }

    /// Map a physical panel row to the stable selection index. The add action
    /// is pinned after the visible hits, even while the hits themselves scroll.
    pub(super) fn visible_item(&self, row: usize) -> Option<usize> {
        if let Stage::Headings(headings) = &self.stage {
            let visible = headings
                .shown
                .len()
                .saturating_sub(self.first)
                .min(MAX_ROWS)
                // The empty state is one selectable row.
                .max(usize::from(headings.shown.is_empty()));
            return (row < visible).then_some(self.first + row);
        }
        let visible_hits = self.hits.len().saturating_sub(self.first).min(MAX_ROWS - 1);
        if row < visible_hits {
            Some(self.first + row)
        } else if row == visible_hits {
            Some(self.hits.len())
        } else {
            None
        }
    }

    pub(super) fn contains_item(&self, item: usize) -> bool {
        item < self.total_count()
    }

    fn ensure_selected_visible(&mut self) {
        let visible = match self.stage {
            Stage::Headings(_) => MAX_ROWS,
            Stage::Documents => MAX_ROWS - 1,
        };
        if matches!(self.stage, Stage::Documents) && self.sel == self.hits.len() {
            self.first = self.hits.len().saturating_sub(visible);
            return;
        }
        if self.sel < self.first {
            self.first = self.sel;
        } else if self.sel >= self.first + visible {
            self.first = self.sel + 1 - visible;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_core::DocumentId;

    fn target(_workspace: WorkspaceId) -> DocumentTarget {
        let document = DocumentId::new();
        DocumentTarget {
            document,
            node: None,
        }
    }

    #[test]
    fn duplicate_titles_remain_distinct_document_choices() {
        let workspace = WorkspaceId::new();
        let mut picker = EntityPicker::new(Trigger::at_start());
        picker.set_results(
            vec![
                DocumentChoice {
                    target: target(workspace),
                    title: "Hello".into(),
                    breadcrumb: "Work / One / Hello".into(),
                    is_file: false,
                },
                DocumentChoice {
                    target: target(workspace),
                    title: "Hello".into(),
                    breadcrumb: "Work / Two / Hello".into(),
                    is_file: false,
                },
            ],
            false,
        );
        let Outcome::Commit(first) = picker.choose(0) else {
            panic!("first document should commit")
        };
        let Outcome::Commit(second) = picker.choose(1) else {
            panic!("second document should commit")
        };
        assert_eq!(first.label, second.label);
        assert_ne!(first.document, second.document);
    }

    /// A move never crosses a profile, so the move-mode picker's scope
    /// holds only the current
    /// profile's workspaces. The mention picker keeps the whole catalog: a
    /// link may point anywhere.
    #[test]
    fn move_mode_scopes_to_the_current_profile() {
        let current = WorkspaceId::new();
        let ours = WorkspaceChoice {
            profile: ProfileId::new(),
            profile_name: "Work".into(),
            id: current,
            name: "Current".into(),
        };
        let foreign = WorkspaceChoice {
            profile: ProfileId::new(),
            profile_name: "Personal".into(),
            id: WorkspaceId::new(),
            name: "Elsewhere".into(),
        };

        let mut engine = koru_draw::text::TextEngine::new();
        let mut picker = EntityPicker::for_move(&mut engine, Trigger::at_start(), "");
        picker.set_context(current, vec![ours.clone(), foreign.clone()]);
        assert_eq!(picker.workspaces, vec![ours.clone()]);
        assert_eq!(picker.selected_workspaces, vec![current]);

        let mut mention = EntityPicker::new(Trigger::at_start());
        mention.set_context(current, vec![ours, foreign.clone()]);
        assert!(
            mention.workspaces.iter().any(|w| w.id == foreign.id),
            "the mention picker still offers every profile"
        );
    }

    #[test]
    fn current_scope_widens_without_changing_query() {
        let current = WorkspaceId::new();
        let other = WorkspaceId::new();
        let profile = ProfileId::new();
        let mut picker = EntityPicker::new(Trigger::at_start());
        picker.set_context(
            current,
            vec![
                WorkspaceChoice {
                    profile,
                    profile_name: "Profile".into(),
                    id: current,
                    name: "Current".into(),
                },
                WorkspaceChoice {
                    profile,
                    profile_name: "Profile".into(),
                    id: other,
                    name: "Other".into(),
                },
            ],
        );
        assert_eq!(picker.take_request().unwrap().workspaces, [current]);
        picker.refilter(Trigger::at_start(), "plan");
        picker.take_request();
        assert!(picker.widen_scope());
        assert_eq!(picker.query, "plan");
        assert_eq!(picker.take_request().unwrap().workspaces, [current, other]);
        assert!(!picker.widen_scope());
    }

    #[test]
    fn ctrl_space_widens_only_an_open_picker() {
        let current = WorkspaceId::new();
        let other = WorkspaceId::new();
        let profile = ProfileId::new();
        let mut picker = EntityPicker::new(Trigger::at_start());
        picker.set_context(
            current,
            vec![
                WorkspaceChoice {
                    profile,
                    profile_name: String::new(),
                    id: current,
                    name: "Current".into(),
                },
                WorkspaceChoice {
                    profile,
                    profile_name: String::new(),
                    id: other,
                    name: "Other".into(),
                },
            ],
        );
        let mut engine = TextEngine::new();
        assert!(matches!(
            picker.key_full(
                &mut engine,
                &Key::Named(NamedKey::Space),
                None,
                ModifiersState::CONTROL,
            ),
            Outcome::Consumed
        ));
        assert_eq!(picker.take_request().unwrap().workspaces, [current, other]);
    }

    #[test]
    fn catalog_arrival_keeps_current_workspace_scope() {
        let current = WorkspaceId::new();
        let other = WorkspaceId::new();
        let profile = ProfileId::new();
        let mut picker = EntityPicker::new(Trigger::at_start());

        picker.set_context(current, Vec::new());
        assert_eq!(picker.take_request().unwrap().workspaces, [current]);

        picker.set_context(
            current,
            vec![
                WorkspaceChoice {
                    profile,
                    profile_name: "Profile".into(),
                    id: current,
                    name: "Current".into(),
                },
                WorkspaceChoice {
                    profile,
                    profile_name: "Profile".into(),
                    id: other,
                    name: "Other".into(),
                },
            ],
        );
        assert_eq!(picker.selected_workspaces, [current]);
        assert!(picker.take_request().is_none());
    }

    #[test]
    fn no_matches_still_offers_a_new_page() {
        let mut picker = EntityPicker::new(Trigger::at_start());
        assert!(matches!(picker.choose(0), Outcome::NewPage(title) if title.is_empty()));
    }

    #[test]
    fn new_page_uses_the_typed_query_as_its_title() {
        let mut picker = EntityPicker::new(Trigger::at_start());
        picker.query = "someone".into();
        assert!(matches!(picker.choose(0), Outcome::NewPage(title) if title == "someone"));
    }

    #[test]
    fn pinned_new_page_row_maps_to_the_action_while_results_scroll() {
        let workspace = WorkspaceId::new();
        let mut picker = EntityPicker::new(Trigger::at_start());
        picker.set_results(
            (0..10)
                .map(|i| DocumentChoice {
                    target: target(workspace),
                    title: format!("Document {i}"),
                    breadcrumb: String::new(),
                    is_file: false,
                })
                .collect(),
            false,
        );
        assert_eq!(picker.visible_item(MAX_ROWS - 1), Some(picker.hits.len()));
        assert!(picker.scrolls());

        let track = picker.scrollbar_track(koru_draw::theme::PANEL_W);
        let thumb = picker.scrollbar_thumb(koru_draw::theme::PANEL_W);
        picker.press_scrollbar(koru_draw::theme::PANEL_W, thumb.y + thumb.h * 0.5);
        picker.drag_scrollbar(koru_draw::theme::PANEL_W, track.y + track.h + 100.0);
        assert_eq!(picker.first, picker.max_first());
        assert!(picker.stop_scrollbar_drag());
    }

    fn choice(title: &str) -> DocumentChoice {
        DocumentChoice {
            target: DocumentTarget {
                document: DocumentId::new(),
                node: None,
            },
            title: title.to_owned(),
            breadcrumb: String::new(),
            is_file: false,
        }
    }

    fn heading(text: &str, level: u8) -> HeadingChoice {
        HeadingChoice {
            node: NodeId::new(),
            level,
            text: text.to_owned(),
        }
    }

    /// Type `@des`, highlight a document, type `#`.
    fn in_headings(hits: Vec<DocumentChoice>, sel: usize) -> EntityPicker {
        let mut picker = EntityPicker::new(Trigger::at_start());
        picker.refilter(Trigger::at_start(), "des");
        picker.set_results(hits, false);
        picker.sel = sel;
        picker.refilter(Trigger::at_start(), "des#");
        picker
    }

    /// **The whole reason the stage is derived from the query.** The query is
    /// recomputed from the row's text every frame, so a stage set by the `#`
    /// keystroke would outlive the `#` — and the person would be stuck in a
    /// list of headings with no way back that they could see.
    #[test]
    fn backspacing_over_the_hash_returns_to_the_document_list_with_the_same_row_highlighted() {
        let hits = vec![choice("Alpha"), choice("Design"), choice("Gamma")];
        let mut picker = in_headings(hits, 1);
        assert!(picker.headings().is_some(), "the `#` opened the stage");

        picker.refilter(Trigger::at_start(), "des");

        assert!(picker.headings().is_none(), "the stage went with the `#`");
        assert_eq!(picker.sel, 1, "and the highlight came back to where it was");
    }

    /// A document search can still be in flight when `#` is typed. Letting a
    /// late answer re-point the anchor would move the link somewhere the person
    /// never chose, and nothing on screen would say so.
    #[test]
    fn a_late_document_search_answer_does_not_re_point_the_anchor() {
        let hits = vec![choice("Alpha"), choice("Design")];
        let chosen = hits[1].target.document;
        let mut picker = in_headings(hits, 1);

        // The answer everyone was waiting for turns up now, with a different
        // set of documents in a different order.
        picker.set_results(vec![choice("Zulu"), choice("Yankee")], false);
        let node = heading("Rollout", 2);
        picker.set_headings(chosen, vec![node.clone()], true);

        let Outcome::Commit(spec) = picker.choose(0) else {
            panic!("a heading commits")
        };
        let target = spec.document.expect("a document target");
        assert_eq!(
            target.document, chosen,
            "still the document that was picked"
        );
        assert_eq!(target.node, Some(node.node));
    }

    /// `@#` with nothing typed before it is this page's own headings.
    #[test]
    fn a_bare_hash_anchors_on_the_current_document() {
        let here = choice("This page");
        let document = here.target.document;
        let mut picker = EntityPicker::new(Trigger::at_start());
        picker.current_document = Some(here);

        picker.refilter(Trigger::at_start(), "#");

        assert_eq!(picker.take_heading_request(), Some(document));
        assert_eq!(
            picker.headings().map(|h| h.anchor.target.document),
            Some(document)
        );
    }

    /// Decision 4: nothing is lost by having tried.
    #[test]
    fn a_document_with_no_headings_commits_the_document_level_pill() {
        let hits = vec![choice("Design")];
        let document = hits[0].target.document;
        let mut picker = in_headings(hits, 0);
        picker.set_headings(document, Vec::new(), true);

        let Outcome::Commit(spec) = picker.choose(0) else {
            panic!("the empty row still commits")
        };
        let target = spec.document.expect("a document target");
        assert_eq!(target.document, document);
        assert_eq!(
            target.node, None,
            "a document-level pill, not a broken deep link"
        );
        assert_eq!(spec.label, "Design");
    }

    /// Filtering headings is local. A round trip per keystroke inside a list
    /// already in hand would put the store in front of typing.
    #[test]
    fn text_after_the_hash_filters_headings_without_a_second_request() {
        let hits = vec![choice("Design")];
        let document = hits[0].target.document;
        let mut picker = in_headings(hits, 0);
        assert_eq!(picker.take_heading_request(), Some(document));
        picker.set_headings(
            document,
            vec![heading("Rollout", 2), heading("Rollback", 2)],
            true,
        );

        picker.refilter(Trigger::at_start(), "des#rollb");

        assert_eq!(picker.take_heading_request(), None, "no second round trip");
        let shown = &picker.headings().expect("still in the stage").shown;
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].text, "Rollback");
    }

    /// The document search must be asked for the half before the `#`, or the
    /// hits the anchor came from are cleared by an answer to "des#roll".
    #[test]
    fn the_document_query_excludes_everything_from_the_hash_onward() {
        let mut picker = EntityPicker::new(Trigger::at_start());
        picker.set_context(
            WorkspaceId::new(),
            vec![WorkspaceChoice {
                profile: ProfileId::new(),
                profile_name: "Profile".into(),
                id: WorkspaceId::new(),
                name: "Workspace".into(),
            }],
        );
        picker.set_results(vec![choice("Design")], false);
        picker.sel = 0;
        picker.refilter(Trigger::at_start(), "des#roll");

        // Whatever else it asks for, it never asks for the text after the `#`.
        if let Some(request) = picker.take_request() {
            assert_eq!(request.query, "des");
        }
    }

    /// An answer for a document the person has moved off must not appear.
    #[test]
    fn the_heading_stage_ignores_an_outline_for_another_document() {
        let hits = vec![choice("Design")];
        let mut picker = in_headings(hits, 0);

        let accepted = picker.set_headings(DocumentId::new(), vec![heading("Elsewhere", 1)], true);

        assert!(!accepted);
        assert!(
            picker
                .headings()
                .expect("still in the stage")
                .shown
                .is_empty()
        );
    }
}
