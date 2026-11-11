// SPDX-License-Identifier: MPL-2.0

//! The event discipline, written once for every runner.
//!
//! [`WindowApp`](crate::WindowApp) drives one window and
//! [`MultiWindowApp`](crate::MultiWindowApp) drives a set of them, but the
//! rules between winit and either of them are the same rules: the key
//! quarantine, the resize border ahead of the caption ahead of content, the
//! drag and drag-resize that only work inside the press that asked, click
//! counting, the cursor cache, IME, the modifier clear on both focus edges,
//! the scale divide, theme re-application, present acknowledgement, and the
//! settle/reschedule decision. ADR-262 is explicit that copying that into a
//! second `ApplicationHandler` would fork exactly what this crate exists to
//! hold in one place, so it lives here and both runners call it.
//!
//! ## Two seams, and why each exists
//!
//! **[`WindowHandle`]** is everything the discipline asks of a live window.
//! A runner passes `Arc<Window>`; a test passes a recorder. Without it the
//! rules could only be exercised by opening a real window on a real GPU, which
//! is why they had no tests at all before.
//!
//! **[`Input`]** is one window event in this crate's own vocabulary. It exists
//! because **winit's `KeyEvent` cannot be constructed outside winit** — it
//! carries a private platform field — so a keyboard sequence can never be
//! synthesised from `WindowEvent`. [`dispatch`] translates and both runners go
//! through it, so the translation cannot differ between them; everything below
//! the translation is what the parity suite drives.

use std::path::Path;
use std::time::{Duration, Instant};

use vello::Scene;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::ControlFlow;
use winit::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, PhysicalKey};
use winit::window::{CursorIcon, ResizeDirection, Window, WindowAttributes};

use koru_draw::text::TextEngine;
use koru_draw::theme;

use crate::app::ClosingWindowState;
use crate::key_quarantine::KeyQuarantine;
use crate::request::WindowRequest;
use crate::snap::Placement;
use crate::{ImeEvent, RichKeyEvent, RichMouseEvent};

/// How close in time and space two presses have to be to count as a
/// double-click. winit does not report one, so a field that selects a word on
/// two clicks has to time them itself.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const DOUBLE_CLICK_SLOP: f32 = 4.0;

pub(crate) fn starts_maintenance(cause: &StartCause) -> bool {
    matches!(
        cause,
        StartCause::ResumeTimeReached { .. } | StartCause::Poll
    )
}

/// Recover the profile-switcher letter when Windows reports Ctrl+Alt+P as an
/// unidentified AltGr key. The main shell performs the same repair before its
/// views see a key; global Search uses this smaller window loop and therefore
/// needs the repair at this boundary too.
///
/// A keyboard layout that supplies a real logical character keeps it. Taking
/// that character away would make an AltGr layout unusable merely to reserve a
/// shortcut.
pub(crate) fn profile_switcher_key(
    logical: &Key,
    physical: PhysicalKey,
    mods: ModifiersState,
) -> Option<Key> {
    ((mods.control_key() || mods.super_key())
        && mods.alt_key()
        && !mods.shift_key()
        && matches!(logical, Key::Unidentified(_))
        && physical == PhysicalKey::Code(KeyCode::KeyP))
    .then(|| Key::Character("p".into()))
}

/// Everything the discipline asks of a window that is on screen.
///
/// A runner hands it `Arc<Window>`. The point of the trait is the other
/// implementation: a recorder, which is how a resize border, a caption drag or
/// a cursor transition can be asserted with no window and no device.
pub(crate) trait WindowHandle {
    fn inner_size(&self) -> PhysicalSize<u32>;
    fn outer_position(&self) -> Option<PhysicalPosition<i32>>;
    fn scale_factor(&self) -> f64;
    fn is_maximized(&self) -> bool;
    fn request_redraw(&self);
    fn set_cursor(&self, icon: CursorIcon);
    fn drag_window(&self);
    fn drag_resize_window(&self, direction: ResizeDirection);
    fn set_maximized(&self, maximized: bool);
    fn set_minimized(&self, minimized: bool);
    fn focus_window(&self);
    fn set_always_on_top(&self, on: bool);
    /// Re-apply the DWM border and shadow. `square` is maximised state: the
    /// border is painted at the *window* rect, which stops being the client
    /// rect the moment an edge reaches the screen.
    fn apply_frame(&self, square: bool);
    fn place(&self, what: Placement, default_size: (u32, u32));
    /// Bring the whole application forward, which `focus_window` cannot do on
    /// macOS. Process-global rather than window-scoped, and overridable only so
    /// a test can see that it happened.
    fn activate_app(&self) {
        crate::activation::activate_app();
    }
}

impl WindowHandle for std::sync::Arc<Window> {
    fn inner_size(&self) -> PhysicalSize<u32> {
        Window::inner_size(self)
    }

    fn outer_position(&self) -> Option<PhysicalPosition<i32>> {
        Window::outer_position(self).ok()
    }

    fn scale_factor(&self) -> f64 {
        Window::scale_factor(self)
    }

    fn is_maximized(&self) -> bool {
        Window::is_maximized(self)
    }

    fn request_redraw(&self) {
        Window::request_redraw(self);
    }

    fn set_cursor(&self, icon: CursorIcon) {
        Window::set_cursor(self, icon);
    }

    fn drag_window(&self) {
        let _ = Window::drag_window(self);
    }

    fn drag_resize_window(&self, direction: ResizeDirection) {
        let _ = Window::drag_resize_window(self, direction);
    }

    fn set_maximized(&self, maximized: bool) {
        Window::set_maximized(self, maximized);
    }

    fn set_minimized(&self, minimized: bool) {
        Window::set_minimized(self, minimized);
    }

    fn focus_window(&self) {
        Window::focus_window(self);
    }

    fn set_always_on_top(&self, on: bool) {
        Window::set_window_level(
            self,
            if on {
                winit::window::WindowLevel::AlwaysOnTop
            } else {
                winit::window::WindowLevel::Normal
            },
        );
    }

    fn apply_frame(&self, square: bool) {
        crate::frame::apply(self, theme::p().panel_border, square);
    }

    fn place(&self, what: Placement, default_size: (u32, u32)) {
        crate::snap::place(self, what, default_size);
    }
}

/// One window's worth of application, as the discipline needs to see it.
///
/// This is [`WindowApp`](crate::WindowApp) with the process-level lifecycle
/// removed — no `should_exit`, no `wants_window`, no `attach`. The single-window
/// runner reaches it through [`Single`], the multi-window runner through
/// [`WindowTarget`], which is the whole of the difference between the two.
pub(crate) trait DispatchApp {
    fn window_attributes(&self) -> WindowAttributes;
    fn window_activation(&self) -> crate::WindowActivation;
    fn wants_ime(&self) -> bool;
    fn paint_before_showing(&self) -> bool;

    fn wants_drag(&self) -> bool;
    fn wants_rich_mouse(&self) -> bool;
    fn is_caption(&self, x: f32, y: f32) -> bool;
    fn cursor_icon(&self, x: f32, y: f32) -> CursorIcon;
    fn resize_direction(
        &self,
        width: f32,
        height: f32,
        x: f32,
        y: f32,
        maximised: bool,
    ) -> Option<ResizeDirection>;

    fn set_maximised(&mut self, maximised: bool) -> bool;
    fn set_pinned(&mut self, pinned: bool) -> bool;
    fn moved(&mut self, position: PhysicalPosition<i32>);
    fn resized(&mut self, size: PhysicalSize<u32>, scale_factor: f64);
    fn scale_factor_changed(&mut self, scale_factor: f64) -> bool;
    fn occluded(&mut self, occluded: bool);
    fn focus_changed(&mut self, focused: bool) -> bool;
    fn system_theme_changed(&mut self, dark: bool) -> bool;
    fn close_requested(&mut self);

    fn pointer_moved(&mut self, x: f32, y: f32) -> bool;
    fn pointer_left(&mut self) -> bool;
    fn click(&mut self, engine: &mut TextEngine, x: f32, y: f32, ctrl: bool) -> bool;
    fn double_click(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> bool;
    fn drag(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> bool;
    fn mouse_up(&mut self) -> bool;
    fn scroll_by(&mut self, delta: f32, mods: ModifiersState) -> bool;
    fn rich_mouse(&mut self, engine: &mut TextEngine, event: RichMouseEvent) -> bool;
    fn context_menu(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> bool;
    fn navigate_history(&mut self, delta: i32) -> bool;

    #[allow(clippy::too_many_arguments)]
    fn physical_key_pressed(
        &mut self,
        engine: &mut TextEngine,
        physical: PhysicalKey,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
        repeat: bool,
    ) -> bool;
    fn physical_key_released(&mut self, physical: PhysicalKey) -> bool;
    fn rich_key(&mut self, engine: &mut TextEngine, event: RichKeyEvent<'_>) -> bool;
    fn ime(&mut self, engine: &mut TextEngine, event: ImeEvent<'_>) -> bool;
    fn dropped_file(&mut self, path: &Path) -> bool;

    fn take_window_request(&mut self) -> Option<WindowRequest>;

    fn maintain(&mut self, now: Instant) -> bool;
    fn maintenance_deadline(&self) -> Option<Instant>;
    fn animating(&self) -> bool;

    fn draw(&mut self, scene: &mut Scene, engine: &mut TextEngine, w: f32, h: f32);
    fn presented(&mut self);
}

/// The single-window runner's view of its one application.
pub(crate) struct Single<'a, A: crate::WindowApp>(pub &'a mut A);

impl<A: crate::WindowApp> DispatchApp for Single<'_, A> {
    fn window_attributes(&self) -> WindowAttributes {
        self.0.window()
    }

    fn window_activation(&self) -> crate::WindowActivation {
        self.0.window_activation()
    }

    fn wants_ime(&self) -> bool {
        self.0.wants_ime()
    }

    fn paint_before_showing(&self) -> bool {
        self.0.paint_before_showing()
    }

    fn wants_drag(&self) -> bool {
        self.0.wants_drag()
    }

    fn wants_rich_mouse(&self) -> bool {
        self.0.wants_rich_mouse()
    }

    fn is_caption(&self, x: f32, y: f32) -> bool {
        self.0.is_caption(x, y)
    }

    fn cursor_icon(&self, x: f32, y: f32) -> CursorIcon {
        self.0.cursor_icon(x, y)
    }

    fn resize_direction(
        &self,
        width: f32,
        height: f32,
        x: f32,
        y: f32,
        maximised: bool,
    ) -> Option<ResizeDirection> {
        self.0.resize_direction(width, height, x, y, maximised)
    }

    fn set_maximised(&mut self, maximised: bool) -> bool {
        self.0.set_maximised(maximised)
    }

    fn set_pinned(&mut self, pinned: bool) -> bool {
        self.0.set_pinned(pinned)
    }

    fn moved(&mut self, position: PhysicalPosition<i32>) {
        self.0.moved(position);
    }

    fn resized(&mut self, size: PhysicalSize<u32>, scale_factor: f64) {
        self.0.resized(size, scale_factor);
    }

    fn scale_factor_changed(&mut self, scale_factor: f64) -> bool {
        self.0.scale_factor_changed(scale_factor)
    }

    fn occluded(&mut self, occluded: bool) {
        self.0.occluded(occluded);
    }

    fn focus_changed(&mut self, focused: bool) -> bool {
        self.0.focus_changed(focused)
    }

    fn system_theme_changed(&mut self, dark: bool) -> bool {
        self.0.system_theme_changed(dark)
    }

    fn close_requested(&mut self) {
        self.0.close_requested();
    }

    fn pointer_moved(&mut self, x: f32, y: f32) -> bool {
        self.0.pointer_moved(x, y)
    }

    fn pointer_left(&mut self) -> bool {
        self.0.pointer_left()
    }

    fn click(&mut self, engine: &mut TextEngine, x: f32, y: f32, ctrl: bool) -> bool {
        self.0.click(engine, x, y, ctrl)
    }

    fn double_click(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> bool {
        self.0.double_click(engine, x, y)
    }

    fn drag(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> bool {
        self.0.drag(engine, x, y)
    }

    fn mouse_up(&mut self) -> bool {
        self.0.mouse_up()
    }

    fn scroll_by(&mut self, delta: f32, mods: ModifiersState) -> bool {
        self.0.scroll_by(delta, mods)
    }

    fn rich_mouse(&mut self, engine: &mut TextEngine, event: RichMouseEvent) -> bool {
        self.0.rich_mouse(engine, event)
    }

    fn context_menu(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> bool {
        self.0.context_menu(engine, x, y)
    }

    fn navigate_history(&mut self, delta: i32) -> bool {
        self.0.navigate_history(delta)
    }

    fn physical_key_pressed(
        &mut self,
        engine: &mut TextEngine,
        physical: PhysicalKey,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
        repeat: bool,
    ) -> bool {
        self.0
            .physical_key_pressed(engine, physical, key, text, mods, repeat)
    }

    fn physical_key_released(&mut self, physical: PhysicalKey) -> bool {
        self.0.physical_key_released(physical)
    }

    fn rich_key(&mut self, engine: &mut TextEngine, event: RichKeyEvent<'_>) -> bool {
        self.0.rich_key(engine, event)
    }

    fn ime(&mut self, engine: &mut TextEngine, event: ImeEvent<'_>) -> bool {
        self.0.ime(engine, event)
    }

    fn dropped_file(&mut self, path: &Path) -> bool {
        self.0.dropped_file(path)
    }

    fn take_window_request(&mut self) -> Option<WindowRequest> {
        self.0.take_window_request()
    }

    fn maintain(&mut self, now: Instant) -> bool {
        self.0.maintain(now)
    }

    fn maintenance_deadline(&self) -> Option<Instant> {
        self.0.maintenance_deadline()
    }

    fn animating(&self) -> bool {
        self.0.animating()
    }

    fn draw(&mut self, scene: &mut Scene, engine: &mut TextEngine, w: f32, h: f32) {
        self.0.draw(scene, engine, w, h);
    }

    fn presented(&mut self) {
        self.0.presented();
    }
}

/// One window's input state: what the discipline must remember about a window
/// between events.
///
/// `pointer`, `dragging`, `last_press` and `mods` deliberately outlive the
/// window itself in the single-window runner, where an overlay is dropped and
/// reopened; `resize_edge` and `cursor_icon` are facts about a live surface and
/// are cleared by [`seat`](Self::seat).
#[derive(Debug, Default)]
pub(crate) struct WindowInput {
    /// Where the pointer is, so a click knows where it landed. winit delivers
    /// the position and the button as separate events.
    pub pointer: (f32, f32),
    /// Held while the left button is down, so a move can extend a selection
    /// rather than only moving the hover.
    pub dragging: bool,
    /// The undecorated window border currently under the pointer.
    pub resize_edge: Option<ResizeDirection>,
    /// The last press and count, for spotting double/triple clicks.
    pub last_press: Option<(Instant, f32, f32, u8)>,
    /// Tracked from `ModifiersChanged`, because a `KeyboardInput` event does
    /// not carry them and a text field needs Ctrl and Shift to tell a chord
    /// from a letter.
    pub mods: ModifiersState,
    /// Cached because `CursorMoved` can be much more frequent than an actual
    /// cursor-shape transition.
    pub cursor_icon: CursorIcon,
    /// Whether this window is currently above the others.
    pub pinned: bool,
}

impl WindowInput {
    /// A window has just been created for this slot.
    pub fn seat(&mut self, pinned: bool) {
        self.resize_edge = None;
        self.cursor_icon = CursorIcon::Default;
        self.pinned = pinned;
    }

    /// The window went away while the process lives on.
    pub fn unseat(&mut self) {
        self.resize_edge = None;
    }
}

/// One keyboard event, without winit's un-constructible platform field.
#[derive(Clone, Copy, Debug)]
pub(crate) struct KeyInput<'a> {
    pub physical_key: PhysicalKey,
    pub logical_key: &'a Key,
    pub text: Option<&'a str>,
    pub location: KeyLocation,
    pub state: ElementState,
    pub repeat: bool,
}

impl<'a> KeyInput<'a> {
    fn from_winit(event: &'a winit::event::KeyEvent) -> Self {
        Self {
            physical_key: event.physical_key,
            logical_key: &event.logical_key,
            text: event.text.as_deref(),
            location: event.location,
            state: event.state,
            repeat: event.repeat,
        }
    }

    /// The lossless record an application receives, with the logical key the
    /// platform repairs may have replaced.
    fn rich(&self, logical_key: &'a Key, modifiers: ModifiersState) -> RichKeyEvent<'a> {
        RichKeyEvent {
            logical_key,
            physical_key: self.physical_key,
            location: self.location,
            text: self.text,
            state: self.state.into(),
            repeat: self.repeat,
            modifiers,
        }
    }
}

/// One window event, in this crate's vocabulary rather than winit's.
///
/// See the module note: a `KeyEvent` cannot be built outside winit, so a
/// keyboard sequence can only be synthesised against a record of ours.
pub(crate) enum Input<'a> {
    Close,
    Resized(PhysicalSize<u32>),
    /// A DPI change, which winit reports separately from any size change.
    ScaleFactorChanged(f64),
    Moved(PhysicalPosition<i32>),
    RedrawRequested,
    /// **Physical** pixels, as winit reports them.
    CursorMoved {
        x: f64,
        y: f64,
    },
    CursorLeft,
    Focused(bool),
    Occluded(bool),
    ModifiersChanged(ModifiersState),
    ThemeChanged(bool),
    Wheel(MouseScrollDelta),
    Mouse {
        state: ElementState,
        button: MouseButton,
    },
    Key(KeyInput<'a>),
    Ime(ImeEvent<'a>),
    DroppedFile(&'a Path),
}

/// The GPU work an event asks of the runner that owns the surface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum FrameWork {
    #[default]
    None,
    Redraw,
    /// The drawable changed size: reconfigure the surface, re-read the flush
    /// edges and ask for a frame.
    Resize(PhysicalSize<u32>),
}

/// What the runner still owes after the discipline has run.
///
/// The split is exactly the part a runner cannot delegate: window lifecycle
/// (`settle`), and the surface (`work`). `invalidate` is carried rather than
/// performed because it happens *after* `settle` in every arm that has both,
/// and a window created by that settle is the one that must be asked to draw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Aftermath {
    pub settle: bool,
    pub invalidate: Option<bool>,
    pub work: FrameWork,
}

impl Aftermath {
    fn nothing() -> Self {
        Self::default()
    }

    fn invalidate(changed: bool) -> Self {
        Self {
            invalidate: Some(changed),
            ..Self::default()
        }
    }

    fn settled(changed: bool) -> Self {
        Self {
            settle: true,
            invalidate: Some(changed),
            work: FrameWork::None,
        }
    }
}

/// Translate a winit event and run the discipline over it.
///
/// The translation is deliberately the only thing above [`deliver`]: both
/// runners call this function, so nothing here can differ between them.
pub(crate) fn dispatch<T: DispatchApp, H: WindowHandle>(
    app: &mut T,
    text: &mut TextEngine,
    state: &mut WindowInput,
    quarantine: &mut KeyQuarantine,
    handle: Option<&H>,
    event: WindowEvent,
) -> Aftermath {
    let input = match &event {
        WindowEvent::CloseRequested => Input::Close,
        WindowEvent::Resized(size) => Input::Resized(*size),
        WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
            Input::ScaleFactorChanged(*scale_factor)
        }
        WindowEvent::Moved(position) => Input::Moved(*position),
        WindowEvent::RedrawRequested => Input::RedrawRequested,
        WindowEvent::CursorMoved { position, .. } => Input::CursorMoved {
            x: position.x,
            y: position.y,
        },
        WindowEvent::CursorLeft { .. } => Input::CursorLeft,
        WindowEvent::Focused(focused) => Input::Focused(*focused),
        WindowEvent::Occluded(occluded) => Input::Occluded(*occluded),
        WindowEvent::ModifiersChanged(mods) => Input::ModifiersChanged(mods.state()),
        WindowEvent::ThemeChanged(theme) => {
            Input::ThemeChanged(*theme == winit::window::Theme::Dark)
        }
        WindowEvent::MouseWheel { delta, .. } => Input::Wheel(*delta),
        WindowEvent::MouseInput { state, button, .. } => Input::Mouse {
            state: *state,
            button: *button,
        },
        WindowEvent::KeyboardInput { event, .. } => Input::Key(KeyInput::from_winit(event)),
        WindowEvent::Ime(ime) => Input::Ime(ImeEvent::from(ime)),
        WindowEvent::DroppedFile(path) => Input::DroppedFile(path),
        _ => return Aftermath::nothing(),
    };
    deliver(app, text, state, quarantine, handle, input)
}

/// The discipline itself.
pub(crate) fn deliver<T: DispatchApp, H: WindowHandle>(
    app: &mut T,
    text: &mut TextEngine,
    state: &mut WindowInput,
    quarantine: &mut KeyQuarantine,
    handle: Option<&H>,
    input: Input<'_>,
) -> Aftermath {
    match input {
        Input::Close => {
            app.close_requested();
            Aftermath {
                settle: true,
                ..Aftermath::nothing()
            }
        }

        Input::Resized(size) => {
            // A maximise can come from Win+Arrow, a snap layout or the taskbar
            // as easily as from our own button, and the middle glyph has to
            // follow all of them.
            let (maximised, scale_factor) = handle
                .map(|handle| (handle.is_maximized(), handle.scale_factor()))
                .unwrap_or((false, 1.0));
            app.set_maximised(maximised);
            app.resized(size, scale_factor);
            Aftermath {
                work: FrameWork::Resize(size),
                ..Aftermath::nothing()
            }
        }

        // **A DPI change with no size change was invisible.** The loop only
        // ever re-read `scale_factor()` on a resize, so dragging a window to a
        // display at a different scale — which winit reports as
        // `ScaleFactorChanged` and, when the drawable size is unchanged,
        // nothing else — never reached the application. Defaulted to `false`,
        // so a host that does not implement it behaves exactly as before.
        Input::ScaleFactorChanged(scale_factor) => {
            Aftermath::invalidate(app.scale_factor_changed(scale_factor))
        }

        Input::Moved(position) => {
            app.moved(position);
            Aftermath::nothing()
        }

        Input::RedrawRequested => Aftermath {
            work: FrameWork::Redraw,
            ..Aftermath::nothing()
        },

        Input::CursorMoved { x, y } => {
            // Logical, like everything the app places: winit reports the
            // pointer in physical pixels and every rect was built in logical
            // ones.
            let scale = scale_of(handle);
            state.pointer = (x as f32 / scale, y as f32 / scale);
            let (x, y) = state.pointer;
            state.resize_edge = handle.and_then(|handle| {
                let size = handle.inner_size();
                app.resize_direction(
                    size.width as f32 / scale,
                    size.height as f32 / scale,
                    x,
                    y,
                    handle.is_maximized(),
                )
            });
            let changed = if state.dragging {
                app.drag(text, x, y)
            } else {
                app.pointer_moved(x, y)
            };
            let icon = state
                .resize_edge
                .map(CursorIcon::from)
                .unwrap_or_else(|| app.cursor_icon(x, y));
            if let Some(handle) = handle
                && state.cursor_icon != icon
            {
                handle.set_cursor(icon);
                state.cursor_icon = icon;
            }
            Aftermath::invalidate(changed)
        }

        // **winit sends this and the loop used to drop it.** Hover was cleared
        // only by the next `CursorMoved`, which never comes if the pointer left
        // the window — so a highlighted row stayed highlighted under a window
        // that no longer had the pointer. Defaulted to `false`.
        Input::CursorLeft => Aftermath::invalidate(app.pointer_left()),

        Input::Focused(focused) => {
            // **Clear the modifiers on a focus change.** A window that appears
            // while a chord is held never sees those keys released, so a stale
            // `Shift` makes the next plain click extend a selection. winit
            // reports modifiers only while focused, so the honest state on
            // either edge is "nothing is held".
            state.mods = ModifiersState::empty();
            Aftermath::settled(app.focus_changed(focused))
        }

        Input::Occluded(occluded) => {
            app.occluded(occluded);
            Aftermath::nothing()
        }

        Input::ModifiersChanged(mods) => {
            state.mods = mods;
            Aftermath::nothing()
        }

        Input::ThemeChanged(dark) => {
            let changed = app.system_theme_changed(dark);
            if changed
                && app.window_activation().allows_focus()
                && let Some(handle) = handle
            {
                handle.apply_frame(handle.is_maximized());
            }
            Aftermath::invalidate(changed)
        }

        Input::Wheel(delta) => {
            // A line of body text per notch, which is what the rest of the
            // product scrolls by, at the chosen wheel speed. A pixel delta is
            // a trackpad following the finger and is not scaled, and neither
            // is primary+wheel: that is the display zoom, whose step is its
            // own, as in the Koru window.
            let zooming = state.mods.control_key() || state.mods.super_key();
            let speed = if zooming {
                1.0
            } else {
                f32::from(theme::wheel_speed())
            };
            let lines = match delta {
                MouseScrollDelta::LineDelta(_, y) => {
                    y * theme::text_metrics::DOC_BODY.line_height * speed
                }
                MouseScrollDelta::PixelDelta(pixels) => pixels.y as f32 / scale_of(handle),
            };
            Aftermath::settled(app.scroll_by(lines, state.mods))
        }

        Input::Mouse {
            state: ElementState::Pressed,
            button: MouseButton::Left,
        } => {
            let (x, y) = state.pointer;
            // The resize border is outermost. `drag_resize_window`, like
            // `drag_window`, only works inside the press that requested it.
            if let Some(direction) = state.resize_edge
                && let Some(handle) = handle
            {
                handle.drag_resize_window(direction);
                return Aftermath::nothing();
            }
            // The window is undecorated, so dragging it is ours to do — and
            // `drag_window` only works inside the press that asked for it,
            // which is why it is here rather than on a later move event.
            if app.is_caption(x, y)
                && let Some(handle) = handle
            {
                handle.drag_window();
                return Aftermath::nothing();
            }
            let ctrl = state.mods.control_key() || state.mods.super_key();
            let changed = if app.wants_rich_mouse() {
                let click_count = next_click_count(state.last_press, x, y);
                state.last_press = (click_count < 3).then(|| (Instant::now(), x, y, click_count));
                app.rich_mouse(
                    text,
                    RichMouseEvent {
                        x,
                        y,
                        button: MouseButton::Left,
                        state: ElementState::Pressed.into(),
                        modifiers: state.mods,
                        click_count,
                    },
                )
            } else if continues_press(state.last_press, x, y) {
                state.last_press = None;
                app.double_click(text, x, y)
            } else {
                state.last_press = Some((Instant::now(), x, y, 1));
                state.dragging = app.wants_drag();
                app.click(text, x, y, ctrl)
            };
            perform_request(app, state, handle);
            Aftermath::settled(changed)
        }

        Input::Mouse {
            state: ElementState::Pressed,
            button: MouseButton::Right,
        } => {
            let (x, y) = state.pointer;
            let changed = if app.wants_rich_mouse() {
                app.rich_mouse(
                    text,
                    RichMouseEvent {
                        x,
                        y,
                        button: MouseButton::Right,
                        state: ElementState::Pressed.into(),
                        modifiers: state.mods,
                        click_count: 1,
                    },
                )
            } else {
                deliver_input(app, text, AppInput::RightClick { x, y })
            };
            perform_request(app, state, handle);
            Aftermath::settled(changed)
        }

        Input::Mouse {
            state: ElementState::Released,
            button: MouseButton::Left,
        } => {
            state.dragging = false;
            let changed = if app.wants_rich_mouse() {
                let (x, y) = state.pointer;
                app.rich_mouse(
                    text,
                    RichMouseEvent {
                        x,
                        y,
                        button: MouseButton::Left,
                        state: ElementState::Released.into(),
                        modifiers: state.mods,
                        click_count: 0,
                    },
                )
            } else {
                app.mouse_up()
            };
            Aftermath::invalidate(changed)
        }

        Input::Mouse {
            state: button_state,
            button: MouseButton::Middle,
        } if app.wants_rich_mouse() => {
            let (x, y) = state.pointer;
            let changed = app.rich_mouse(
                text,
                RichMouseEvent {
                    x,
                    y,
                    button: MouseButton::Middle,
                    state: button_state.into(),
                    modifiers: state.mods,
                    click_count: u8::from(button_state == ElementState::Pressed),
                },
            );
            Aftermath::invalidate(changed)
        }

        Input::Mouse {
            state: ElementState::Released,
            button: MouseButton::Right,
        } if app.wants_rich_mouse() => {
            let (x, y) = state.pointer;
            let changed = app.rich_mouse(
                text,
                RichMouseEvent {
                    x,
                    y,
                    button: MouseButton::Right,
                    state: ElementState::Released.into(),
                    modifiers: state.mods,
                    click_count: 0,
                },
            );
            Aftermath::invalidate(changed)
        }

        // The two thumb buttons walk the app's history, as they do in every
        // browser — a terminal opting into rich mouse included, since no
        // terminal protocol reports them. They fell into the arm below and
        // were dropped until the standalone Koru AI window had a history.
        Input::Mouse {
            state: ElementState::Pressed,
            button: button @ (MouseButton::Back | MouseButton::Forward),
        } => {
            let delta = if button == MouseButton::Back { -1 } else { 1 };
            let changed = app.navigate_history(delta);
            perform_request(app, state, handle);
            Aftermath::settled(changed)
        }

        Input::Mouse { .. } => Aftermath::nothing(),

        Input::Key(key) if key.state == ElementState::Released => {
            // A release ends the quarantine for that key, which is the ordinary
            // way an entry leaves — the timeout exists only for the release
            // that never arrives.
            let quarantined = quarantine.consumes(key.physical_key, Instant::now());
            quarantine.released(key.physical_key);
            if quarantined {
                return Aftermath::nothing();
            }
            if app.physical_key_released(key.physical_key) {
                perform_request(app, state, handle);
                return Aftermath::settled(true);
            }
            let rich = key.rich(key.logical_key, state.mods);
            let changed = deliver_input(app, text, AppInput::Key(rich));
            perform_request(app, state, handle);
            Aftermath::settled(changed)
        }

        Input::Key(key) => {
            if quarantine.consumes(key.physical_key, Instant::now()) {
                return Aftermath::nothing();
            }
            // Every key goes to the app, which is what lets a screen with a
            // text field on it exist at all: a loop that resolved five named
            // keys itself would have to grow a second, silent copy of the
            // field's key map to let anything be typed.
            let recovered = profile_switcher_key(key.logical_key, key.physical_key, state.mods);
            let logical_key = recovered.as_ref().unwrap_or(key.logical_key);
            if app.physical_key_pressed(
                text,
                key.physical_key,
                logical_key,
                key.text,
                state.mods,
                key.repeat,
            ) {
                perform_request(app, state, handle);
                return Aftermath::settled(true);
            }
            let rich = key.rich(logical_key, state.mods);
            let changed = deliver_input(app, text, AppInput::Key(rich));
            perform_request(app, state, handle);
            Aftermath::settled(changed)
        }

        Input::Ime(event) => {
            let changed = deliver_input(app, text, AppInput::Ime(event));
            perform_request(app, state, handle);
            Aftermath::settled(changed)
        }

        Input::DroppedFile(path) => {
            let changed = deliver_input(app, text, AppInput::DroppedFile(path));
            perform_request(app, state, handle);
            Aftermath::settled(changed)
        }
    }
}

/// Device pixels per logical pixel, or 1.0 before there is a window.
fn scale_of<H: WindowHandle>(handle: Option<&H>) -> f32 {
    handle
        .map(|handle| handle.scale_factor() as f32)
        .unwrap_or(1.0)
}

pub(crate) enum AppInput<'a> {
    Key(RichKeyEvent<'a>),
    Ime(ImeEvent<'a>),
    RightClick { x: f32, y: f32 },
    DroppedFile(&'a Path),
}

pub(crate) fn deliver_input<T: DispatchApp>(
    app: &mut T,
    engine: &mut TextEngine,
    input: AppInput<'_>,
) -> bool {
    match input {
        AppInput::Key(event) => app.rich_key(engine, event),
        AppInput::Ime(event) => app.ime(engine, event),
        AppInput::RightClick { x, y } => app.context_menu(engine, x, y),
        AppInput::DroppedFile(path) => app.dropped_file(path),
    }
}

/// Do whatever the app asked of the window.
///
/// **Here and not in the app**, because only a runner holds a `Window`. The app
/// records a request and this drains it — the same shape the editor's
/// `wants_open` has, and the reason a click on a caption button is testable
/// without a window.
///
/// `Close` and `HideToTray` are deliberately not performed; see
/// [`WindowRequest`]. The runner's `settle` has already read the app's answer.
pub(crate) fn perform_request<T: DispatchApp, H: WindowHandle>(
    app: &mut T,
    state: &mut WindowInput,
    handle: Option<&H>,
) {
    let Some(request) = app.take_window_request() else {
        return;
    };
    let Some(handle) = handle else { return };
    if !app.window_activation().allows_focus()
        && matches!(
            request,
            WindowRequest::Drag
                | WindowRequest::Focus
                | WindowRequest::Minimise
                | WindowRequest::ToggleMaximise
                | WindowRequest::TogglePin
                | WindowRequest::Place(_)
                | WindowRequest::PlaceAndLockExact { .. }
                | WindowRequest::LockCurrentSize
                | WindowRequest::UnlockSizeAndRestore
                | WindowRequest::StartRecording(_)
                | WindowRequest::StopRecording
        )
    {
        // winit rewrites the complete Windows extended-style word for several
        // of these operations. Passive windows are status surfaces, so consume
        // incompatible chrome/activation requests rather than risk losing
        // WS_EX_NOACTIVATE after reveal.
        return;
    }
    match request {
        WindowRequest::Drag => handle.drag_window(),
        WindowRequest::Focus => {
            if app.window_activation().allows_focus() {
                handle.set_minimized(false);
                handle.focus_window();
                handle.activate_app();
            }
        }
        WindowRequest::Minimise => handle.set_minimized(true),
        WindowRequest::ToggleMaximise => {
            let now = !handle.is_maximized();
            handle.set_maximized(now);
            // The border is painted at the *window* rect, which stops being the
            // client rect the moment an edge reaches the screen — so the frame
            // has to be re-applied, and corners squared only when maximised.
            handle.apply_frame(now);
            app.set_maximised(now);
        }
        WindowRequest::TogglePin => {
            state.pinned = !state.pinned;
            handle.set_always_on_top(state.pinned);
            app.set_pinned(state.pinned);
        }
        WindowRequest::Place(what) => {
            handle.place(what, default_size(&app.window_attributes()));
        }
        // Not a runner's to decide. See `WindowRequest`. `TestUpgrade` needs
        // `koru-updater`, which only the full shell owns, and
        // `SetCloseBehavior` a live `DesktopPreferences` copy, which only the
        // shell holds. The recording size lock is the shell's for the same
        // reason: geometry memory and the minimum size are app policy — and
        // the capture itself is the shell's because the HWND is.
        WindowRequest::Close
        | WindowRequest::HideToTray
        | WindowRequest::TestUpgrade
        | WindowRequest::SetCloseBehavior(_)
        | WindowRequest::PlaceAndLockExact { .. }
        | WindowRequest::LockCurrentSize
        | WindowRequest::UnlockSizeAndRestore
        | WindowRequest::StartRecording(_)
        | WindowRequest::StopRecording => {}
    }
}

/// Ask for a frame, and only when something actually changed.
pub(crate) fn invalidate<H: WindowHandle>(handle: Option<&H>, changed: bool) {
    if changed && let Some(handle) = handle {
        handle.request_redraw();
    }
}

/// What one window answered when its settle pass ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Settle {
    pub changed: bool,
    pub animating: bool,
    pub deadline: Option<Instant>,
}

/// Drain one window's pending work and read back its next-wake facts.
///
/// **Drain first, then decide when to wake next.** Work that arrived on another
/// thread is applied here rather than waiting for an input the person has no
/// reason to give. The runner folds the answers — one window, or the OR and the
/// `min` across a set — into a single [`ControlFlow`].
pub(crate) fn maintain_window<T: DispatchApp, H: WindowHandle>(
    app: &mut T,
    handle: Option<&H>,
    now: Instant,
) -> Settle {
    let changed = app.maintain(now);
    let animating = app.animating();
    let deadline = app.maintenance_deadline();
    // An animation asks for the next frame outright, which is the single
    // exception ADR-26 allows.
    invalidate(handle, animating);
    invalidate(handle, changed);
    Settle {
        changed,
        animating,
        deadline,
    }
}

/// Sleep until the next deadline, or until something happens.
///
/// `Wait` with no deadline is the resting state and what keeps an idle,
/// windowless process at zero CPU.
pub(crate) fn next_flow(animating: bool, deadline: Option<Instant>) -> ControlFlow {
    if animating {
        ControlFlow::Poll
    } else if let Some(deadline) = deadline {
        ControlFlow::WaitUntil(deadline)
    } else {
        ControlFlow::Wait
    }
}

/// The live OS state of one window, taken at a lifecycle boundary.
pub(crate) fn closing_state<H: WindowHandle>(handle: &H, pinned: bool) -> ClosingWindowState {
    ClosingWindowState {
        position: handle.outer_position(),
        inner_size: handle.inner_size(),
        scale_factor: handle.scale_factor(),
        maximised: handle.is_maximized(),
        pinned,
    }
}

/// Whether a press at `(x, y)` continues `last` rather than starting a click of
/// its own.
///
/// winit reports presses, not clicks, so the timing is ours. **Both the
/// interval and the distance matter**: two presses a second apart are two
/// clicks, and so are two presses across the window from each other — without
/// the distance test, clicking one row and then quickly clicking a different one
/// selects a word in the second.
///
/// Free rather than a method so it can be asserted without a GPU.
pub(crate) fn continues_press(last: Option<(Instant, f32, f32, u8)>, x: f32, y: f32) -> bool {
    last.is_some_and(|(at, px, py, _)| {
        at.elapsed() < DOUBLE_CLICK
            && (px - x).abs() <= DOUBLE_CLICK_SLOP
            && (py - y).abs() <= DOUBLE_CLICK_SLOP
    })
}

pub(crate) fn next_click_count(last: Option<(Instant, f32, f32, u8)>, x: f32, y: f32) -> u8 {
    if continues_press(last, x, y) {
        last.map_or(2, |(_, _, _, count)| count.saturating_add(1).min(3))
    } else {
        1
    }
}

/// The size a `Place::Reset` returns to: whatever the app asks for on a fresh
/// window, or a sane default when it names none.
pub(crate) fn default_size(attributes: &WindowAttributes) -> (u32, u32) {
    attributes
        .inner_size
        .map(|size| {
            let size = size.to_physical::<u32>(1.0);
            (size.width, size.height)
        })
        .unwrap_or((1120, 800))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod parity;
