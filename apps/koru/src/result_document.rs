// SPDX-License-Identifier: MPL-2.0

//! **A result, opened as the document it is** — in a workspace nobody owns.
//!
//! The document view is built to edit a workspace document, and a result is
//! not one: an artifact version lives in the Artifact Store, it is immutable
//! (ADR-344), and opening one must not put a document into the person's
//! library that they never asked to create. So the document it is opened as
//! is a **private, in-memory workspace**: built out of bytes the tab was
//! handed, never saved, never synced, and thrown away whole when the tab
//! closes. Nothing in this file can reach the window's workspace, because it
//! holds one of its own and hands the view that one — the same structural
//! trick the *What's new* tab uses for release notes
//! ([ADR-120](../../../docs/decisions/ADR-101-125.md), and see
//! `mainview::tab::WhatsNewTab`).
//!
//! **Read, and — when the tab says so — written** (ADR-378).
//!
//! A page opens read-only and stays that way until *Edit* is pressed, and
//! `ViewState::read_only` is documented as set once and never unset, so an
//! edit does not flip the flag: it builds a second page over the same
//! snapshot with the flag off. That is one line here and it keeps the
//! editor's own invariant intact.
//!
//! **What editing does not weaken is the workspace.** It is still private,
//! still in memory, still nobody's — typing into it reaches no store, no
//! disk and no document anybody owns. A revision leaves this page only as
//! Markdown, through [`ResultDocument::markdown`] and the exporter every
//! other export in Koru goes through, and it becomes the *next* version by
//! `register_version` (ADR-364). A version is never changed.
//!
//! **The round trip is the risk, and it is not a new one.** Markdown in, a
//! Koru document, Markdown out: whatever the node model cannot hold comes
//! back spelled differently. That was already true of the composer this
//! replaced — it was loaded from the exporter's Markdown, not from the
//! version's bytes — so a save has always written the round trip.
//! `crates/koru-markdown-view/src/tests/round_trip.rs` measures exactly what
//! changes, and `MarkdownDocument::is_faithful` is how the tab says so
//! before the first keystroke.
//!
//! The bytes come across as the Loro snapshot `koru-markdown-view` already
//! built on its worker — the document that surface's Markdown was exported
//! *from* — so nothing here parses anything and the window thread does no
//! work proportional to the file beyond the one the document view does for
//! every document it opens.

use koru_core::NodeId;
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_draw::ImageCache;
use koru_draw::theme::ViewMode;
use koru_editor::Editing;
use koru_workspace::Workspace;
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState};
use winit::window::CursorIcon;

use koru_workspace_host::View;

use crate::leaf::Leaf;

/// The reading column a result is laid out in.
///
/// **Pinned, not the person's default.** A result is a page to read rather
/// than a page to lay out: Page mode's paper sheet and grey canvas belong to
/// something being written, and Full mode would run a paragraph the width of
/// the window. Narrow is the centred, readable column — and because it is a
/// pure function of the viewport width, the tab's own chrome can be put over
/// exactly the same column without asking this view where it ended up.
const MODE: ViewMode = ViewMode::Narrow;

/// One artifact version, as a document.
pub struct ResultDocument {
    /// Not the window's workspace. Never saved, never synced, thrown away
    /// whole when the tab closes or the version it was built from changes.
    workspace: Workspace,
    leaf: Leaf,
    /// Whether this page was built for typing. Read by the view that owns
    /// it, so that pressing *Edit* rebuilds rather than trying to unset a
    /// flag the editor documents as set-once.
    editable: bool,
    /// **Where the document stood when this page was built.**
    ///
    /// The cheap answer to *has anybody typed*: a Loro version vector moves
    /// when the document does and stands still when it does not, so the
    /// question costs a comparison rather than an export. Exporting to find
    /// out would be work proportional to the document, once a frame.
    opened_at: loro::VersionVector,
    /// Latched once the document has moved off [`Self::opened_at`]. Latched
    /// rather than recomputed because a person who types and then deletes
    /// has still edited; whether there is anything *different* to save is a
    /// separate question, and the tab answers it by comparing the Markdown.
    edited: bool,
}

impl ResultDocument {
    /// Open the snapshot `koru-markdown-view` built.
    ///
    /// `editable` is the whole of the difference between reading a result
    /// and revising one. It is passed at build time rather than toggled:
    /// `ViewState::read_only` is documented as set once when a view is
    /// opened and never unset, because a page whose affordances appeared and
    /// disappeared halfway through would need every one of them to handle
    /// both — so *Edit* makes a new page over the same snapshot.
    ///
    /// `None` when the bytes are not a workspace holding a document, which
    /// is the same answer a surface gives for bytes that are not a document
    /// at all: the tab keeps saying what it can say about the version rather
    /// than showing an empty page.
    pub fn of(snapshot: &[u8], editable: bool) -> Option<Self> {
        let mut doc = WorkspaceDoc::from_snapshot(snapshot).ok()?;
        let root = first_document(&doc)?;
        let mut leaf = Leaf::document(&doc, root);
        // The result's name is the tab's, over the top; the document has
        // none of its own and must not be given one.
        leaf.hide_title_row(&doc);
        leaf.set_read_only(!editable);
        Editing::set_view_mode(&mut leaf, MODE);
        if editable {
            // **Ctrl+Z is part of being editable.** Undo is off by default so
            // that a headless consumer pays nothing for it; a page somebody
            // is typing into is exactly the case that wants it. It records
            // nothing until the first edit, so this does not move the
            // version vector taken below.
            doc.enable_undo();
        }
        let opened_at = doc.version();
        Some(Self {
            workspace: Workspace::new(doc),
            leaf,
            editable,
            opened_at,
            edited: false,
        })
    }

    /// Whether this page was built for typing.
    pub fn is_editable(&self) -> bool {
        self.editable
    }

    /// Whether the document has moved since the page was built.
    pub fn is_edited(&self) -> bool {
        self.edited
    }

    /// **The Markdown this page would be saved as.**
    ///
    /// The way back out of the round trip, through `koru-markdown-view` —
    /// which is the one crate that holds both ends of it, so what a save
    /// writes and what the read-only page was drawn from cannot become two
    /// dialects. `None` when the document would not export, which is a
    /// refusal the tab says rather than a save that writes something else.
    ///
    /// Proportional to the document, so it is asked when *Save* is pressed
    /// and never in a frame.
    pub fn markdown(&self) -> Option<String> {
        koru_artifacts_ui::MarkdownDocument::of_document(&self.workspace.ws)
            .ok()
            .map(|document| document.markdown().to_owned())
    }

    /// Notice whether that last input moved the document. Cheap, and latched.
    fn note_change(&mut self) {
        if !self.edited && self.workspace.ws.version() != self.opened_at {
            self.edited = true;
        }
    }

    /// The text column this document's prose is laid out in at `width`:
    /// where it starts, and how wide it runs.
    ///
    /// **The one arithmetic**, so the chrome above the page and the page
    /// itself cannot start in two places. It is the document view's own —
    /// the centred content column, inset by the page margin and the
    /// affordance lane a row reserves for its `+` and its grip.
    pub fn text_column(width: f32) -> (f32, f32) {
        let inset = koru_draw::geometry::text_x(0u32, false);
        let x = MODE.content_x(width) + inset;
        let w = (MODE.effective_w(width) - inset * 2.0).max(120.0);
        (x, w)
    }

    /// The view, for a test to read the page out of.
    #[cfg(test)]
    pub fn leaf(&self) -> &Leaf {
        &self.leaf
    }

    /// Whether a selection drag this page owns is in flight, so the
    /// container keeps forwarding pointer moves that leave the content rect.
    pub fn is_dragging(&self) -> bool {
        let view = self.leaf.view();
        view.dragging || view.scroll_drag.is_some()
    }

    /// Whether the page wants another frame — a smooth scroll or a drag's
    /// autoscroll still in flight.
    pub fn is_animating(&self) -> bool {
        View::animating(&self.leaf)
    }

    pub fn set_viewport(&mut self, w: f32, h: f32) {
        Leaf::set_viewport(&mut self.leaf, w, h);
    }

    pub fn key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        let took = Leaf::key(&mut self.leaf, &mut self.workspace.ws, key, text, mods);
        self.note_change();
        took
    }

    pub fn set_mods(&mut self, mods: ModifiersState) {
        View::set_mods(&mut self.leaf, mods);
    }

    pub fn mouse_down(&mut self, x: f32, y: f32, ctrl: bool) -> bool {
        let took = Leaf::mouse_down(&mut self.leaf, &mut self.workspace.ws, x, y, ctrl);
        self.note_change();
        took
    }

    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        View::mouse_move(&mut self.leaf, x, y)
    }

    pub fn mouse_up(&mut self) -> bool {
        let released = Leaf::mouse_up(&mut self.leaf, &mut self.workspace.ws);
        self.note_change();
        released
    }

    pub fn mouse_double_click(&mut self, x: f32, y: f32) -> bool {
        Leaf::mouse_double_click(&mut self.leaf, &self.workspace.ws, x, y)
    }

    pub fn scroll_by(&mut self, delta: MouseScrollDelta) {
        View::scroll_by(&mut self.leaf, delta);
    }

    pub fn cursor_icon(&self) -> CursorIcon {
        View::cursor_icon(&self.leaf)
    }

    pub fn invalidate(&mut self) {
        View::invalidate(&mut self.leaf);
    }

    /// [`invalidate`](Self::invalidate) for a tab that is not shown: the
    /// page is marked and laid out when it is next sized.
    pub fn invalidate_deferred(&mut self) {
        koru_editor::Editing::invalidate_deferred(&mut self.leaf);
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        images: &ImageCache,
        root: Affine,
        caret_on: bool,
        pointer: (f32, f32),
    ) {
        // **Its own workspace, never the window's.** The two fields are
        // disjoint, which is what lets one reference to each be handed out at
        // once — and the reason this method exists rather than the caller
        // reaching in for both.
        View::draw(
            &mut self.leaf,
            scene,
            &self.workspace,
            images,
            root,
            caret_on,
            pointer,
        );
    }
}

/// The document in a workspace that holds exactly one.
fn first_document(ws: &WorkspaceDoc) -> Option<NodeId> {
    ws.children(Parent::Root).ok()?.into_iter().find(|&id| {
        ws.read(id)
            .is_ok_and(|node| node.kind == koru_core::Kind::Document)
    })
}

#[cfg(test)]
mod tests;
