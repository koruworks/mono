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

/// How much of the page the band may take before it scrolls (D8), while
/// nobody has sized it by hand.
const BAND_SHARE: f32 = 0.4;

/// What a band sized by hand leaves the rows below: the heading's height
/// again, so the table never vanishes under a tall note.
const MIN_ROWS_H: f32 = 120.0;

/// The grip's hit lane straddling the band's bottom edge, in px — the pane
/// divider's recipe, so the two drag the same way.
const GRIP_HIT: f32 = 7.0;

/// The corner mark that says the band can be dragged, like an HTML text
/// area's: its side.
pub(super) const GRIP_MARK: f32 = 10.0;

/// The *open in a new tab* control's side, at the band's top-right — the
/// header's `…`, one row down.
pub(super) const OPEN_ICON: f32 = super::view::HEADER_DOTS;

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
    /// Built and not yet fed the container's caches — entity titles, the
    /// schedule pills — which every document leaf gets on open.
    fresh: bool,
    /// The height the band was last laid out to by hand, clamped to the
    /// page, or `None` while it follows its content.
    fixed: Option<f32>,
    /// The page's padding, so the corner controls sit on its margin.
    pad: f32,
}

impl PageBody {
    /// A body over `document`, which must be the document `ws` is — the
    /// root the genesis gave it has to be there.
    pub fn open(
        ws: &WorkspaceDoc,
        document: DocumentId,
        placeholder: String,
        default_list: Option<koru_core::ids::ListId>,
    ) -> Option<Self> {
        let root = genesis_root_id(document);
        ws.read(root).ok()?;
        let mut leaf = Leaf::document(ws, root);
        leaf.hide_title_row(ws);
        leaf.set_body_placeholder(placeholder);
        Editing::set_view_mode(&mut leaf, MODE);
        // A task written on a list's note joins the list (D5); a bucket's
        // note gives nothing (D6).
        Editing::view_mut(&mut leaf).default_list = default_list;
        Some(Self {
            document,
            root,
            leaf,
            focused: false,
            band: Rect::new(0.0, 0.0, 0.0, 0.0),
            origin: (0.0, 0.0),
            dirty: true,
            fresh: true,
            fixed: None,
            pad: 0.0,
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
    /// content's height up to the share the page allows, or `fixed` when
    /// the band was sized by hand — then it is that tall whatever the
    /// content, scrolling inside when the note is taller and leaving room
    /// when it is shorter, the way an HTML text area does. Answers the
    /// band's height. Lays the leaf out only when something moved it.
    pub fn layout(&mut self, pad: f32, top: f32, w: f32, page_h: f32, fixed: Option<f32>) -> f32 {
        let share = (page_h * BAND_SHARE).max(1.0);
        let most = (page_h - top - MIN_ROWS_H).max(1.0);
        let inset = koru_draw::geometry::text_x(0u32, false);
        let x = pad - inset - MODE.content_x(w);
        let leaf_w = (w - x).max(1.0);
        if self.dirty || self.band.w != w || self.band.y != top || self.fixed != fixed {
            self.fixed = fixed;
            self.pad = pad;
            // Measured at the page's share so the content height is known,
            // then the band is cut to the content when it is shorter.
            Leaf::set_viewport(&mut self.leaf, leaf_w, share);
            let content = koru_editor::rowlayout::content_height(self.leaf.body());
            let natural = content.min(share).max(1.0);
            let h = match fixed {
                // **Never shorter than the band is on its own.** A drag may
                // give the note more room; it cannot take its words away —
                // a band squeezed to a sliver showed a line's top edge and
                // nothing to read (acceptance round 2).
                Some(h) => h.clamp(natural, most.max(natural)),
                None => natural,
            };
            if (h - share).abs() > 0.5 {
                Leaf::set_viewport(&mut self.leaf, leaf_w, h);
            }
            self.band = Rect::new(0.0, top, w, h);
            self.origin = (x, top);
            // Embedded in the page's chrome: the rows paint inside the band
            // and a picker may float anywhere on the page, which is what lets
            // a `/` menu opened on the last line open downwards over the rows
            // rather than being cut at the band.
            Editing::view_mut(&mut self.leaf).embedded = Some(koru_editor::Embedded {
                clip: Rect::new(0.0, 0.0, leaf_w, h),
                panels: Rect::new(-x, -top, w, page_h),
            });
            self.dirty = false;
        }
        self.band.h
    }

    /// The grip's hit lane: the band's bottom edge, full width, straddled
    /// the way a pane divider's is, plus the corner mark.
    pub fn grip_rect(&self) -> Rect {
        Rect::new(
            0.0,
            self.band.y + self.band.h - GRIP_HIT * 0.5,
            self.band.w,
            GRIP_HIT,
        )
    }

    /// The corner mark's square, inside the band's bottom-right corner on
    /// the page's margin — left of the scrollbar's lane, which keeps the
    /// edge.
    pub fn grip_mark(&self) -> Rect {
        Rect::new(
            self.band.w - self.pad - GRIP_MARK,
            self.band.y + self.band.h - GRIP_MARK - 2.0,
            GRIP_MARK,
            GRIP_MARK,
        )
    }

    /// The *open in a new tab* control: the band's top-right corner, in the
    /// column the header's `…` is in.
    pub fn open_rect(&self) -> Rect {
        Rect::new(
            self.band.w - self.pad - OPEN_ICON,
            self.band.y,
            OPEN_ICON,
            OPEN_ICON,
        )
    }

    /// Whether the caret is on a task's line — what decides whether typing
    /// here is something the rows below have to be told about.
    pub fn caret_on_task(&self, ws: &WorkspaceDoc) -> bool {
        self.leaf
            .caret_node()
            .is_some_and(|node| koru_core::tasks::is_task_node(ws, node).unwrap_or(false))
    }

    /// Whether a picker or menu of the note's is open — a press anywhere on
    /// the page is then the note's to answer, wherever it lands.
    pub fn overlay_open(&self) -> bool {
        Editing::view(&self.leaf).overlay.is_open()
    }

    /// Draw the note's open picker, after everything else the page draws,
    /// so it floats over the rows rather than being cut at the band.
    pub fn draw_panels(&mut self, scene: &mut Scene, root: Affine, ws: &WorkspaceDoc) {
        if self.band.h <= 0.0 {
            return;
        }
        let origin = root * Affine::translate((self.origin.0 as f64, self.origin.1 as f64));
        koru_editor::draw::floating_panels(scene, origin, &mut self.leaf);
        // The document's own menu — a right-click's — over the rows too.
        self.leaf.draw_menu(scene, origin, ws);
    }

    /// Whether the note's own menu (a right-click's) is open.
    #[cfg(test)]
    pub fn menu_open(&self) -> bool {
        self.leaf.menu_open()
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
        let origin = root * Affine::translate((self.origin.0 as f64, self.origin.1 as f64));
        View::draw(
            &mut self.leaf,
            scene,
            workspace,
            images,
            origin,
            caret_on && self.focused,
            (lx, ly),
        );
        // The thumb a document tab draws at its right edge, only when the
        // note is taller than the band. An embedded leaf leaves it out — its
        // viewport's edge is the host's — so the band draws it here, where
        // the leaf's right edge is the page's.
        koru_editor::draw::draw_scrollbar(
            scene,
            origin,
            self.leaf.body(),
            Editing::view(&self.leaf),
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

    /// A press while the note's picker is open: the leaf decides whether it
    /// chose something or dismissed the panel. No clamping — the press may
    /// be anywhere on the page.
    pub fn mouse_down_on_overlay(
        &mut self,
        ws: &mut WorkspaceDoc,
        x: f32,
        y: f32,
        ctrl: bool,
    ) -> bool {
        let (lx, ly) = self.local(x, y);
        self.dirty = true;
        Leaf::mouse_down(&mut self.leaf, ws, lx, ly, ctrl)
    }

    /// The caches every document leaf is fed when the library moves.
    pub fn take_fresh(&mut self) -> bool {
        std::mem::take(&mut self.fresh)
    }

    pub fn mouse_up(&mut self, ws: &mut WorkspaceDoc) -> bool {
        self.dirty = true;
        Leaf::mouse_up(&mut self.leaf, ws)
    }

    /// A right-click in the band: the editor's own menu over the note, as
    /// in a document tab. Drawn with the note's other panels, over the rows.
    pub fn context_menu(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32) -> bool {
        let (lx, ly) = self.local(x, y);
        self.dirty = true;
        View::context_menu(&mut self.leaf, ws, lx, ly)
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
