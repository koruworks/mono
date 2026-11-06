// SPDX-License-Identifier: MPL-2.0

//! The note above a Tasks page's rows: a leaf over the page's document, in a
//! band under the heading (page-document plan §6.9, D7, D8).
//!
//! The same component a document tab is — `Leaf` — with its title row hidden
//! (the heading above is the page's name) and its own scroll, laid out to its
//! content until that reaches forty percent of the page, then scrolling
//! inside the band. The rows move down by the band. It is built only over
//! the document it belongs to: the Tasks tab activates its page document the
//! way selecting a document tab does, so the leaf here is a leaf over the
//! active document and every guard a document tab has holds unchanged.

use koru_core::schema::{WorkspaceDoc, genesis_root_id};
use koru_core::{DocumentId, NodeId};
use koru_draw::ImageCache;
use koru_draw::geometry::Rect;
use koru_editor::Editing;
use koru_workspace::Workspace;
use koru_workspace_host::View;
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState};
use winit::window::CursorIcon;

use crate::leaf::Leaf;

/// How much of the page the band may take before it scrolls (D8).
const BAND_SHARE: f32 = 0.4;

/// The body is laid out full width; its text starts where the heading's
/// does, which is what the horizontal offset below arranges.
const MODE: koru_draw::theme::ViewMode = koru_draw::theme::ViewMode::Full;

pub(super) struct PageBody {
    pub document: DocumentId,
    pub root: NodeId,
    leaf: Leaf,
    /// Whether the keyboard is in the note rather than in the table.
    pub focused: bool,
    /// The band this frame: where it starts and how tall it is, in page
    /// coordinates. Set by [`layout`](Self::layout).
    pub band: Rect,
    /// Where the leaf's own origin is inside the band, so the text column
    /// starts at the page's padding rather than the leaf's.
    origin: (f32, f32),
    /// The leaf has to be laid out again before it is drawn.
    dirty: bool,
}

impl PageBody {
    /// A body over `document`, which must be the document `ws` is — the
    /// root the genesis gave it has to be there.
    pub fn open(ws: &WorkspaceDoc, document: DocumentId, placeholder: String) -> Option<Self> {
        let root = genesis_root_id(document);
        ws.read(root).ok()?;
        let mut leaf = Leaf::document(ws, root);
        leaf.hide_title_row(ws);
        leaf.set_body_placeholder(placeholder);
        Editing::set_view_mode(&mut leaf, MODE);
        Some(Self {
            document,
            root,
            leaf,
            focused: false,
            band: Rect::new(0.0, 0.0, 0.0, 0.0),
            origin: (0.0, 0.0),
            dirty: true,
        })
    }

    /// Whether `ws` is the document this body is over — the identity guard
    /// every leaf arm makes before taking a document in.
    pub fn is_in(&self, ws: &WorkspaceDoc) -> bool {
        ws.read(self.root).is_ok()
    }

    pub fn leaf(&self) -> &Leaf {
        &self.leaf
    }

    pub fn leaf_mut(&mut self) -> &mut Leaf {
        self.dirty = true;
        &mut self.leaf
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Lay the band out under `top`, `w` wide, on a page `page_h` tall: the
    /// content's height up to the share the page allows. Answers the band's
    /// height. Lays the leaf out only when something moved it.
    pub fn layout(&mut self, pad: f32, top: f32, w: f32, page_h: f32) -> f32 {
        let cap = (page_h * BAND_SHARE).max(1.0);
        let inset = koru_draw::geometry::text_x(0u32, false);
        let x = pad - inset - MODE.content_x(w);
        let leaf_w = (w - x).max(1.0);
        if self.dirty || self.band.w != w || self.band.y != top {
            // Measured at the cap so the content height is known, then the
            // band is cut to the content when it is shorter.
            Leaf::set_viewport(&mut self.leaf, leaf_w, cap);
            let content = koru_editor::rowlayout::content_height(self.leaf.body());
            let h = content.min(cap).max(1.0);
            if (h - cap).abs() > 0.5 {
                Leaf::set_viewport(&mut self.leaf, leaf_w, h);
            }
            self.band = Rect::new(0.0, top, w, h);
            self.origin = (x, top);
            self.dirty = false;
        }
        self.band.h
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.band.contains(x, y)
    }

    fn local(&self, x: f32, y: f32) -> (f32, f32) {
        (x - self.origin.0, y - self.origin.1)
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        workspace: &Workspace,
        images: &ImageCache,
        root: Affine,
        caret_on: bool,
        pointer: (f32, f32),
    ) {
        if self.band.h <= 0.0 {
            return;
        }
        // The clip is not optional: the leaf draws a row of margin either
        // side of its visible range, and a note scrolled by half a line
        // would paint that line over the heading or the first task.
        scene.push_clip_layer(
            vello::peniko::Fill::NonZero,
            root,
            &vello::kurbo::Rect::new(
                0.0,
                self.band.y as f64,
                self.band.w as f64,
                (self.band.y + self.band.h) as f64,
            ),
        );
        let (lx, ly) = self.local(pointer.0, pointer.1);
        View::draw(
            &mut self.leaf,
            scene,
            workspace,
            images,
            root * Affine::translate((self.origin.0 as f64, self.origin.1 as f64)),
            caret_on && self.focused,
            (lx, ly),
        );
        scene.pop_layer();
    }

    pub fn key(
        &mut self,
        ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        self.dirty = true;
        Leaf::key(&mut self.leaf, ws, key, text, mods)
    }

    /// A press anywhere in the band is a press in the note: left of the text
    /// column it lands at the start of the nearest line rather than in the
    /// leaf's affordance lane, so the caret always arrives.
    pub fn mouse_down(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        let (lx, ly) = self.local(x, y);
        let lx = lx.max(koru_draw::geometry::text_x(0u32, false));
        self.dirty = true;
        let took = Leaf::mouse_down(&mut self.leaf, ws, lx, ly.max(0.0), ctrl);
        if self.leaf.caret_spot().is_none() {
            // Above the first line or below the last: the press began no
            // selection, so end it, and the caret still arrives — at the
            // start of the first line.
            Leaf::mouse_up(&mut self.leaf, ws);
            let first = self.leaf.body().first_body_row();
            self.leaf.place_caret_at(first, 0);
        }
        // The band is the note: a press anywhere in it gives the editor the
        // keyboard, which a press on a line's own glyphs does by itself and
        // a press beside them does not.
        Editing::view_mut(&mut self.leaf).active = true;
        took || self.focused
    }

    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let (lx, ly) = self.local(x, y);
        View::mouse_move(&mut self.leaf, lx, ly)
    }

    pub fn mouse_up(&mut self, ws: &mut WorkspaceDoc) -> bool {
        self.dirty = true;
        Leaf::mouse_up(&mut self.leaf, ws)
    }

    pub fn mouse_double_click(&mut self, ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        let (lx, ly) = self.local(x, y);
        Leaf::mouse_double_click(&mut self.leaf, ws, lx, ly)
    }

    pub fn scroll_by(&mut self, delta: MouseScrollDelta) {
        View::scroll_by(&mut self.leaf, delta);
    }

    pub fn set_mods(&mut self, mods: ModifiersState) {
        View::set_mods(&mut self.leaf, mods);
    }

    pub fn cursor_icon(&self) -> CursorIcon {
        View::cursor_icon(&self.leaf)
    }

    pub fn is_dragging(&self) -> bool {
        let view = self.leaf.view();
        view.dragging || view.scroll_drag.is_some()
    }

    pub fn is_animating(&self) -> bool {
        View::animating(&self.leaf)
    }

    /// Take in the document's changes, the way a document tab's leaf does —
    /// only when `ws` is its document.
    pub fn adopt(&mut self, ws: &WorkspaceDoc) {
        if self.is_in(ws) {
            self.leaf.body_mut().defer_pending(ws);
        }
    }

    pub fn catch_up(&mut self, ws: &WorkspaceDoc) -> bool {
        if !self.is_in(ws) || !self.leaf.body().is_stale() {
            return false;
        }
        self.leaf.catch_up(ws);
        self.dirty = true;
        true
    }

    /// Put the caret in the note.
    pub fn focus(&mut self) {
        self.focused = true;
    }

    /// Hand the keyboard back to the table.
    pub fn blur(&mut self) {
        self.focused = false;
    }
}
