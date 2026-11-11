// SPDX-License-Identifier: MPL-2.0

//! `TrashView`: state, geometry and input. What it draws is in `draw.rs`.

use std::collections::HashSet;

use koru_core::schema::WorkspaceDoc;
use koru_core::{Content, Kind, NodeId};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_ui::table::{Band, Column, Metrics, Table};
use koru_workspace_host::{Settings, View};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

/// The page's own margin. Generous on purpose: this is a short list on a wide
/// sheet, and a table pinned to the window's edges reads as a spreadsheet.
pub(super) const PAD: f32 = 40.0;
/// Between the subtitle and the column headings.
pub(super) const SUBTITLE_GAP: f32 = 28.0;
/// Between the title and its subtitle.
pub(super) const TITLE_GAP: f32 = 4.0;
/// The header block's height before it has been measured. Replaced by
/// [`TrashView::measure_header`] on the first `set_viewport`; it only matters
/// for the frame before that.
const HEADER_H_GUESS: f32 = 120.0;
pub(super) const RESTORE_W: f32 = 70.0;
pub(super) const DOTS_W: f32 = 24.0;
pub(super) const CONTROL_GAP: f32 = 8.0;
/// The lane the two controls share, **reserved on every row** whether or not
/// they are showing. A button that took its space only on hover would reflow the
/// name beside it as the pointer went down the list.
pub(super) const LANE_W: f32 = RESTORE_W + CONTROL_GAP + DOTS_W;
/// The `…` at the top right, whose only row empties the bin.
pub(super) const HEADER_DOTS: f32 = 24.0;

/// One row of the table. Read from the document when the tab is about to be
/// drawn, never per frame.
#[derive(Clone, PartialEq, Debug)]
pub struct Entry {
    pub node: NodeId,
    /// The title with any leading emoji taken off, so the emoji is not drawn
    /// twice — once as the icon and once at the head of the name.
    pub name: String,
    /// The leading emoji of the title, when it has one. The page's own icon, on
    /// exactly the rule the explore column and the tab strip already follow.
    pub icon: Option<String>,
    /// Files use the shared type icon instead of a page emoji/glyph.
    pub is_file: bool,
    /// Present for tree-backed files. Manifest-backed files currently use the
    /// name fallback in `FileIcon`, which also covers older Store listings.
    pub mime: Option<String>,
    /// Where it was, as a path a person reads. Composed here rather than in the
    /// model: it is presentation, and it has to re-derive when an ancestor is
    /// renamed rather than freeze at the moment it was binned.
    pub origin: String,
    pub trashed_at_ms: Option<i64>,
    pub nested: usize,
    /// Who binned it, already resolved for display: `"You"` when it was this
    /// window, the label the other client wrote when it was not, and `"Someone
    /// else"` when there is no label at all.
    ///
    /// **Never a guess.** Nothing infers who acted; see `NodeMeta::trashed_by`.
    pub deleted_by: String,
    /// **The run this row is**, when it is a run rather than a page.
    ///
    /// A run is a document of the workspace's manifest like any other, so it
    /// is in the bin the same way — but restoring one and deleting one for
    /// good are each two halves, and only one half is the manifest's. The
    /// container reads this to send the other half to the delegate worker.
    pub run: Option<koru_delegate_core::RunId>,
    /// **The Bot or Bot conversation this row is** (Bots P1.8 S14), when it
    /// is one. Not a workspace document at all: its Restore is the Bot
    /// owner's, and it is never deleted for good — that is not built.
    pub bot: Option<crate::bots::trash::BotSubject>,
    /// Whether the row offers Restore. Every page and run does; a Bot row
    /// only when the window knows the Trash its Restore names.
    pub restorable: bool,
}

/// Newest delete first. Missing timestamps sink to the bottom so a row without
/// a clock never steals the top of the list from one that has one.
fn sort_newest_first(entries: &mut [Entry]) {
    entries.sort_by(|a, b| match (a.trashed_at_ms, b.trashed_at_ms) {
        (Some(x), Some(y)) => y.cmp(&x),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(super) enum Hit {
    Row(usize),
    /// The Restore button. Outranks the row it sits in, exactly as a tab's
    /// close box outranks the tab.
    Restore(usize),
    Dots(usize),
    HeaderDots,
}

/// What the view wants the container to do.
///
/// Deliberately thin: it never mutates the document and it never builds a menu.
#[derive(Clone, PartialEq, Debug)]
pub enum Request {
    Restore(Vec<NodeId>),
    RowMenu { nodes: Vec<NodeId>, at: (f32, f32) },
    HeaderMenu { at: (f32, f32) },
}

/// The page's title, in the person's language (DESIGN.md: Trash text is
/// localized).
pub(super) fn title_text() -> &'static str {
    koru_i18n::t(crate::format::lang(), koru_i18n::Key::TrashTitle)
}

/// The page's subtitle.
pub(super) fn subtitle_text() -> &'static str {
    koru_i18n::t(crate::format::lang(), koru_i18n::Key::TrashSubtitle)
}

/// *You*, for a row this window binned.
fn you() -> String {
    koru_i18n::t(crate::format::lang(), koru_i18n::Key::TrashYou).to_owned()
}

/// *Someone else*, for a row whose client wrote no label. Never a guess.
fn someone_else() -> String {
    koru_i18n::t(crate::format::lang(), koru_i18n::Key::TrashSomeoneElse).to_owned()
}

pub struct TrashView {
    pub(super) viewport: (f32, f32),
    pub(super) engine: TextEngine,
    pub(super) entries: Vec<Entry>,
    /// Stamped when `entries` was read, so "Deleted today" is measured against
    /// when the list was taken rather than against a clock this file owns.
    pub(super) now_ms: i64,
    pub(super) hover: Option<Hit>,
    pub(super) selection: HashSet<NodeId>,
    selection_anchor: Option<NodeId>,
    mods: ModifiersState,
    /// This window's own actor label, so a row it binned reads "You". `None`
    /// when nobody has set a name.
    actor: Option<String>,
    /// How tall the heading block actually is, measured rather than guessed —
    /// the title and its subtitle are text, and their height follows the font
    /// role and the density, both of which a person can change while looking at
    /// this page.
    header_h: f32,
    scroll: f32,
    /// Set by `adopt`, drained by `catch_up`. **A background Trash tab costs a
    /// bool** — the same deferred-work contract a document tab has.
    stale: bool,
    pending: Option<Request>,
    /// The workspace's trashed **documents**, pushed by the container.
    ///
    /// **This is the bin now.** It used to read the open document's hidden
    /// `Kind::Trash` node, which was right while a document *was* a node in the
    /// library tree and has been empty ever since documents became manifest
    /// entries — so the screen said "The Trash is empty" while holding several.
    ///
    /// `None` before the first push, and for a workspace with no manifest: the
    /// node walk is still the answer there, and it is what the editor's own
    /// suite drives.
    documents: Option<Vec<koru_core::explore::ExploreEntry>>,
    /// **The runs in the workspace's bin**, pushed by the container from the
    /// run index.
    ///
    /// They are manifest entries like the documents above and are trashed
    /// the same way — but they are filtered out of the listing a *person*
    /// sees (`koru_core::explore::listed`), because nobody opens, renames
    /// or drags a run as a page. The bin is the one place they belong in a
    /// list, so they arrive here by their own road, named by the two words
    /// a person recognises rather than by the manifest's `run-9f2c11ab`.
    runs: Vec<koru_runs_ui::TrashedRunRow>,
    /// **The trashed Bots and Bot conversations** (Bots P1.8 S14), pushed by
    /// the container from the Bot owner's Trash filter and the window's
    /// book. Not documents: they arrive by their own road, as runs do.
    bots: Vec<crate::bots::trash::TrashedBotRow>,
}

impl Default for TrashView {
    fn default() -> Self {
        Self::new()
    }
}

impl TrashView {
    pub fn new() -> Self {
        Self {
            viewport: (900.0, 600.0),
            engine: TextEngine::new(),
            entries: Vec::new(),
            now_ms: 0,
            hover: None,
            selection: HashSet::new(),
            selection_anchor: None,
            mods: ModifiersState::empty(),
            actor: None,
            header_h: HEADER_H_GUESS,
            scroll: 0.0,
            stale: true,
            pending: None,
            documents: None,
            runs: Vec::new(),
            bots: Vec::new(),
        }
    }

    /// Re-read the listing, if anything has changed since it was last read.
    ///
    /// Called from `TabView::catch_up`, which runs **only for the tab about to
    /// be drawn** — so a Trash tab sitting in the background does not walk the
    /// document every time somebody types in another one.
    /// The workspace's trashed documents, from the manifest.
    ///
    /// Returns whether anything changed, so an unchanged push costs no frame.
    pub fn set_documents(&mut self, documents: Vec<koru_core::explore::ExploreEntry>) -> bool {
        if self.documents.as_ref() == Some(&documents) {
            return false;
        }
        self.documents = Some(documents);
        self.stale = true;
        true
    }

    /// The runs in the bin, from the run index. Same contract as
    /// [`set_documents`]: an unchanged push costs no frame.
    ///
    /// [`set_documents`]: Self::set_documents
    pub fn set_runs(&mut self, runs: Vec<koru_runs_ui::TrashedRunRow>) -> bool {
        if self.runs == runs {
            return false;
        }
        self.runs = runs;
        self.stale = true;
        true
    }

    /// The trashed Bots and Bot conversations. Same contract as
    /// [`set_runs`](Self::set_runs).
    pub(crate) fn set_bots(&mut self, bots: Vec<crate::bots::trash::TrashedBotRow>) -> bool {
        if self.bots == bots {
            return false;
        }
        self.bots = bots;
        self.stale = true;
        true
    }

    pub fn catch_up(&mut self, ws: &WorkspaceDoc) -> bool {
        if !std::mem::take(&mut self.stale) {
            return false;
        }
        self.now_ms = koru_workspace::open::now_ms();
        if let Some(documents) = &self.documents {
            // **Titles resolve against the whole list**, binned or not: a
            // document's original location may be a section that is still in the
            // library, and one that went with it is in here.
            let title_of = |id: koru_core::DocumentId| -> Option<String> {
                documents
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| e.title.clone())
            };
            self.entries = documents
                .iter()
                .map(|e| {
                    let (icon, rest) = koru_shell_chrome::split_leading_emoji(&e.title);
                    Entry {
                        node: NodeId::from_uuid(e.id.as_uuid()),
                        name: if rest.is_empty() {
                            "Untitled".into()
                        } else {
                            rest.to_owned()
                        },
                        icon: icon.map(str::to_owned),
                        is_file: e.kind == koru_core::explore::KIND_FILE,
                        mime: e.mime.clone(),
                        origin: e
                            .parent
                            .and_then(title_of)
                            .unwrap_or_else(|| "Library".to_owned()),
                        trashed_at_ms: e.trashed_at,
                        // Documents filed under this one, which is what the
                        // confirm and the row's `· N nested` suffix quote.
                        nested: documents.iter().filter(|c| c.parent == Some(e.id)).count(),
                        deleted_by: match e.trashed_by.as_deref() {
                            Some(who) if Some(who) == self.actor.as_deref() => you(),
                            Some(who) => who.to_owned(),
                            None => someone_else(),
                        },
                        run: None,
                        bot: None,
                        restorable: true,
                    }
                })
                .collect();
            self.entries.extend(self.run_entries());
            self.entries.extend(self.bot_entries());
            sort_newest_first(&mut self.entries);
            self.keep_selection();
            return true;
        }
        self.entries = ws
            .trashed()
            .unwrap_or_default()
            .into_iter()
            .map(|e| {
                let read = ws.read(e.node).ok();
                let is_file = read.as_ref().is_some_and(|node| node.kind == Kind::File);
                let mime = read.as_ref().and_then(|node| match &node.content {
                    Content::Blob { mime, .. } => Some(mime.clone()),
                    _ => None,
                });
                Entry {
                    node: e.node,
                    // One rule for what a page's icon is, in one place:
                    // `split_leading_emoji`, the same call the explore column and
                    // the tab strip make. A second copy of "is the first character
                    // an emoji" is how three surfaces come to disagree.
                    name: {
                        let (_, rest) = koru_shell_chrome::split_leading_emoji(&e.name);
                        if rest.is_empty() {
                            "Untitled".into()
                        } else {
                            rest.to_owned()
                        }
                    },
                    icon: koru_shell_chrome::split_leading_emoji(&e.name)
                        .0
                        .map(str::to_owned),
                    is_file,
                    mime,
                    origin: origin_path(ws, e.origin),
                    trashed_at_ms: e.trashed_at,
                    nested: e.nested,
                    deleted_by: match e.trashed_by.as_deref() {
                        Some(who) if Some(who) == self.actor.as_deref() => you(),
                        Some(who) => who.to_owned(),
                        // No label at all — an unnamed profile, or a build that
                        // never wrote one. Say that it was somebody rather than
                        // naming the wrong person.
                        None => someone_else(),
                    },
                    run: None,
                    bot: None,
                    restorable: true,
                }
            })
            .collect();
        self.entries.extend(self.run_entries());
        self.entries.extend(self.bot_entries());
        sort_newest_first(&mut self.entries);
        self.keep_selection();
        true
    }

    /// A selection naming something that has left the bin — restored here or
    /// by a peer — falls back to nothing rather than to a neighbour.
    fn keep_selection(&mut self) {
        self.selection
            .retain(|id| self.entries.iter().any(|e| e.node == *id));
        if self
            .selection_anchor
            .is_some_and(|id| !self.entries.iter().any(|e| e.node == id))
        {
            self.selection_anchor = None;
        }
    }

    /// The trashed Bots and Bot conversations, as rows of the same table:
    /// keyed by the Bot's or the conversation's own id, the way a run's row
    /// is keyed by the run's. *Deleted by* is this person: only the owner's
    /// own Bots are listed.
    fn bot_entries(&self) -> Vec<Entry> {
        self.bots
            .iter()
            .map(|row| Entry {
                node: NodeId::from_uuid(row.subject.uuid()),
                name: row.name.clone(),
                icon: None,
                is_file: false,
                mime: None,
                origin: if row.restorable {
                    row.origin.clone()
                } else {
                    format!(
                        "{} · {}",
                        row.origin,
                        koru_i18n::t(
                            crate::format::lang(),
                            koru_i18n::Key::TrashRestoreUnavailable
                        )
                    )
                },
                trashed_at_ms: row.trashed_at_ms,
                nested: 0,
                deleted_by: you(),
                run: None,
                bot: Some(row.subject),
                restorable: row.restorable,
            })
            .collect()
    }

    /// The Bot rows among `nodes`, and the rest. Read by the container,
    /// which restores a Bot row through the Bot owner and never deletes one.
    pub(crate) fn split_bots(
        &self,
        nodes: Vec<NodeId>,
    ) -> (Vec<crate::bots::trash::BotSubject>, Vec<NodeId>) {
        let mut bots = Vec::new();
        let mut rest = Vec::new();
        for node in nodes {
            match self
                .entries
                .iter()
                .find(|entry| entry.node == node)
                .and_then(|entry| entry.bot)
            {
                Some(subject) => bots.push(subject),
                None => rest.push(node),
            }
        }
        (bots, rest)
    }

    /// The bin's runs, as rows of the same table.
    ///
    /// **The run's name, not the manifest's title.** The manifest calls a
    /// run `run-9f2c11ab` — a short, content-free name nothing reads — and
    /// what a person recognises is the two words derived from its id
    /// (ADR-360). *Origin* says *Run* rather than a folder, because a run
    /// came from the Agents column and not from anywhere in the library.
    fn run_entries(&self) -> Vec<Entry> {
        let lang = crate::format::lang();
        self.runs
            .iter()
            .map(|row| Entry {
                node: NodeId::from_uuid(row.run.as_uuid()),
                name: row.name.clone(),
                icon: None,
                is_file: false,
                mime: None,
                origin: match row.artifacts.deletes() {
                    // What the person answered when it went in, so it can be
                    // read back before the bin is emptied rather than only
                    // afterwards.
                    true => format!(
                        "{} · {}",
                        koru_i18n::t(lang, koru_i18n::Key::TrashRunOrigin),
                        koru_i18n::t(lang, koru_i18n::Key::TrashRunResultsDeleted)
                    ),
                    false => koru_i18n::t(lang, koru_i18n::Key::TrashRunOrigin).to_owned(),
                },
                trashed_at_ms: row.trashed_at_ms,
                nested: 0,
                deleted_by: you(),
                run: Some(row.run),
                bot: None,
                restorable: true,
            })
            .collect()
    }

    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    /// Tell the view who this window is, so it can say "You" rather than
    /// repeating your own name back at you.
    pub fn set_actor(&mut self, actor: Option<String>) {
        if self.actor != actor {
            self.actor = actor;
            self.stale = true;
        }
    }

    /// Select the top-level Trash entry that a search result belongs to. The
    /// entry list may still be stale when navigation arrives; `catch_up`
    /// validates the id after rebuilding instead of silently selecting a
    /// neighbouring row.
    pub fn reveal(&mut self, node: NodeId) {
        self.selection.clear();
        self.selection.insert(node);
        self.selection_anchor = Some(node);
        self.stale = true;
    }

    pub fn take_request(&mut self) -> Option<Request> {
        self.pending.take()
    }

    /// Whether there is anything in the bin. Read by the tests, and by anything
    /// that wants to know without asking the document again.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    // ---------------------------------------------------------- geometry

    pub(super) fn table(&self) -> Table {
        Table::new(
            Rect::new(0.0, 0.0, self.viewport.0, self.viewport.1),
            Metrics::manager()
                .with_pad(PAD)
                // **The same height as an explore row.** These are the same
                // kind of thing — one line naming a document — and a table row
                // half again as tall as the tree's made the list read as a
                // spreadsheet of records rather than a list of pages.
                .with_row_h(koru_draw::theme::NAV_ROW_H)
                .with_header(self.header_h)
                .with_footer(0.0)
                // Rounded, quieter rows: with this much air around the table a
                // full-strength band across it shouts.
                .roomy(),
            self.scroll,
        )
    }

    /// Every row is a row: this table has no sections, and saying so in one
    /// place keeps the table's contract honest rather than special-casing it.
    pub(super) fn bands(&self) -> Vec<Band> {
        vec![Band::Row; self.entries.len()]
    }

    /// Sentence case, not small caps: these are labels for what is under them,
    /// not a section heading, and shouting them competes with the page title.
    pub(super) fn columns(&self) -> [Column; 4] {
        let w = self.viewport.0;
        [
            Column::new(PAD, "Name"),
            Column::new((w * 0.42).max(240.0), "Original location"),
            Column::new((w * 0.60).max(340.0), "Deleted"),
            // The label the acting client wrote, which is the only reason it is
            // stored — see ADR-70. "You" when this window did it.
            Column::new((w * 0.76).max(440.0), "Deleted by"),
        ]
    }

    /// Measure the heading block: the page's top margin, the title, the gap, the
    /// subtitle, and [`SUBTITLE_GAP`] of air before the table starts.
    ///
    /// Called whenever the viewport changes, which is also when the font role or
    /// the density could have. Cheap — two `simple_layout`s of fixed strings.
    fn measure_header(&mut self) {
        let title = self.engine.simple_layout(title_text(), super::draw::TITLE);
        let sub = self
            .engine
            .simple_layout(subtitle_text(), koru_draw::theme::role::meta());
        self.header_h = self.title_top() + title.height() + TITLE_GAP + sub.height() + SUBTITLE_GAP;
    }

    /// Where the title's layout box starts.
    ///
    /// **Less than [`PAD`], on purpose.** A text layout reserves half its
    /// leading above the glyphs, so a title drawn at `y = PAD` has its visible
    /// top a few pixels lower than a column drawn at `x = PAD` — and the page
    /// then looks as though it has more air on top than down the side, which is
    /// exactly what it had. Trimming the half-leading makes the two margins
    /// match optically rather than only arithmetically.
    pub(super) fn title_top(&self) -> f32 {
        let m = super::draw::TITLE.metrics;
        (PAD - (m.line_height - m.size) * 0.5).max(0.0)
    }

    /// The `…` beside the heading. Its own target, and it is always there —
    /// unlike a row's controls, which appear on hover.
    pub(super) fn header_dots(&self) -> Rect {
        Rect::new(
            self.viewport.0 - PAD - HEADER_DOTS,
            self.title_top(),
            HEADER_DOTS,
            HEADER_DOTS,
        )
    }

    /// The Restore button and the `…` inside row `r`, in reading order.
    pub(super) fn controls(&self, r: Rect) -> Vec<Rect> {
        self.table()
            .trailing_rects(r, LANE_W, &[RESTORE_W, DOTS_W], CONTROL_GAP)
    }

    /// Where row `i`'s `…` is, in this view's own coordinates. Tests only — the
    /// container's test for the anchor translation needs a button to press
    /// without reaching into the geometry module by module.
    #[cfg(test)]
    pub(crate) fn dots_rect_for_test(&self, i: usize) -> Rect {
        self.controls(self.table().row_rect(&self.bands(), i))[1]
    }

    pub(super) fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        if self.header_dots().contains(x, y) {
            return Some(Hit::HeaderDots);
        }
        let bands = self.bands();
        let i = self.table().row_at(&bands, x, y)?;
        let r = self.table().row_rect(&bands, i);
        // Inside-out: the controls sit *in* the row, so they outrank it.
        let c = self.controls(r);
        let entry = self.entries.get(i);
        // A Bot row has no ⋯ — its only answer is Restore, when it can be
        // asked for here — and no Delete permanently, which is not built.
        if c[0].contains(x, y) && entry.is_some_and(|e| e.restorable) {
            return Some(Hit::Restore(i));
        }
        if c[1].contains(x, y) && entry.is_some_and(|e| e.bot.is_none()) {
            return Some(Hit::Dots(i));
        }
        Some(Hit::Row(i))
    }

    fn node_at(&self, i: usize) -> Option<NodeId> {
        self.entries.get(i).map(|e| e.node)
    }

    fn selected_in_order(&self) -> Vec<NodeId> {
        self.entries
            .iter()
            .map(|e| e.node)
            .filter(|id| self.selection.contains(id))
            .collect()
    }

    fn select_only(&mut self, node: NodeId) {
        self.selection.clear();
        self.selection.insert(node);
        self.selection_anchor = Some(node);
    }

    fn select_row(&mut self, i: usize, ctrl: bool, shift: bool) {
        let Some(node) = self.node_at(i) else {
            return;
        };
        if shift {
            let anchor = self
                .selection_anchor
                .and_then(|id| self.entries.iter().position(|e| e.node == id))
                .unwrap_or(i);
            let (first, last) = if anchor <= i {
                (anchor, i)
            } else {
                (i, anchor)
            };
            self.selection.clear();
            self.selection
                .extend(self.entries[first..=last].iter().map(|e| e.node));
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(node);
            }
        } else if ctrl {
            if !self.selection.remove(&node) {
                self.selection.insert(node);
            }
            self.selection_anchor = Some(node);
        } else {
            self.select_only(node);
        }
    }

    /// A row control acts on the complete selection when its row belongs to
    /// that selection. An unselected row first becomes the sole selection,
    /// which prevents an old, unrelated selection from being acted on.
    fn action_nodes(&mut self, i: usize) -> Vec<NodeId> {
        let Some(node) = self.node_at(i) else {
            return Vec::new();
        };
        if !self.selection.contains(&node) {
            self.select_only(node);
        }
        self.selected_in_order()
    }

    fn clamp_scroll(&mut self) {
        let max = self.table().max_scroll(&self.bands());
        self.scroll = self.scroll.clamp(0.0, max);
    }

    fn step(&mut self, delta: i32) {
        if self.entries.is_empty() {
            return;
        }
        let at = self
            .selection_anchor
            .and_then(|id| self.entries.iter().position(|e| e.node == id));
        let next = match at {
            Some(i) => i
                .saturating_add_signed(delta as isize)
                .min(self.entries.len() - 1),
            None if delta > 0 => 0,
            None => self.entries.len() - 1,
        };
        if let Some(node) = self.node_at(next) {
            self.select_only(node);
        }
    }

    // ------------------------------------------------------------- input

    fn press(&mut self, x: f32, y: f32, ctrl: bool, shift: bool) -> bool {
        match self.hit_at(x, y) {
            Some(Hit::HeaderDots) => {
                let d = self.header_dots();
                self.pending = Some(Request::HeaderMenu {
                    at: (d.x + d.w, d.y + d.h),
                });
                true
            }
            Some(Hit::Restore(i)) => {
                let nodes = self.action_nodes(i);
                self.pending = (!nodes.is_empty()).then_some(Request::Restore(nodes));
                self.pending.is_some()
            }
            Some(Hit::Dots(i)) => {
                let r = self.table().row_rect(&self.bands(), i);
                let d = self.controls(r)[1];
                let nodes = self.action_nodes(i);
                self.pending = (!nodes.is_empty()).then_some(Request::RowMenu {
                    nodes,
                    // The button's bottom-right, so the menu hangs back under it
                    // rather than out past the window's edge.
                    at: (d.x + d.w, d.y + d.h),
                });
                self.pending.is_some()
            }
            Some(Hit::Row(i)) => {
                self.select_row(i, ctrl, shift);
                true
            }
            None => {
                let had = !self.selection.is_empty();
                self.selection.clear();
                self.selection_anchor = None;
                had
            }
        }
    }
}

/// Where a trashed item goes back to, as a person reads it.
///
/// Composed here rather than in the model: the model hands back a `NodeId`, and
/// what a path *reads* like — the separator, "Library" for the top level, an
/// empty title showing as "Untitled" — is presentation. Deriving it now rather
/// than freezing it at the moment of binning is also what makes it stay true
/// when somebody renames a folder in between.
fn origin_path(ws: &WorkspaceDoc, origin: Option<NodeId>) -> String {
    let Some(id) = origin else {
        return "Library".into();
    };
    if ws.read(id).is_err() {
        // The parent has gone, so a restore will land at the top. Say where it
        // will actually go rather than where it came from.
        return "Library".into();
    }
    let mut parts: Vec<String> = ws
        .ancestors(id)
        .unwrap_or_default()
        .into_iter()
        .rev()
        .map(|a| koru_workspace::name_of(ws, a))
        .collect();
    parts.push(koru_workspace::name_of(ws, id));
    parts.join(" / ")
}

impl View for TrashView {
    fn key(
        &mut self,
        _ws: &mut WorkspaceDoc,
        key: &Key,
        _text: Option<&str>,
        _mods: ModifiersState,
    ) -> bool {
        match key {
            Key::Named(NamedKey::ArrowDown) => {
                self.step(1);
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.step(-1);
                true
            }
            Key::Named(NamedKey::Enter) => {
                let nodes = self.selected_in_order();
                self.pending = (!nodes.is_empty()).then_some(Request::Restore(nodes));
                self.pending.is_some()
            }
            Key::Named(NamedKey::Escape) => {
                let had = !self.selection.is_empty();
                self.selection.clear();
                self.selection_anchor = None;
                had
            }
            _ => false,
        }
    }

    fn mouse_down(&mut self, _ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        self.press(x, y, ctrl, self.mods.shift_key())
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let was = self.hover;
        self.hover = self.hit_at(x, y);
        was != self.hover
    }

    fn mouse_up(&mut self, _ws: &mut WorkspaceDoc) -> bool {
        false
    }

    /// **Nothing.** Opening a document that is in the bin is a state the tab
    /// strip has no story for, and "restore, then open" is two decisions worn as
    /// one — so a double-click asks for neither.
    fn mouse_double_click(&mut self, _ws: &WorkspaceDoc, _x: f32, _y: f32) -> bool {
        false
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * Metrics::manager().row_h,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        self.scroll -= dy;
        self.clamp_scroll();
    }

    fn set_viewport(&mut self, w: f32, h: f32) {
        self.viewport = (w, h);
        // Also the moment the font role or the density could have moved under
        // us, which is the other thing the heading's height depends on.
        self.measure_header();
        self.clamp_scroll();
    }

    fn set_mods(&mut self, mods: ModifiersState) {
        self.mods = mods;
    }

    fn cursor_icon(&self) -> CursorIcon {
        match self.hover {
            Some(Hit::Restore(_)) | Some(Hit::Dots(_)) | Some(Hit::HeaderDots) => {
                koru_ui::cursor::PRESS
            }
            _ => koru_ui::cursor::IDLE,
        }
    }

    /// Only a flag. The listing is read in `catch_up`, which runs for the tab
    /// about to be drawn and not for the ones that are not.
    fn adopt(&mut self, _ws: &WorkspaceDoc) {
        self.stale = true;
    }

    fn reseat(&mut self, _ws: &WorkspaceDoc) {
        self.stale = true;
        self.selection.clear();
        self.selection_anchor = None;
        self.scroll = 0.0;
    }

    fn invalidate(&mut self) {}

    fn draw(
        &mut self,
        scene: &mut Scene,
        _workspace: &koru_workspace::Workspace,
        _images: &koru_draw::ImageCache,
        root: Affine,
        _caret_on: bool,
        _pointer: (f32, f32),
    ) {
        self.paint(scene, root);
    }

    fn restore(&mut self, _ws: &koru_workspace::Workspace, _settings: &Settings) {}

    fn capture(&self, _settings: &mut Settings) {}
}
