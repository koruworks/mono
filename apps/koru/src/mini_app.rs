// SPDX-License-Identifier: MPL-2.0

//! The bounded task/document composition. Child identity is independent of
//! placement: resizing and changing arrangement only change `Regions`.

use koru_core::{DocumentId, NodeId, WorkspaceId, schema::WorkspaceDoc};
use koru_draw::{ImageCache, geometry::Rect, scene, theme};
use koru_editor::Editing;
use koru_workspace::Workspace;
use koru_workspace_host::View;
use vello::{Scene, kurbo::Affine};
use winit::{
    event::MouseScrollDelta,
    keyboard::{Key, ModifiersState},
    window::CursorIcon,
};

use crate::{leaf::Leaf, panes::explore::rail::RailTarget, tasks_view::TasksView};

pub(crate) mod artifact;
mod chrome;
mod declaration;
pub(crate) mod pin;
mod refinement;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Region {
    Tasks,
    Document,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Regions {
    pub tasks: Rect,
    pub document: Rect,
}

impl Regions {
    fn solve(w: f32, h: f32, swapped: bool, shown: Region) -> Self {
        let whole = Rect::new(0.0, 0.0, w.max(0.0), h.max(0.0));
        // Two normal document margins plus the existing task capture control
        // width are the minimum useful pair; no child is discarded below it.
        let narrow = w < 2.0 * theme::CONTENT_MIN_W;
        if narrow {
            let hidden = Rect::new(0.0, 0.0, 0.0, 0.0);
            return match shown {
                Region::Tasks => Self {
                    tasks: whole,
                    document: hidden,
                },
                Region::Document => Self {
                    tasks: hidden,
                    document: whole,
                },
            };
        }
        let half = w * 0.5;
        let left = Rect::new(0.0, 0.0, half, h);
        let right = Rect::new(half, 0.0, w - half, h);
        if swapped {
            Self {
                tasks: right,
                document: left,
            }
        } else {
            Self {
                tasks: left,
                document: right,
            }
        }
    }

    pub fn rect(self, region: Region) -> Rect {
        match region {
            Region::Tasks => self.tasks,
            Region::Document => self.document,
        }
    }

    pub fn local(self, region: Region, x: f32, y: f32) -> (f32, f32) {
        let rect = self.rect(region);
        (x - rect.x, y - rect.y)
    }

    pub fn transform(self, region: Region, root: Affine) -> Affine {
        let rect = self.rect(region);
        root * Affine::translate((rect.x as f64, rect.y as f64))
    }

    pub fn hit(self, x: f32, y: f32) -> Option<Region> {
        [Region::Tasks, Region::Document]
            .into_iter()
            .find(|&region| {
                let rect = self.rect(region);
                rect.w > 0.0 && rect.h > 0.0 && rect.contains(x, y)
            })
    }
}

pub(crate) struct MiniApp {
    pub workspace: WorkspaceId,
    pub surface: u64,
    live_id: uuid::Uuid,
    revision: u64,
    tasks_viewport: Option<(f32, f32)>,
    generation: u64,
    reactivating: bool,
    window_focused: bool,
    pub pending: Option<(koru_workspace_host::ChildDocumentRequest, Option<NodeId>)>,
    pub tasks: TasksView,
    pub document: Option<DocumentId>,
    leaf: Option<Leaf>,
    pub regions: Regions,
    focus: Region,
    pointer: (f32, f32),
    viewport: (f32, f32),
    swapped: bool,
    chrome: chrome::Chrome,
    pub artifact: artifact::SaveState,
    pub pin: pin::State,
}

impl MiniApp {
    pub fn new(workspace: WorkspaceId, surface: u64, target: RailTarget) -> Self {
        Self {
            workspace,
            surface,
            live_id: uuid::Uuid::new_v4(),
            revision: 0,
            tasks_viewport: None,
            generation: 0,
            reactivating: false,
            window_focused: true,
            pending: None,
            tasks: TasksView::new(target).collection_only(),
            document: None,
            leaf: None,
            regions: Regions::solve(0.0, 0.0, false, Region::Tasks),
            focus: Region::Tasks,
            pointer: (0.0, 0.0),
            viewport: (0.0, 0.0),
            swapped: false,
            chrome: chrome::Chrome::new(),
            artifact: artifact::SaveState::default(),
            pin: pin::State::default(),
        }
    }

    pub fn fixture(workspace: WorkspaceId, surface: u64) -> Result<Self, &'static str> {
        let declaration = declaration::Declaration::parse(declaration::FIXTURE.as_bytes())?;
        Ok(Self::with_declaration(workspace, surface, declaration))
    }
    pub fn from_declaration(
        workspace: WorkspaceId,
        surface: u64,
        declaration: koru_ai_core::mini_apps::Declaration,
    ) -> Result<Self, &'static str> {
        let declaration = declaration::Declaration::from_shared(declaration)?;
        Ok(Self::with_declaration(workspace, surface, declaration))
    }
    fn with_declaration(
        workspace: WorkspaceId,
        surface: u64,
        declaration: declaration::Declaration,
    ) -> Self {
        let mut app = Self::new(workspace, surface, declaration.target);
        app.swapped = declaration.swapped;
        if let Some(document) = declaration.document {
            app.request_document(document, None);
        }
        app
    }

    pub fn request_document(&mut self, document: DocumentId, node: Option<NodeId>) {
        self.reactivating = false;
        self.generation = self.generation.wrapping_add(1);
        self.pending = Some((
            koru_workspace_host::ChildDocumentRequest {
                workspace: self.workspace,
                surface: self.surface,
                generation: self.generation,
                document,
            },
            node,
        ));
    }

    pub fn reactivate(&mut self) {
        if self.pending.is_none()
            && let Some(document) = self.document
        {
            self.request_document(document, None);
            self.reactivating = true;
        }
    }

    pub fn leaf(&self) -> Option<&Leaf> {
        self.leaf.as_ref()
    }
    pub fn leaf_mut(&mut self) -> Option<&mut Leaf> {
        self.leaf.as_mut()
    }
    pub fn focused_leaf(&mut self) -> Option<&mut Leaf> {
        self.document_focused()
            .then_some(self.leaf.as_mut())
            .flatten()
    }
    pub fn document_focused(&self) -> bool {
        self.focus == Region::Document
            && !self.chrome.search_focus
            && !self.header_focused()
            && self.chrome.menu.is_none()
    }
    pub fn owns(&self, ws: &WorkspaceDoc) -> bool {
        ws.id() == Some(self.workspace)
            && self.document.is_some()
            && self.document == ws.own_document_id()
    }
    pub fn focus_region(&mut self, region: Region) {
        self.focus = region;
        self.chrome.search_focus = false;
        self.chrome.header_focus = None;
        self.chrome.menu = None;
        self.layout();
    }

    pub fn land(&mut self, ws: &WorkspaceDoc, document: DocumentId) -> bool {
        if ws.id() != Some(self.workspace) || ws.own_document_id() != Some(document) {
            return false;
        }
        let Some(root) = koru_workspace::document_root(ws) else {
            return false;
        };
        if self.document != Some(document) {
            self.leaf = Some(Leaf::document(ws, root));
            self.document = Some(document);
            self.revision += 1;
        }
        if !self.reactivating {
            self.focus = Region::Document;
        }
        self.reactivating = false;
        self.layout();
        true
    }

    fn layout(&mut self) {
        self.regions = self.chrome.layout(self.viewport, self.swapped, self.focus);
        let tasks = self.regions.tasks;
        if tasks.w > 0.0 && self.tasks_viewport != Some((tasks.w, tasks.h)) {
            View::set_viewport(&mut self.tasks, tasks.w, tasks.h);
            self.tasks_viewport = Some((tasks.w, tasks.h));
        }
        let doc = self.regions.document;
        if doc.w > 0.0
            && let Some(leaf) = &mut self.leaf
        {
            if leaf.view().viewport != (doc.w, doc.h) {
                leaf.set_viewport(doc.w, doc.h);
            }
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.chrome.input.is_dragging()
            || self.tasks.holding_rows()
            || self.leaf.as_ref().is_some_and(|leaf| {
                let view = leaf.view();
                view.dragging
                    || view.drag_row.is_some()
                    || view.drag_field
                    || view.scroll_drag.is_some()
                    || view.image_resize.is_some()
            })
    }

    pub fn adopt_cheap(&mut self, ws: &WorkspaceDoc) {
        View::adopt(&mut self.tasks, ws);
        if self.owns(ws)
            && let Some(leaf) = &mut self.leaf
        {
            leaf.body_mut().defer_pending(ws);
        }
    }

    pub fn catch_up(&mut self, ws: &WorkspaceDoc) -> bool {
        let mut changed = self.regions.tasks.w > 0.0 && self.tasks.catch_up(ws);
        if self.regions.document.w > 0.0
            && self.owns(ws)
            && let Some(leaf) = &mut self.leaf
            && leaf.body().is_stale()
        {
            leaf.catch_up(ws);
            changed = true;
        }
        changed
    }
}

impl View for MiniApp {
    fn set_window_focused(&mut self, focused: bool) {
        self.window_focused = focused;
    }
    fn ime_target(&self) -> Option<koru_workspace_host::ImeTarget> {
        use koru_workspace_host::{ImeTarget, ImeTargetId};
        if self.chrome.menu.is_some() && !self.chrome.search_focus {
            return None;
        }
        if self.chrome.search_focus {
            return Some(ImeTarget {
                id: ImeTargetId {
                    owner: self.surface | (1 << 63),
                    control: 0,
                },
                cursor_area: self.chrome.input.ime_cursor_area(self.chrome.search),
            });
        }
        let leaf = self.leaf.as_ref().filter(|_| self.document_focused())?;
        let control = leaf.native_ime_target()?;
        let region = self.regions.document;
        Some(ImeTarget {
            id: ImeTargetId {
                owner: leaf.native_ime_owner(),
                control,
            },
            cursor_area: leaf
                .native_ime_rect()
                .map(|rect| Rect::new(region.x + rect.x, region.y + rect.y, rect.w, rect.h)),
        })
    }

    fn ime(
        &mut self,
        ws: &mut WorkspaceDoc,
        target: koru_workspace_host::ImeTargetId,
        event: &winit::event::Ime,
    ) -> bool {
        if target.owner == self.surface | (1 << 63) && target.control == 0 {
            if matches!(event, winit::event::Ime::Disabled) {
                return self.chrome.input.ime_cancel(&mut self.chrome.engine);
            }
            if View::ime_target(self).is_none_or(|current| current.id != target) {
                return false;
            }
            let changed = self.chrome.input.ime(&mut self.chrome.engine, event);
            self.chrome.changed_query(self.workspace);
            return changed;
        }
        if !self.owns(ws) {
            return false;
        }
        let current = View::ime_target(self).map(|target| target.id);
        let Some(leaf) = &mut self.leaf else {
            return false;
        };
        if leaf.native_ime_owner() != target.owner {
            return false;
        }
        if matches!(event, winit::event::Ime::Disabled) {
            return leaf.native_ime_cancel(ws);
        }
        current == Some(target) && leaf.native_ime(ws, event)
    }

    fn key(
        &mut self,
        ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        if self.chrome_key(key, text, mods) {
            return true;
        }
        match self.focus {
            Region::Tasks => View::key(&mut self.tasks, ws, key, text, mods),
            Region::Document if self.owns(ws) => self
                .leaf
                .as_mut()
                .is_some_and(|leaf| leaf.key(ws, key, text, mods)),
            _ => false,
        }
    }

    fn mouse_down(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        if self.chrome_down(x, y) {
            return true;
        }
        let Some(region) = self.regions.hit(x, y) else {
            return false;
        };
        self.focus = region;
        let (x, y) = self.regions.local(region, x, y);
        match region {
            Region::Tasks => View::mouse_down(&mut self.tasks, ws, x, y, ctrl),
            Region::Document if self.owns(ws) => self
                .leaf
                .as_mut()
                .is_some_and(|leaf| leaf.mouse_down(ws, x, y, ctrl)),
            _ => false,
        }
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.pointer = (x, y);
        if self.chrome.input.is_dragging() {
            return self
                .chrome
                .input
                .drag(&mut self.chrome.engine, self.chrome.search, x);
        }
        if let Some(menu) = &mut self.chrome.menu {
            let hover = menu.item_at(self.viewport, x, y);
            let changed = hover != menu.hover;
            menu.hover = hover;
            return changed;
        }
        let region = if self.is_dragging() {
            Some(self.focus)
        } else {
            self.regions.hit(x, y)
        };
        let Some(region) = region else {
            return false;
        };
        let (x, y) = self.regions.local(region, x, y);
        match region {
            Region::Tasks => View::mouse_move(&mut self.tasks, x, y),
            Region::Document => self.leaf.as_mut().is_some_and(|leaf| leaf.mouse_move(x, y)),
        }
    }

    fn mouse_up(&mut self, ws: &mut WorkspaceDoc) -> bool {
        if self.chrome.input.is_dragging() {
            self.chrome.input.release();
            return true;
        }
        match self.focus {
            Region::Tasks => View::mouse_up(&mut self.tasks, ws),
            Region::Document if self.owns(ws) => {
                self.leaf.as_mut().is_some_and(|leaf| leaf.mouse_up(ws))
            }
            _ => false,
        }
    }

    fn mouse_double_click(&mut self, ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        if self.chrome.search.w > 0.0 && self.chrome.search.contains(x, y) {
            return self.chrome.input.double_click(
                &mut self.chrome.engine,
                self.chrome.search,
                x,
                y,
            );
        }
        let Some(region) = self.regions.hit(x, y) else {
            return false;
        };
        let (x, y) = self.regions.local(region, x, y);
        match region {
            Region::Tasks => View::mouse_double_click(&mut self.tasks, ws, x, y),
            Region::Document if self.owns(ws) => self
                .leaf
                .as_mut()
                .is_some_and(|leaf| leaf.mouse_double_click(ws, x, y)),
            _ => false,
        }
    }

    fn context_menu(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32) -> bool {
        let Some(region) = self.regions.hit(x, y) else {
            return false;
        };
        self.focus_region(region);
        let (x, y) = self.regions.local(region, x, y);
        match region {
            Region::Tasks => View::context_menu(&mut self.tasks, ws, x, y),
            Region::Document if self.owns(ws) => self
                .leaf
                .as_mut()
                .is_some_and(|leaf| View::context_menu(leaf, ws, x, y)),
            _ => false,
        }
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        match self.regions.hit(self.pointer.0, self.pointer.1) {
            Some(Region::Tasks) => View::scroll_by(&mut self.tasks, delta),
            Some(Region::Document) => {
                if let Some(leaf) = &mut self.leaf {
                    leaf.scroll_by(delta);
                }
            }
            None => {}
        }
    }
    fn set_viewport(&mut self, w: f32, h: f32) {
        self.viewport = (w, h);
        self.layout();
    }
    fn set_mods(&mut self, mods: ModifiersState) {
        View::set_mods(&mut self.tasks, mods);
        if let Some(leaf) = &mut self.leaf {
            View::set_mods(leaf, mods);
        }
    }
    fn cursor_icon(&self) -> CursorIcon {
        match self.regions.hit(self.pointer.0, self.pointer.1) {
            Some(Region::Tasks) => View::cursor_icon(&self.tasks),
            Some(Region::Document) => self
                .leaf
                .as_ref()
                .map_or(CursorIcon::Default, View::cursor_icon),
            _ => CursorIcon::Default,
        }
    }
    fn adopt(&mut self, ws: &WorkspaceDoc) {
        self.adopt_cheap(ws);
        self.catch_up(ws);
    }
    fn invalidate(&mut self) {
        self.tasks_viewport = None;
        View::invalidate(&mut self.tasks);
        if let Some(leaf) = &mut self.leaf {
            View::invalidate(leaf);
        }
    }
    fn reseat(&mut self, ws: &WorkspaceDoc) {
        if self.owns(ws)
            && let Some(leaf) = &mut self.leaf
        {
            View::reseat(leaf, ws);
        }
    }
    fn maintenance_deadline(&self) -> Option<std::time::Instant> {
        [
            self.chrome.deadline(),
            self.leaf
                .as_ref()
                .filter(|_| self.window_focused && self.regions.document.w > 0.0)
                .and_then(View::maintenance_deadline),
        ]
        .into_iter()
        .flatten()
        .min()
    }
    fn maintain(&mut self, ws: &mut WorkspaceDoc, now: std::time::Instant) -> bool {
        let mut changed = self.chrome.poll(self.workspace, now);
        if self.window_focused
            && self.regions.document.w > 0.0
            && self.owns(ws)
            && let Some(leaf) = &mut self.leaf
        {
            changed |= View::maintain(leaf, ws, now);
        }
        changed
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        workspace: &Workspace,
        images: &ImageCache,
        root: Affine,
        caret_on: bool,
        pointer: (f32, f32),
    ) {
        scene::fill(
            scene,
            root,
            theme::p().bg,
            &scene::rect(0.0, 0.0, self.viewport.0, self.viewport.1),
        );
        self.draw_surface(scene, workspace, images, root, caret_on, pointer);
    }
}

#[cfg(test)]
mod tests;
