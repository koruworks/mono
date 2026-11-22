// SPDX-License-Identifier: MPL-2.0

//! **Koru's conformance adapter for `koru-app-shell`. `#[cfg(test)]`, so none
//! of it ships.**
//!
//! `MainView` hosted as **one** shell surface, per ADR-262. It is the compiler's
//! answer to "does the shipped boundary carry a Koru tab", and the answer it
//! gives is a specific one:
//!
//! **It can, because the adapter owns the domain and the boundary never sees
//! it.** `View::key`, `View::mouse_down`, `View::adopt` and `View::reseat` all
//! take a live `&mut WorkspaceDoc`. A shell forbidden from naming `koru-core`
//! could never pass one — and does not have to, because a `Surface` method
//! takes only `&mut self`. The workspace lives *inside* this adapter, beside
//! the `MainView` that borrows it. That is exactly ADR-262's conclusion
//! ("`MainView` remains Koru's one host surface … Koru's domain tab dispatch,
//! workspace borrowing and restoration never leave `apps/koru`") turned into
//! something that either compiles or does not.
//!
//! **This is not the delegation.** SP-14 makes `MainView` the shell's surface in
//! the running product; what is here is the seam, kept compiled and kept
//! exercised so the boundary cannot drift away from its one Koru consumer in
//! between. The seam this app gains is a `#[cfg(test)] mod` line and nothing
//! else — `koru-app-shell` is already a real dependency for the tab manager.
//!
//! ## What is deliberately *not* here
//!
//! `View::restore`, `View::capture` and `View::on_workspace_switched` take
//! `&Workspace` and `&Settings`. They are not on the boundary and must not be:
//! the minimal contract forbids payload persistence and restoration hooks, and
//! a workspace switch is the host reseating its own surface after a banner
//! selection, not something the shell performs. This file drives them
//! **around** the boundary, which is what proves they still work when the shell
//! knows nothing about them.

use std::path::Path;

use koru_app_shell::{
    Banner, BannerChip, BannerKey, BannerSlot, CloseResponse, HostCx, PointerEvent, ProfileKey,
    SeatRefused, ShellHost, Surface, SurfaceCx, WindowKey, WorkspaceKey,
};
use koru_workspace_host::View;
use koru_draw::geometry::Rect;
use koru_window::{KeyState, RichKeyEvent};
use koru_workspace::Workspace;
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseButton;
use winit::window::CursorIcon;

use super::MainView;

/// `MainView` plus the domain it borrows. **This pairing is the whole finding.**
pub(super) struct KoruSurface {
    view: MainView,
    workspace: Workspace,
    images: koru_draw::image::ImageCache,
    /// Where the shell put us, kept so pointer coordinates can be reported the
    /// way `MainView`'s own tests report them.
    bounds: Rect,
    /// The last thing the shell asked us to pop out. Koru's pop-out is an
    /// in-process sibling window over the live workspace, so it is an
    /// `OpenWindow` and the host queues the reason itself.
    pub(super) popped_out: Option<koru_core::NodeId>,
}

impl KoruSurface {
    fn new() -> Self {
        let ws =
            koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "conformance")
                .expect("an in-memory workspace");
        let id = ws.id().expect("a workspace id");
        let mut view = MainView::new();
        view.set_pane_context(
            Vec::new(),
            koru_core::ProfileId::new(),
            Vec::new(),
            Some(id),
        );
        Self {
            view,
            workspace: Workspace::new(ws),
            images: koru_draw::image::ImageCache::new(),
            bounds: Rect::new(0.0, 0.0, 0.0, 0.0),
            popped_out: None,
        }
    }

    pub(super) fn document(&mut self, title: &str) -> koru_core::NodeId {
        let doc = koru_workspace::document(&mut self.workspace.ws, title).expect("a document");
        self.view.open_node(&self.workspace.ws, doc);
        doc
    }

    pub(super) fn view(&self) -> &MainView {
        &self.view
    }

    /// A workspace switch, driven the way the host drives one: **around** the
    /// boundary, because `reseat` needs a document the shell may not name.
    ///
    /// `reseat` rather than `adopt`, and it is required with no default for a
    /// reason worth restating here: every `ChangeTracker` a view holds is a Loro
    /// subscription bound to one `LoroDoc`, and after a swap they are all
    /// subscribed to a document nobody holds.
    pub(super) fn switch_workspace(&mut self) {
        let ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "other")
            .expect("a second in-memory workspace");
        self.workspace = Workspace::new(ws);
        self.view.reseat(&self.workspace.ws);
        let settings = koru_workspace_host::Settings::default();
        self.view.on_workspace_switched(&self.workspace, &settings);
    }

    /// A remote edit, or another window's typing. Driven from `maintain`, which
    /// *is* on the boundary — so the shell needs no notion of "the document
    /// changed", only of "this surface had work to do".
    pub(super) fn adopt_now(&mut self) {
        self.view.adopt(&self.workspace.ws);
    }

    pub(super) fn workspace_mut(&mut self) -> &mut Workspace {
        &mut self.workspace
    }
}

impl Surface for KoruSurface {
    fn set_bounds(&mut self, _cx: &mut SurfaceCx<'_>, bounds: Rect) -> bool {
        self.bounds = bounds;
        // `MainView` still cuts its own window: it owns the caption and both
        // side columns. So the shell's content rect is its *whole* viewport
        // here, which is what "hosted as one surface" means and what a
        // per-tab hosting would have had to unpick.
        self.view.set_viewport(bounds.w, bounds.h);
        true
    }

    fn draw(&mut self, _cx: &mut SurfaceCx<'_>, scene: &mut Scene, root: Affine, _bounds: Rect) {
        // The real signature, unchanged: a `Scene`, the workspace, the image
        // cache, a root transform, the caret phase and the pointer. Two of
        // those six are domain values the shell has never heard of, and they
        // come from this adapter's own fields.
        self.view.draw(
            scene,
            &self.workspace,
            &self.images,
            root,
            true,
            (self.bounds.w * 0.5, self.bounds.h * 0.5),
        );
    }

    fn invalidate(&mut self) {
        View::invalidate(&mut self.view);
    }

    fn key(&mut self, cx: &mut SurfaceCx<'_>, event: RichKeyEvent<'_>) -> bool {
        // Koru edits text: it wants presses only, and reads three of the seven
        // fields. A terminal reads all seven. One rich event serves both; the
        // narrower triple would have served only this one.
        if event.state != KeyState::Pressed {
            return false;
        }
        let _ = cx;
        self.view.key(
            &mut self.workspace.ws,
            event.logical_key,
            event.text,
            event.modifiers,
        )
    }

    fn pointer(&mut self, cx: &mut SurfaceCx<'_>, event: PointerEvent) -> bool {
        let _ = cx;
        match event {
            PointerEvent::Moved { x, y, mods } => {
                self.view.set_mods(mods);
                self.view.mouse_move(x, y)
            }
            PointerEvent::Pressed {
                x,
                y,
                button: MouseButton::Left,
                mods,
                click_count,
            } => {
                self.view.set_mods(mods);
                let primary = mods.control_key() || mods.super_key();
                if click_count >= 2 {
                    self.view.mouse_double_click(&self.workspace.ws, x, y)
                } else {
                    self.view.mouse_down(&mut self.workspace.ws, x, y, primary)
                }
            }
            PointerEvent::Pressed {
                x,
                y,
                button: MouseButton::Right,
                ..
            } => View::context_menu(&mut self.view, &mut self.workspace.ws, x, y),
            PointerEvent::Pressed { .. } => false,
            // One enum collapses `wants_drag`: a surface that does not drag
            // answers `false`, and there is no flag for the shell to consult.
            PointerEvent::Dragged { x, y, .. } => self.view.mouse_move(x, y),
            PointerEvent::Released { .. } => self.view.mouse_up(&mut self.workspace.ws),
            PointerEvent::Wheel { lines, .. } => {
                self.view
                    .scroll_by(winit::event::MouseScrollDelta::LineDelta(0.0, lines));
                true
            }
            // The leave `CursorLeft` never produced. Koru clears a hover on the
            // next move today, so there is nothing owed here yet — the seam is
            // what matters.
            PointerEvent::Left => self.view.preview_pointer(-1.0, -1.0),
        }
    }

    fn cursor_icon(&self, _x: f32, _y: f32) -> CursorIcon {
        // A question about the view's own state, not about a point: the move
        // has already told it where the pointer is.
        View::cursor_icon(&self.view)
    }

    fn dropped_file(&mut self, _cx: &mut SurfaceCx<'_>, _path: &Path) -> bool {
        // Koru imports a dropped file through the shell's own path today
        // (`PaneRequest`), which is host work. The seam is here and the
        // routing stays in `apps/koru`.
        false
    }

    fn window_focus_changed(&mut self, _cx: &mut SurfaceCx<'_>, _focused: bool) -> bool {
        false
    }

    fn close_requested(&mut self, _cx: &mut SurfaceCx<'_>) -> CloseResponse {
        if self.view.request_close() {
            CloseResponse::Veto
        } else {
            CloseResponse::Close
        }
    }

    fn maintenance_deadline(&self) -> Option<std::time::Instant> {
        View::maintenance_deadline(&self.view)
    }

    fn maintain(&mut self, _cx: &mut SurfaceCx<'_>, now: std::time::Instant) -> bool {
        // The one place a workspace is needed on a clock. It comes from
        // this adapter, not from an argument — and the clock has no
        // `TextEngine`, which is why `SurfaceCx::engine` is an `Option`.
        let changed = self.view.maintain(&mut self.workspace.ws, now);
        // A pop-out is a *request* here, drained by the asking, exactly as
        // `take_wants_popout` documents. The shell learns only "a window".
        if let Some(node) = self.view.take_wants_popout() {
            self.popped_out = Some(node);
        }
        changed
    }

    fn animating(&self) -> bool {
        View::animating(&self.view)
    }
}

/// Koru as a `ShellHost`: **one surface per window, no shell tabs.**
///
/// The tab strip a Koru window shows is `MainView`'s own `TabStrip<Tab>` —
/// which is now the shell's generic one, with Koru's rules on the element —
/// because a Koru tab needs a tab-private workspace substitution the boundary
/// cannot carry. So this host never records `OpenTab`, and a pop-out is
/// `OpenWindow`.
pub(super) struct Koru {
    /// The documents this host has asked to pop out, in the order it asked.
    /// **The shell carries no intent**, so the correlation is the host's — and
    /// that is cheaper than teaching the boundary a `NodeId`.
    pending_windows: Vec<koru_core::NodeId>,
    pub(super) seated: usize,
    pub(super) rollbacks: usize,
    pub(super) refuse: bool,
    pub(super) slots: Vec<BannerSlot>,
    pub(super) profile: ProfileKey,
    pub(super) workspace: WorkspaceKey,
}

impl Koru {
    pub(super) fn new() -> Self {
        Self {
            pending_windows: Vec::new(),
            seated: 0,
            rollbacks: 0,
            refuse: false,
            slots: Vec::new(),
            profile: ProfileKey::from_uuid(koru_core::ProfileId::new().as_uuid()),
            workspace: WorkspaceKey::from_uuid(koru_core::WorkspaceId::new().as_uuid()),
        }
    }

    pub(super) fn queue_popout(&mut self, node: koru_core::NodeId) {
        self.pending_windows.push(node);
    }
}

impl ShellHost for Koru {
    /// Koru's real window has background threads; this adapter has none, and
    /// the shell forwards whatever the host declares (SP-10).
    type Event = ();

    fn floor_surface(&mut self, _cx: &mut HostCx<'_>, _window: WindowKey) -> Box<dyn Surface> {
        Box::new(KoruSurface::new())
    }

    fn banner(&self, _window: WindowKey) -> Banner {
        Banner {
            // Already localized, and the chrome never learns what it says.
            profile: Some(
                BannerChip::new("Steven", BannerKey::Profile(self.profile))
                    .with_mark("S")
                    .with_tooltip("Switch profile"),
            ),
            workspace: Some(BannerChip::new(
                "Notes",
                BannerKey::Workspace(self.workspace),
            )),
            settings: true,
        }
    }

    fn slot_pressed(&mut self, _cx: &mut HostCx<'_>, _window: WindowKey, slot: BannerSlot) -> bool {
        // A press answers *which slot*. Turning that into a workspace switch is
        // this host's, and it performs it by reseating its own surface — which
        // is why no `Surface::reseat` exists.
        self.slots.push(slot);
        true
    }

    fn window_key(
        &mut self,
        cx: &mut HostCx<'_>,
        window: WindowKey,
        _event: RichKeyEvent<'_>,
    ) -> bool {
        // Koru's pop-out chord, standing in for the drag past the tear-off
        // threshold. The host has already queued *what* is being popped out;
        // the shell is only asked for a window.
        if self.pending_windows.is_empty() {
            return false;
        }
        cx.command(window, koru_app_shell::ShellAction::OpenWindow);
        true
    }

    fn seat_window(
        &mut self,
        _cx: &mut HostCx<'_>,
        _window: WindowKey,
    ) -> Result<Box<dyn Surface>, SeatRefused> {
        if self.refuse {
            return Err(SeatRefused("no render surface".into()));
        }
        let node = self
            .pending_windows
            .pop()
            .ok_or_else(|| SeatRefused("nothing was queued".into()))?;
        self.seated += 1;
        let mut surface = KoruSurface::new();
        // A popped-out document window is a sibling over the live workspace.
        let _ = node;
        surface.document("popped out");
        Ok(Box::new(surface))
    }

    fn window_open_failed(&mut self, _cx: &mut HostCx<'_>) {
        // The source tab was never touched, so rolling back is dropping the
        // reason we queued.
        self.pending_windows.pop();
        self.rollbacks += 1;
    }
}

#[cfg(test)]
mod tests;
