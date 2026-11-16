// SPDX-License-Identifier: MPL-2.0

//! **The Lab's two windows**: the Lab itself and, while it is open, the F12
//! inspector's panel, on the shared multi-window runner.
//!
//! [`LabWindows`] owns no story of its own. The Lab's window is
//! [`shell::Lab`], driven exactly as it was when it was the only window; the
//! inspector is the workspace host's own, reached through its hosted facade.
//! This type is the wiring between them and the runner:
//!
//! ```text
//! an event of the Lab's window    -> the inspector first; the Lab only if it was left it
//! a frame of the Lab's window     -> measured while the Lab draws; the highlight on top
//! an event of the panel's window  -> the inspector; the Lab never sees it
//! a frame of the panel's window   -> the inspector's panel
//! ```
//!
//! ## Lifetimes
//!
//! F12 in either window opens and closes the panel. Closing the panel leaves
//! the Lab open and gives it the keyboard back. Closing the Lab ends the
//! process, and the panel with it.
//!
//! ## What is not here
//!
//! Nothing is remembered between launches. The panel's size, position and
//! pin live in the facade's memory for this process only, and the Lab's size
//! is [`super::placement`]'s. No file is read or written for either window.
//!
//! Nothing runs on a clock. An inspector that is closed costs one test per
//! frame and per event; a screen reader that is not attached costs one
//! drained mailbox per window when the loop settles.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_window::{ClosingWindowState, ImeEvent, MultiWindowApp, RichKeyEvent, WindowApp};
use koru_window::{WindowBridge, WindowKey, WindowRequest};
use koru_workspace_host::accessibility::{PaneAccessibility, Resolved, Slots};
use koru_workspace_host::hosted_inspector::{HostedInspector, PanelEvent};
use std::sync::Arc;
use std::time::Instant;
use vello::Scene;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{Key as KeyboardKey, ModifiersState};
use winit::window::{CursorIcon, ResizeDirection, Window, WindowAttributes, WindowId};

use super::shell::Lab;

/// The Lab's own window. It exists for the whole of the process.
pub(crate) const LAB: WindowKey = WindowKey(1);
/// The inspector's panel. It exists while the inspector is open.
pub(crate) const PANEL: WindowKey = WindowKey(2);

/// The name the Lab's window has at the top of the inspector's tree. A
/// technical identifier, like the element names beneath it.
const OWNER: &str = "UI Lab window";

/// The one thing another thread, or this one, asks of the loop: settle. A
/// screen reader attached or asked for something, or one window changed what
/// the other shows. It names no window; settling asks each what it owes.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Settle;

/// What the wiring keeps for one window between events.
#[derive(Default)]
struct Seat {
    /// The window's native accessibility state, from before it is first shown
    /// until it is dropped. `None` before the runner has created the window.
    access: Option<PaneAccessibility>,
    /// Device pixels per logical pixel, as the window last reported it.
    scale: f64,
    /// The client size of the last frame, in logical pixels.
    size: (f32, f32),
    /// The window shows something out of date and no event of its own has
    /// said so: the other window changed it.
    stale: bool,
    /// The pointer is on the window's resize border, where a press is the
    /// runner's.
    on_border: bool,
}

impl Seat {
    fn new() -> Self {
        Self {
            scale: 1.0,
            ..Self::default()
        }
    }

    fn take_stale(&mut self) -> bool {
        std::mem::take(&mut self.stale)
    }
}

/// The Lab and its inspector, as the multi-window runner sees them.
pub(crate) struct LabWindows {
    lab: Lab,
    inspector: HostedInspector,
    /// The windows that should exist: the Lab, then the panel while the
    /// inspector is open. The runner opens and drops windows to match.
    windows: Vec<WindowKey>,
    lab_seat: Seat,
    panel_seat: Seat,
    slots: Slots,
    proxy: Option<EventLoopProxy<Settle>>,
    /// A [`Settle`] is already on its way, so another is not sent.
    settle_queued: bool,
    /// The event the runner is about to hand the Lab was the inspector's: a
    /// pick, or F12 itself. The Lab's content must not act on it.
    withheld: bool,
    /// What the panel asks of its window, taken by the runner inside the
    /// event that asked.
    panel_request: Option<WindowRequest>,
    /// Whether the panel's window is above the others, as the runner last
    /// said.
    panel_on_top: bool,
    /// The monitors when the inspector was opened, so a remembered panel
    /// position is only used where it can be reached.
    monitors: Vec<(i32, i32, i32, i32)>,
    /// The keyboard goes back to the Lab's window: the panel just closed, or
    /// a tree row asked to see what it measures.
    refocus_lab: bool,
    /// A screen reader's requests on the Lab's controls, waiting for the
    /// next frame: acting on one needs the loop's text engine, which only an
    /// event or a frame is handed.
    requests: Vec<Resolved>,
}

impl LabWindows {
    pub(crate) fn new(lab: Lab) -> Self {
        Self {
            lab,
            inspector: HostedInspector::in_memory(),
            windows: vec![LAB],
            lab_seat: Seat::new(),
            panel_seat: Seat::new(),
            slots: Slots::default(),
            proxy: None,
            settle_queued: false,
            withheld: false,
            panel_request: None,
            panel_on_top: false,
            monitors: Vec::new(),
            refocus_lab: false,
            requests: Vec::new(),
        }
    }

    /// Ask the loop to settle once, so a window that had no event of its own
    /// is asked whether it owes a frame.
    fn queue_settle(&mut self) {
        if self.settle_queued {
            return;
        }
        if let Some(proxy) = &self.proxy {
            self.settle_queued = proxy.send_event(Settle).is_ok();
        }
    }

    /// Make the window list say what the inspector is: the panel's window is
    /// wanted exactly while it is open.
    fn follow_inspector(&mut self) {
        let open = self.inspector.is_open();
        let listed = self.windows.contains(&PANEL);
        if open && !listed {
            self.windows.push(PANEL);
        } else if !open && listed {
            self.windows.retain(|key| *key != PANEL);
            self.panel_request = None;
            // Closing the panel must not leave the keyboard nowhere.
            self.refocus_lab = true;
            self.lab_seat.stale = true;
        }
    }

    /// The Lab's client size in logical pixels: the last frame's, or the
    /// size it was asked to open at before there has been one.
    fn lab_size(&self) -> (f32, f32) {
        self.lab.frame_size().unwrap_or(self.lab_seat.size)
    }

    /// One event of the Lab's window, shown to the inspector before the Lab.
    /// Returns whether the event opened the inspector.
    fn source_event(&mut self, event: &WindowEvent) -> bool {
        let size = self.lab_size();
        let out = self
            .inspector
            .source_event(event, self.lab_seat.scale, size);
        self.withheld = out.consumed;
        if out.opened {
            // A press the Lab was in the middle of is over: the inspector
            // starts by taking this window's input.
            WindowApp::mouse_up(&mut self.lab);
        }
        self.lab_seat.stale |= out.redraw_source;
        if out.redraw_panel {
            self.panel_seat.stale = true;
            self.queue_settle();
        }
        self.follow_inspector();
        out.opened
    }

    /// One event of the panel's window. All of it is the inspector's.
    fn panel_event(&mut self, event: &WindowEvent) {
        let out = self.inspector.panel_event(event, self.panel_seat.scale);
        self.after_panel(out);
    }

    /// Do what the inspector asked after something happened in its panel.
    fn after_panel(&mut self, out: PanelEvent) {
        if out.drag_window {
            self.panel_request = Some(WindowRequest::Drag);
        } else if self.inspector.is_open() && self.inspector.panel_pinned() != self.panel_on_top {
            self.panel_request = Some(WindowRequest::TogglePin);
        }
        self.refocus_lab |= out.reveal_source;
        self.panel_seat.stale |= out.redraw_panel;
        if out.redraw_source || out.reveal_source {
            self.lab_seat.stale |= out.redraw_source;
            self.queue_settle();
        }
        self.follow_inspector();
    }

    /// The Lab's side of the accessibility bridge: publish when a screen
    /// reader is listening and something changed, and take its requests.
    /// Returns whether the Lab's window owes a frame.
    fn serve_lab(&mut self) -> bool {
        let Some(access) = self.lab_seat.access.as_mut() else {
            return false;
        };
        // The revision reads the story; nobody listening, nobody asks.
        if access.listening() {
            access.observe(self.lab.semantics_revision());
        }
        if access.poll().snapshot
            && let Some(semantics) =
                self.lab
                    .window_semantics(access.slot(), access.view_slot(), self.lab_seat.scale)
        {
            // The inspector reads a tree the window was publishing anyway;
            // it never has one built for its own sake.
            self.inspector.observe_source_semantics(&semantics.children);
            access.publish(&semantics);
        }
        let requests = access.resolve();
        if requests.is_empty() {
            return false;
        }
        self.requests.extend(requests);
        true
    }

    /// The panel's side of the bridge.
    fn serve_panel(&mut self) {
        let Some(access) = self.panel_seat.access.as_mut() else {
            return;
        };
        if self.inspector.take_panel_semantics_dirty() {
            access.invalidate_structure();
        }
        if access.poll().snapshot
            && let Some(semantics) = self.inspector.panel_semantics(
                access.slot(),
                self.panel_seat.scale,
                self.panel_seat.size,
            )
        {
            access.publish(&semantics);
        }
        for request in access.resolve() {
            let out = self
                .inspector
                .panel_action(request.occurrence.part, request.action);
            self.after_panel(out);
        }
    }

    fn seat(&mut self, key: WindowKey) -> &mut Seat {
        if key == PANEL {
            &mut self.panel_seat
        } else {
            &mut self.lab_seat
        }
    }

    /// Whether `key`'s window owes a frame nobody has asked for yet.
    fn take_stale(&mut self, key: WindowKey) -> bool {
        self.seat(key).take_stale()
    }

    /// The answer to an input method of the Lab's window: the Lab's own when
    /// the event was left to it, and nothing when the inspector took it.
    fn lab_input(&mut self, act: impl FnOnce(&mut Lab) -> bool) -> bool {
        let changed = !self.withheld && act(&mut self.lab);
        self.take_stale(LAB) || changed
    }
}

/// Every monitor's bounds in physical pixels, `left, top, right, bottom`.
fn monitors_of(window: &Window) -> Vec<(i32, i32, i32, i32)> {
    window
        .available_monitors()
        .map(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            (
                position.x,
                position.y,
                position.x.saturating_add(size.width as i32),
                position.y.saturating_add(size.height as i32),
            )
        })
        .collect()
}

impl MultiWindowApp for LabWindows {
    type Event = Settle;

    fn windows(&self) -> &[WindowKey] {
        &self.windows
    }

    fn window(&self, key: WindowKey) -> WindowAttributes {
        if key == PANEL {
            self.inspector
                .panel_window(self.lab_seat.scale, &self.monitors)
        } else {
            WindowApp::window(&self.lab)
        }
    }

    fn should_exit(&self) -> bool {
        WindowApp::should_exit(&self.lab)
    }

    fn attach(&mut self, proxy: EventLoopProxy<Settle>) {
        self.proxy = Some(proxy);
    }

    fn wake(&mut self, _event: Settle) -> bool {
        // The runner settles after this, which asks each window what it owes
        // in `maintain`. Answering `true` here would repaint both.
        self.settle_queued = false;
        false
    }

    fn seated(&mut self, key: WindowKey, _id: WindowId) {
        if key == PANEL {
            self.panel_on_top = self.inspector.panel_pinned();
        }
        if let Some(access) = self.seat(key).access.as_mut() {
            access.revealed();
        }
    }

    fn window_closing(&mut self, key: WindowKey, state: ClosingWindowState) {
        if let Some(mut access) = self.seat(key).access.take() {
            access.retire();
        }
        if key == PANEL {
            let size = state.inner_size.to_logical::<f32>(state.scale_factor);
            self.inspector.panel_closing(
                state.position.map(|position| (position.x, position.y)),
                (size.width, size.height),
                state.maximised,
                state.pinned,
            );
            self.panel_seat = Seat::new();
        }
    }

    fn close_requested(&mut self, key: WindowKey) {
        if key == PANEL {
            self.inspector.close();
            self.follow_inspector();
        } else {
            WindowApp::close_requested(&mut self.lab);
        }
    }

    fn take_activation_request(&mut self, key: WindowKey) -> bool {
        key == LAB && std::mem::take(&mut self.refocus_lab)
    }

    fn take_window_request(&mut self, key: WindowKey) -> Option<WindowRequest> {
        if key == PANEL {
            self.panel_request.take()
        } else {
            WindowApp::take_window_request(&mut self.lab)
        }
    }

    fn wants_drag(&self, key: WindowKey) -> bool {
        key == LAB && !self.withheld && WindowApp::wants_drag(&self.lab)
    }

    fn is_caption(&self, key: WindowKey, x: f32, y: f32) -> bool {
        // The panel drags by its own title, through `take_window_request`.
        key == LAB && !self.withheld && WindowApp::is_caption(&self.lab, x, y)
    }

    fn cursor_icon(&self, key: WindowKey, x: f32, y: f32) -> CursorIcon {
        if key == PANEL || self.inspector.wants_arrow_cursor() {
            CursorIcon::Default
        } else {
            WindowApp::cursor_icon(&self.lab, x, y)
        }
    }

    fn resize_direction(
        &self,
        key: WindowKey,
        width: f32,
        height: f32,
        x: f32,
        y: f32,
        maximised: bool,
    ) -> Option<ResizeDirection> {
        // A press while a target is being picked is the inspector's, on the
        // window's border as anywhere else.
        if key == LAB && self.withheld {
            return None;
        }
        koru_window::resize::edge(width, height, x, y, maximised)
    }

    fn resized(&mut self, key: WindowKey, size: PhysicalSize<u32>, scale_factor: f64) {
        self.seat(key).scale = scale_factor;
        if key == LAB {
            WindowApp::resized(&mut self.lab, size, scale_factor);
        }
    }

    fn scale_factor_changed(&mut self, key: WindowKey, scale_factor: f64) -> bool {
        self.seat(key).scale = scale_factor;
        false
    }

    fn paint_before_showing(&self, _key: WindowKey) -> bool {
        // Both windows are created hidden and shown with their first frame;
        // it is also the only way a window can be given an accessibility
        // provider.
        true
    }

    fn set_maximised(&mut self, key: WindowKey, maximised: bool) -> bool {
        key == LAB && WindowApp::set_maximised(&mut self.lab, maximised)
    }

    fn set_pinned(&mut self, key: WindowKey, pinned: bool) -> bool {
        if key != PANEL {
            return false;
        }
        self.panel_on_top = pinned;
        true
    }

    fn pinned(&self, key: WindowKey) -> bool {
        key == PANEL && self.inspector.panel_pinned()
    }

    fn pointer_moved(&mut self, key: WindowKey, x: f32, y: f32) -> bool {
        if key == PANEL {
            return self.take_stale(PANEL);
        }
        self.lab_input(|lab| WindowApp::pointer_moved(lab, x, y))
    }

    fn pointer_left(&mut self, key: WindowKey) -> bool {
        if key == PANEL {
            return self.take_stale(PANEL);
        }
        self.lab_input(|lab| WindowApp::pointer_left(lab))
    }

    fn click(
        &mut self,
        key: WindowKey,
        engine: &mut TextEngine,
        x: f32,
        y: f32,
        ctrl: bool,
    ) -> bool {
        if key == PANEL {
            return self.take_stale(PANEL);
        }
        self.lab_input(|lab| WindowApp::click(lab, engine, x, y, ctrl))
    }

    fn double_click(&mut self, key: WindowKey, engine: &mut TextEngine, x: f32, y: f32) -> bool {
        if key == PANEL {
            return self.take_stale(PANEL);
        }
        self.lab_input(|lab| WindowApp::double_click(lab, engine, x, y))
    }

    fn drag(&mut self, key: WindowKey, engine: &mut TextEngine, x: f32, y: f32) -> bool {
        if key == PANEL {
            return self.take_stale(PANEL);
        }
        self.lab_input(|lab| WindowApp::drag(lab, engine, x, y))
    }

    fn mouse_up(&mut self, key: WindowKey) -> bool {
        if key == PANEL {
            return self.take_stale(PANEL);
        }
        self.lab_input(|lab| WindowApp::mouse_up(lab))
    }

    fn scroll_by(&mut self, key: WindowKey, _delta: f32, _mods: ModifiersState) -> bool {
        // The Lab has nothing that scrolls; the panel's wheel is the
        // inspector's and has already been answered.
        self.take_stale(key)
    }

    fn key(
        &mut self,
        key: WindowKey,
        engine: &mut TextEngine,
        logical: &KeyboardKey,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        if key == PANEL {
            return self.take_stale(PANEL);
        }
        self.lab_input(|lab| WindowApp::key(lab, engine, logical, text, mods))
    }

    fn rich_key(
        &mut self,
        key: WindowKey,
        engine: &mut TextEngine,
        event: RichKeyEvent<'_>,
    ) -> bool {
        if key == PANEL {
            return self.take_stale(PANEL);
        }
        self.lab_input(|lab| WindowApp::rich_key(lab, engine, event))
    }

    fn wants_ime(&self, key: WindowKey) -> bool {
        key == LAB && WindowApp::wants_ime(&self.lab)
    }

    fn ime(&mut self, key: WindowKey, engine: &mut TextEngine, event: ImeEvent<'_>) -> bool {
        if key == PANEL {
            return self.take_stale(PANEL);
        }
        self.lab_input(|lab| WindowApp::ime(lab, engine, event))
    }

    fn ime_cursor_area(&self, key: WindowKey) -> Option<Rect> {
        if key == LAB {
            self.lab.ime_cursor_area()
        } else {
            None
        }
    }

    fn focus_changed(&mut self, key: WindowKey, focused: bool) -> bool {
        if key == PANEL {
            return self.take_stale(PANEL);
        }
        // Never withheld: the Lab must know it lost the keyboard even while
        // the inspector is picking from it.
        let changed = WindowApp::focus_changed(&mut self.lab, focused);
        self.take_stale(LAB) || changed
    }

    fn maintain(&mut self, key: WindowKey, _now: Instant) -> bool {
        if key == PANEL {
            self.serve_panel();
            return self.take_stale(PANEL);
        }
        let requested = self.serve_lab();
        self.take_stale(LAB) || requested
    }

    fn draw(&mut self, key: WindowKey, scene: &mut Scene, engine: &mut TextEngine, w: f32, h: f32) {
        self.seat(key).size = (w, h);
        if key == PANEL {
            self.inspector.draw_panel(scene, (w, h));
            return;
        }
        // A screen reader's requests, done with the loop's engine before the
        // frame that shows what they did.
        if !self.requests.is_empty() {
            for request in std::mem::take(&mut self.requests) {
                self.lab.accessibility_action(engine, &request);
            }
            // What they changed is published when the loop next settles.
            self.queue_settle();
        }
        // `None` while the inspector is closed or paused: the frame is then
        // drawn exactly as it is without one.
        let capture = self.inspector.begin_capture(OWNER, (w, h));
        WindowApp::draw(&mut self.lab, scene, engine, w, h);
        if self.inspector.end_capture(capture, scene) {
            self.panel_seat.stale = true;
            self.queue_settle();
        }
    }

    fn raw_event(&mut self, key: WindowKey, window: &Window, event: &WindowEvent) {
        let seat = self.seat(key);
        seat.scale = window.scale_factor();
        // The provider sees a window event before the application does.
        if let Some(access) = seat.access.as_mut() {
            access.process_event(window, event);
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            let size = window.inner_size().to_logical::<f32>(seat.scale);
            let at = position.to_logical::<f32>(seat.scale);
            seat.on_border = koru_window::resize::edge(
                size.width,
                size.height,
                at.x,
                at.y,
                window.is_maximized(),
            )
            .is_some();
        }
        if key != PANEL {
            if self.source_event(event) {
                self.monitors = monitors_of(window);
            }
            return;
        }
        // A request the runner never came for belongs to an event that is
        // over: a native drag only works inside the press that asked.
        self.panel_request = None;
        let resizing = self.panel_seat.on_border
            && matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            );
        // A press on the window's border resizes it, which is the runner's;
        // it is not also a press on whatever panel control is under it.
        if !resizing {
            self.panel_event(event);
        }
    }

    fn accessibility_wake(&self, _key: WindowKey) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let proxy = self.proxy.clone()?;
        // A screen reader's handlers run on whichever thread the platform
        // chose, and all this does is ask the loop to settle.
        Some(Arc::new(move || {
            let _ = proxy.send_event(Settle);
        }))
    }

    fn accessibility_seated(&mut self, key: WindowKey, provider: Option<WindowBridge>) {
        let (window, view) = (self.slots.window(), self.slots.view());
        self.seat(key).access = Some(PaneAccessibility::new(
            window,
            view,
            provider.map(|bridge| Box::new(bridge) as Box<dyn koru_window::AccessibilityProvider>),
        ));
    }
}

#[cfg(test)]
mod tests;
