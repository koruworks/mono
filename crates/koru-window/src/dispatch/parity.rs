// SPDX-License-Identifier: MPL-2.0

//! The packet's gate: identical event sequences through both runners.
//!
//! Every row of `docs/features/terminal/plans/SP-0.EVENT_MATRIX.md` that dispatch owns
//! is driven twice — once through [`Single`], the single-window runner's view
//! of its application, and once through
//! [`WindowTarget`](crate::multi::WindowTarget), a one-window instance of the
//! multi-window runner's — over one recorder that implements **both** traits.
//! The two traces must be equal, and each test also pins what the trace says,
//! so a change that breaks both runners equally still fails.
//!
//! **What is being proven.** The discipline is one body of code, so parity is
//! structural rather than lucky; what could still differ is the adapter, which
//! is where a `WindowKey` is threaded and a method could be forwarded to the
//! wrong hook or in the wrong order. That is exactly what these drive. The
//! `WindowEvent` → [`Input`] translation sits above both and is therefore
//! shared by construction (see the module note on [`super`]).

use std::path::PathBuf;
use std::time::Duration;

use winit::event::MouseScrollDelta;
use winit::keyboard::{KeyLocation, NamedKey, NativeKeyCode};

use super::tests::FakeWindow;
use super::*;
use crate::WindowApp;
use crate::multi::{MultiWindowApp, WindowKey, WindowTarget};

/// The one window both drivers use. A value with no special meaning, on
/// purpose: nothing in the runner may depend on which key a window has.
const KEY: WindowKey = WindowKey(7);

/// An application that answers, and remembers being asked.
///
/// Every hook is written **once**, as an inherent method; the two trait impls
/// are pure forwarding. A behavioral difference between the runners therefore
/// cannot hide in the recorder.
#[derive(Default)]
struct Recorder {
    log: Vec<String>,
    /// Which window the multi-window impl was told about, asserted to be `KEY`
    /// throughout — the single-window impl has no key to record.
    keys: Vec<WindowKey>,
    windows: Vec<WindowKey>,
    /// What every input hook answers.
    changed: bool,
    caption: bool,
    wants_drag: bool,
    wants_rich_mouse: bool,
    edge: Option<ResizeDirection>,
    icon: CursorIcon,
    theme_changes: bool,
    consume_physical: bool,
    request: Option<WindowRequest>,
    quarantine: Vec<PhysicalKey>,
    deadline: Option<Instant>,
    animating: bool,
    activation: crate::WindowActivation,
    generation: u64,
}

impl Recorder {
    fn new() -> Self {
        Self {
            windows: vec![KEY],
            changed: true,
            ..Self::default()
        }
    }

    fn say(&mut self, line: impl Into<String>) {
        self.log.push(line.into());
    }

    fn taken(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }

    fn on_pointer_moved(&mut self, x: f32, y: f32) -> bool {
        self.say(format!("pointer_moved {x} {y}"));
        self.changed
    }

    fn on_pointer_left(&mut self) -> bool {
        self.say("pointer_left");
        self.changed
    }

    fn on_click(&mut self, x: f32, y: f32, ctrl: bool) -> bool {
        self.say(format!("click {x} {y} ctrl={ctrl}"));
        self.changed
    }

    fn on_double_click(&mut self, x: f32, y: f32) -> bool {
        self.say(format!("double_click {x} {y}"));
        self.changed
    }

    fn on_drag(&mut self, x: f32, y: f32) -> bool {
        self.say(format!("drag {x} {y}"));
        self.changed
    }

    fn on_mouse_up(&mut self) -> bool {
        self.say("mouse_up");
        self.changed
    }

    fn on_scroll_by(&mut self, delta: f32, mods: ModifiersState) -> bool {
        self.say(format!("scroll_by {delta} {mods:?}"));
        self.changed
    }

    fn on_navigate_history(&mut self, delta: i32) -> bool {
        self.say(format!("navigate_history {delta}"));
        self.changed
    }

    fn on_rich_mouse(&mut self, event: RichMouseEvent) -> bool {
        self.say(format!(
            "rich_mouse {:?} {:?} count={} at {} {} {:?}",
            event.button, event.state, event.click_count, event.x, event.y, event.modifiers
        ));
        self.changed
    }

    fn on_right_click(&mut self, x: f32, y: f32) -> bool {
        self.say(format!("right_click {x} {y}"));
        self.changed
    }

    fn on_key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        self.say(format!("key {key:?} text={text:?} {mods:?}"));
        self.changed
    }

    fn on_physical_key_pressed(
        &mut self,
        physical: PhysicalKey,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
        repeat: bool,
    ) -> bool {
        self.say(format!(
            "physical_key_pressed {physical:?} {key:?} text={text:?} {mods:?} repeat={repeat}"
        ));
        self.consume_physical
    }

    fn on_physical_key_released(&mut self, physical: PhysicalKey) -> bool {
        self.say(format!("physical_key_released {physical:?}"));
        false
    }

    fn on_ime(&mut self, event: ImeEvent<'_>) -> bool {
        self.say(format!("ime {event:?}"));
        self.changed
    }

    fn on_dropped_file(&mut self, path: &Path) -> bool {
        self.say(format!("dropped_file {}", path.display()));
        self.changed
    }

    fn on_focus_changed(&mut self, focused: bool) -> bool {
        self.say(format!("focus_changed {focused}"));
        self.changed
    }

    fn on_occluded(&mut self, occluded: bool) {
        self.say(format!("occluded {occluded}"));
    }

    fn on_system_theme_changed(&mut self, dark: bool) -> bool {
        self.say(format!("system_theme_changed dark={dark}"));
        self.theme_changes
    }

    fn on_resized(&mut self, size: PhysicalSize<u32>, scale_factor: f64) {
        self.say(format!(
            "resized {}x{} scale={scale_factor}",
            size.width, size.height
        ));
    }

    fn on_scale_factor_changed(&mut self, scale_factor: f64) -> bool {
        self.say(format!("scale_factor_changed {scale_factor}"));
        self.changed
    }

    fn on_set_maximised(&mut self, maximised: bool) -> bool {
        self.say(format!("set_maximised {maximised}"));
        false
    }

    fn on_set_pinned(&mut self, pinned: bool) -> bool {
        self.say(format!("set_pinned {pinned}"));
        false
    }

    fn on_moved(&mut self, position: PhysicalPosition<i32>) {
        self.say(format!("moved {} {}", position.x, position.y));
    }

    fn on_close_requested(&mut self) {
        self.say("close_requested");
    }

    fn on_maintain(&mut self, _now: Instant) -> bool {
        self.say("maintain");
        false
    }

    fn on_presented(&mut self) {
        self.say("presented");
    }

    fn on_take_window_request(&mut self) -> Option<WindowRequest> {
        let request = self.request.take();
        if let Some(request) = &request {
            self.say(format!("take_window_request {request:?}"));
        }
        request
    }

    fn on_take_key_quarantine(&mut self) -> Vec<PhysicalKey> {
        let asked = std::mem::take(&mut self.quarantine);
        if !asked.is_empty() {
            self.say(format!("take_key_quarantine {asked:?}"));
        }
        asked
    }
}

impl WindowApp for Recorder {
    type Event = ();

    fn window_activation(&self) -> crate::WindowActivation {
        self.activation
    }

    fn window_generation(&self) -> u64 {
        self.generation
    }

    fn should_exit(&self) -> bool {
        false
    }

    fn close_requested(&mut self) {
        self.on_close_requested();
    }

    fn wants_drag(&self) -> bool {
        self.wants_drag
    }

    fn wants_rich_mouse(&self) -> bool {
        self.wants_rich_mouse
    }

    fn is_caption(&self, _x: f32, _y: f32) -> bool {
        self.caption
    }

    fn cursor_icon(&self, _x: f32, _y: f32) -> CursorIcon {
        self.icon
    }

    fn resize_direction(
        &self,
        _width: f32,
        _height: f32,
        _x: f32,
        _y: f32,
        _maximised: bool,
    ) -> Option<ResizeDirection> {
        self.edge
    }

    fn set_maximised(&mut self, maximised: bool) -> bool {
        self.on_set_maximised(maximised)
    }

    fn set_pinned(&mut self, pinned: bool) -> bool {
        self.on_set_pinned(pinned)
    }

    fn moved(&mut self, position: PhysicalPosition<i32>) {
        self.on_moved(position);
    }

    fn resized(&mut self, size: PhysicalSize<u32>, scale_factor: f64) {
        self.on_resized(size, scale_factor);
    }

    fn scale_factor_changed(&mut self, scale_factor: f64) -> bool {
        self.on_scale_factor_changed(scale_factor)
    }

    fn occluded(&mut self, occluded: bool) {
        self.on_occluded(occluded);
    }

    fn focus_changed(&mut self, focused: bool) -> bool {
        self.on_focus_changed(focused)
    }

    fn system_theme_changed(&mut self, dark: bool) -> bool {
        self.on_system_theme_changed(dark)
    }

    fn pointer_moved(&mut self, x: f32, y: f32) -> bool {
        self.on_pointer_moved(x, y)
    }

    fn pointer_left(&mut self) -> bool {
        self.on_pointer_left()
    }

    fn click(&mut self, _engine: &mut TextEngine, x: f32, y: f32, ctrl: bool) -> bool {
        self.on_click(x, y, ctrl)
    }

    fn double_click(&mut self, _engine: &mut TextEngine, x: f32, y: f32) -> bool {
        self.on_double_click(x, y)
    }

    fn drag(&mut self, _engine: &mut TextEngine, x: f32, y: f32) -> bool {
        self.on_drag(x, y)
    }

    fn mouse_up(&mut self) -> bool {
        self.on_mouse_up()
    }

    fn scroll_by(&mut self, delta: f32, mods: ModifiersState) -> bool {
        self.on_scroll_by(delta, mods)
    }

    fn navigate_history(&mut self, delta: i32) -> bool {
        self.on_navigate_history(delta)
    }

    fn rich_mouse(&mut self, _engine: &mut TextEngine, event: RichMouseEvent) -> bool {
        self.on_rich_mouse(event)
    }

    fn right_click(&mut self, _engine: &mut TextEngine, x: f32, y: f32) -> bool {
        self.on_right_click(x, y)
    }

    fn key(
        &mut self,
        _engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        self.on_key(key, text, mods)
    }

    fn physical_key_pressed(
        &mut self,
        _engine: &mut TextEngine,
        physical: PhysicalKey,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
        repeat: bool,
    ) -> bool {
        self.on_physical_key_pressed(physical, key, text, mods, repeat)
    }

    fn physical_key_released(&mut self, physical: PhysicalKey) -> bool {
        self.on_physical_key_released(physical)
    }

    fn ime(&mut self, _engine: &mut TextEngine, event: ImeEvent<'_>) -> bool {
        self.on_ime(event)
    }

    fn dropped_file(&mut self, path: &Path) -> bool {
        self.on_dropped_file(path)
    }

    fn take_window_request(&mut self) -> Option<WindowRequest> {
        self.on_take_window_request()
    }

    fn take_key_quarantine(&mut self) -> Vec<PhysicalKey> {
        self.on_take_key_quarantine()
    }

    fn maintenance_deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn maintain(&mut self, now: Instant) -> bool {
        self.on_maintain(now)
    }

    fn animating(&self) -> bool {
        self.animating
    }

    fn draw(&mut self, _scene: &mut Scene, _engine: &mut TextEngine, _w: f32, _h: f32) {}

    fn presented(&mut self) {
        self.on_presented();
    }
}

impl MultiWindowApp for Recorder {
    type Event = ();

    fn windows(&self) -> &[WindowKey] {
        &self.windows
    }

    fn window_activation(&self, _key: WindowKey) -> crate::WindowActivation {
        self.activation
    }

    fn window_generation(&self, _key: WindowKey) -> u64 {
        self.generation
    }

    fn should_exit(&self) -> bool {
        false
    }

    fn close_requested(&mut self, key: WindowKey) {
        self.keys.push(key);
        self.on_close_requested();
    }

    fn wants_drag(&self, _key: WindowKey) -> bool {
        self.wants_drag
    }

    fn wants_rich_mouse(&self, _key: WindowKey) -> bool {
        self.wants_rich_mouse
    }

    fn is_caption(&self, _key: WindowKey, _x: f32, _y: f32) -> bool {
        self.caption
    }

    fn cursor_icon(&self, _key: WindowKey, _x: f32, _y: f32) -> CursorIcon {
        self.icon
    }

    fn resize_direction(
        &self,
        _key: WindowKey,
        _width: f32,
        _height: f32,
        _x: f32,
        _y: f32,
        _maximised: bool,
    ) -> Option<ResizeDirection> {
        self.edge
    }

    fn set_maximised(&mut self, key: WindowKey, maximised: bool) -> bool {
        self.keys.push(key);
        self.on_set_maximised(maximised)
    }

    fn set_pinned(&mut self, key: WindowKey, pinned: bool) -> bool {
        self.keys.push(key);
        self.on_set_pinned(pinned)
    }

    fn moved(&mut self, key: WindowKey, position: PhysicalPosition<i32>) {
        self.keys.push(key);
        self.on_moved(position);
    }

    fn resized(&mut self, key: WindowKey, size: PhysicalSize<u32>, scale_factor: f64) {
        self.keys.push(key);
        self.on_resized(size, scale_factor);
    }

    fn scale_factor_changed(&mut self, key: WindowKey, scale_factor: f64) -> bool {
        self.keys.push(key);
        self.on_scale_factor_changed(scale_factor)
    }

    fn occluded(&mut self, key: WindowKey, occluded: bool) {
        self.keys.push(key);
        self.on_occluded(occluded);
    }

    fn focus_changed(&mut self, key: WindowKey, focused: bool) -> bool {
        self.keys.push(key);
        self.on_focus_changed(focused)
    }

    fn system_theme_changed(&mut self, key: WindowKey, dark: bool) -> bool {
        self.keys.push(key);
        self.on_system_theme_changed(dark)
    }

    fn pointer_moved(&mut self, key: WindowKey, x: f32, y: f32) -> bool {
        self.keys.push(key);
        self.on_pointer_moved(x, y)
    }

    fn pointer_left(&mut self, key: WindowKey) -> bool {
        self.keys.push(key);
        self.on_pointer_left()
    }

    fn click(
        &mut self,
        key: WindowKey,
        _engine: &mut TextEngine,
        x: f32,
        y: f32,
        ctrl: bool,
    ) -> bool {
        self.keys.push(key);
        self.on_click(x, y, ctrl)
    }

    fn double_click(&mut self, key: WindowKey, _engine: &mut TextEngine, x: f32, y: f32) -> bool {
        self.keys.push(key);
        self.on_double_click(x, y)
    }

    fn drag(&mut self, key: WindowKey, _engine: &mut TextEngine, x: f32, y: f32) -> bool {
        self.keys.push(key);
        self.on_drag(x, y)
    }

    fn mouse_up(&mut self, key: WindowKey) -> bool {
        self.keys.push(key);
        self.on_mouse_up()
    }

    fn scroll_by(&mut self, key: WindowKey, delta: f32, mods: ModifiersState) -> bool {
        self.keys.push(key);
        self.on_scroll_by(delta, mods)
    }

    fn navigate_history(&mut self, key: WindowKey, delta: i32) -> bool {
        self.keys.push(key);
        self.on_navigate_history(delta)
    }

    fn rich_mouse(
        &mut self,
        key: WindowKey,
        _engine: &mut TextEngine,
        event: RichMouseEvent,
    ) -> bool {
        self.keys.push(key);
        self.on_rich_mouse(event)
    }

    fn right_click(&mut self, key: WindowKey, _engine: &mut TextEngine, x: f32, y: f32) -> bool {
        self.keys.push(key);
        self.on_right_click(x, y)
    }

    fn key(
        &mut self,
        key: WindowKey,
        _engine: &mut TextEngine,
        logical: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        self.keys.push(key);
        self.on_key(logical, text, mods)
    }

    fn physical_key_pressed(
        &mut self,
        key: WindowKey,
        _engine: &mut TextEngine,
        physical: PhysicalKey,
        logical: &Key,
        text: Option<&str>,
        mods: ModifiersState,
        repeat: bool,
    ) -> bool {
        self.keys.push(key);
        self.on_physical_key_pressed(physical, logical, text, mods, repeat)
    }

    fn physical_key_released(&mut self, key: WindowKey, physical: PhysicalKey) -> bool {
        self.keys.push(key);
        self.on_physical_key_released(physical)
    }

    fn ime(&mut self, key: WindowKey, _engine: &mut TextEngine, event: ImeEvent<'_>) -> bool {
        self.keys.push(key);
        self.on_ime(event)
    }

    fn dropped_file(&mut self, key: WindowKey, path: &Path) -> bool {
        self.keys.push(key);
        self.on_dropped_file(path)
    }

    fn take_window_request(&mut self, key: WindowKey) -> Option<WindowRequest> {
        self.keys.push(key);
        self.on_take_window_request()
    }

    fn take_key_quarantine(&mut self) -> Vec<PhysicalKey> {
        self.on_take_key_quarantine()
    }

    fn maintenance_deadline(&self, _key: WindowKey) -> Option<Instant> {
        self.deadline
    }

    fn maintain(&mut self, key: WindowKey, now: Instant) -> bool {
        self.keys.push(key);
        self.on_maintain(now)
    }

    fn animating(&self, _key: WindowKey) -> bool {
        self.animating
    }

    fn draw(
        &mut self,
        _key: WindowKey,
        _scene: &mut Scene,
        _engine: &mut TextEngine,
        _w: f32,
        _h: f32,
    ) {
    }

    fn presented(&mut self, key: WindowKey) {
        self.keys.push(key);
        self.on_presented();
    }
}

/// One thing that happens to a window, owning whatever it borrows.
///
/// An owned script rather than a `Vec<Input>` because a key event borrows its
/// logical key and its text.
enum Step {
    CursorMoved(f64, f64),
    CursorLeft,
    Press(MouseButton),
    Release(MouseButton),
    Mods(ModifiersState),
    Focus(bool),
    Occluded(bool),
    Theme(bool),
    Resize(u32, u32),
    ScaleFactor(f64),
    Moved(i32, i32),
    Wheel(f32),
    KeyDown {
        physical: PhysicalKey,
        logical: Key,
        text: Option<String>,
        repeat: bool,
    },
    KeyUp {
        physical: PhysicalKey,
        logical: Key,
    },
    Ime(ImeStep),
    Drop(PathBuf),
    Close,
    /// One settle pass, as both runners perform it: drain the quarantine the
    /// app asked for, maintain, then decide the control flow.
    Settle,
    /// Move the recorded press back in time, so double-click timing can be
    /// asserted without sleeping through it.
    BackdatePress(Duration),
}

enum ImeStep {
    Enabled,
    Preedit(String, Option<(usize, usize)>),
    Commit(String),
    Disabled,
}

fn as_input<R>(step: &Step, act: impl FnOnce(Input<'_>) -> R) -> Option<R> {
    let input = match step {
        Step::CursorMoved(x, y) => Input::CursorMoved { x: *x, y: *y },
        Step::CursorLeft => Input::CursorLeft,
        Step::Press(button) => Input::Mouse {
            state: ElementState::Pressed,
            button: *button,
        },
        Step::Release(button) => Input::Mouse {
            state: ElementState::Released,
            button: *button,
        },
        Step::Mods(mods) => Input::ModifiersChanged(*mods),
        Step::Focus(focused) => Input::Focused(*focused),
        Step::Occluded(occluded) => Input::Occluded(*occluded),
        Step::Theme(dark) => Input::ThemeChanged(*dark),
        Step::Resize(w, h) => Input::Resized(PhysicalSize::new(*w, *h)),
        Step::ScaleFactor(scale) => Input::ScaleFactorChanged(*scale),
        Step::Moved(x, y) => Input::Moved(PhysicalPosition::new(*x, *y)),
        Step::Wheel(lines) => Input::Wheel(MouseScrollDelta::LineDelta(0.0, *lines)),
        Step::KeyDown {
            physical,
            logical,
            text,
            repeat,
        } => Input::Key(KeyInput {
            physical_key: *physical,
            logical_key: logical,
            text: text.as_deref(),
            location: KeyLocation::Standard,
            state: ElementState::Pressed,
            repeat: *repeat,
        }),
        Step::KeyUp { physical, logical } => Input::Key(KeyInput {
            physical_key: *physical,
            logical_key: logical,
            text: None,
            location: KeyLocation::Standard,
            state: ElementState::Released,
            repeat: false,
        }),
        Step::Ime(ime) => Input::Ime(match ime {
            ImeStep::Enabled => ImeEvent::Enabled,
            ImeStep::Preedit(text, cursor) => ImeEvent::Preedit {
                text,
                cursor: *cursor,
            },
            ImeStep::Commit(text) => ImeEvent::Commit { text },
            ImeStep::Disabled => ImeEvent::Disabled,
        }),
        Step::Drop(path) => Input::DroppedFile(path),
        Step::Close => Input::Close,
        Step::Settle | Step::BackdatePress(_) => return None,
    };
    Some(act(input))
}

/// One line per step: what the app was asked, what the window was told, and
/// what the runner is left holding.
type Trace = Vec<String>;

/// Joined rather than `{:?}`-ed: a trace read by a person, and one an
/// assertion can name a fragment of without counting escaped quotes.
fn line(app: &mut Recorder, window: &FakeWindow, tail: String) -> String {
    format!(
        "app=[{}] win=[{}] {tail}",
        app.taken().join(" · "),
        window.taken().join(" · ")
    )
}

/// The flow's *kind*, not its deadline: two runs of the same script name two
/// different instants, and the moment is the app's answer rather than the
/// runner's decision.
fn flow_kind(flow: ControlFlow) -> &'static str {
    match flow {
        ControlFlow::Poll => "Poll",
        ControlFlow::Wait => "Wait",
        ControlFlow::WaitUntil(_) => "WaitUntil",
    }
}

fn backdate(state: &mut WindowInput, by: Duration) {
    if let Some((at, x, y, count)) = state.last_press {
        state.last_press = Some((at - by, x, y, count));
    }
}

fn drive_single(steps: &[Step], arm: impl Fn(&mut Recorder)) -> Trace {
    let mut app = Recorder::new();
    arm(&mut app);
    let mut text = TextEngine::new();
    let mut state = WindowInput::default();
    let mut quarantine = KeyQuarantine::default();
    let window = FakeWindow::default();
    let mut trace = Trace::new();
    for step in steps {
        let tail = match step {
            Step::Settle => {
                let asked = WindowApp::take_key_quarantine(&mut app);
                if !asked.is_empty() {
                    quarantine.suppress_all(asked, Instant::now());
                }
                let settled = maintain_window(&mut Single(&mut app), Some(&window), Instant::now());
                format!(
                    "flow={} animating={}",
                    flow_kind(next_flow(settled.animating, settled.deadline)),
                    settled.animating
                )
            }
            Step::BackdatePress(by) => {
                backdate(&mut state, *by);
                String::from("backdated")
            }
            step => as_input(step, |input| {
                let after = deliver(
                    &mut Single(&mut app),
                    &mut text,
                    &mut state,
                    &mut quarantine,
                    Some(&window),
                    input,
                );
                format!("after={after:?}")
            })
            .expect("every remaining step is an input"),
        };
        trace.push(line(&mut app, &window, tail));
    }
    trace
}

fn drive_multi(steps: &[Step], arm: impl Fn(&mut Recorder)) -> Trace {
    let mut app = Recorder::new();
    arm(&mut app);
    let mut text = TextEngine::new();
    let mut state = WindowInput::default();
    let mut quarantine = KeyQuarantine::default();
    let window = FakeWindow::default();
    let mut trace = Trace::new();
    for step in steps {
        let tail = match step {
            Step::Settle => {
                let asked = MultiWindowApp::take_key_quarantine(&mut app);
                if !asked.is_empty() {
                    quarantine.suppress_all(asked, Instant::now());
                }
                // One window, so the fold over the set is the identity — which
                // is the point: the same numbers must come out.
                let settled = maintain_window(
                    &mut WindowTarget {
                        app: &mut app,
                        key: KEY,
                    },
                    Some(&window),
                    Instant::now(),
                );
                let animating = settled.animating;
                format!(
                    "flow={} animating={animating}",
                    flow_kind(next_flow(animating, settled.deadline))
                )
            }
            Step::BackdatePress(by) => {
                backdate(&mut state, *by);
                String::from("backdated")
            }
            step => as_input(step, |input| {
                let after = deliver(
                    &mut WindowTarget {
                        app: &mut app,
                        key: KEY,
                    },
                    &mut text,
                    &mut state,
                    &mut quarantine,
                    Some(&window),
                    input,
                );
                format!("after={after:?}")
            })
            .expect("every remaining step is an input"),
        };
        trace.push(line(&mut app, &window, tail));
        assert!(
            app.keys.iter().all(|key| *key == KEY),
            "the multi-window adapter routed to a window nobody named"
        );
    }
    trace
}

/// Drive one sequence through both runners and hand back the trace they agree
/// on. The equality is the gate; each caller then pins the content.
#[track_caller]
fn parity(steps: &[Step], arm: impl Fn(&mut Recorder) + Copy) -> Trace {
    let single = drive_single(steps, arm);
    let multi = drive_multi(steps, arm);
    assert_eq!(
        single, multi,
        "the two runners answered the same events differently"
    );
    single
}

fn no_arming(_: &mut Recorder) {}

fn key_of(letter: &str) -> Key {
    Key::Character(letter.into())
}

// ---------------------------------------------------------------- pointer

#[test]
fn a_pointer_move_divides_by_the_scale_and_sets_the_cursor_once() {
    let trace = parity(
        &[
            Step::CursorMoved(200.0, 100.0),
            Step::CursorMoved(202.0, 100.0),
        ],
        |app| app.icon = CursorIcon::Text,
    );
    assert!(
        trace[0].contains("pointer_moved 200 100"),
        "a 1.0 scale divides by one: {}",
        trace[0]
    );
    assert!(
        trace[0].contains("set_cursor Text"),
        "the first move installs the icon the app names: {}",
        trace[0]
    );
    assert!(
        !trace[1].contains("set_cursor"),
        "an unchanged icon is not re-sent; `CursorMoved` is far more frequent \
         than a shape transition: {}",
        trace[1]
    );
}

#[test]
fn a_pointer_leave_reaches_both_runners() {
    // The gap SP-0 found: winit sends `CursorLeft` and the loop dropped it, so
    // nothing could clear a hover.
    let trace = parity(&[Step::CursorLeft], no_arming);
    assert!(trace[0].contains("pointer_left"), "{}", trace[0]);
    assert!(trace[0].contains("invalidate: Some(true)"), "{}", trace[0]);
}

#[test]
fn a_press_that_wants_drag_captures_the_pointer_in_both_runners() {
    let trace = parity(
        &[
            Step::CursorMoved(10.0, 10.0),
            Step::Press(MouseButton::Left),
            Step::CursorMoved(40.0, 10.0),
            Step::Release(MouseButton::Left),
            Step::CursorMoved(60.0, 10.0),
        ],
        |app| app.wants_drag = true,
    );
    assert!(trace[2].contains("drag 40 10"), "{}", trace[2]);
    assert!(trace[3].contains("mouse_up"), "{}", trace[3]);
    assert!(
        trace[4].contains("pointer_moved 60 10"),
        "the release ends the capture: {}",
        trace[4]
    );
}

#[test]
fn without_wants_drag_a_held_move_is_still_only_a_move_in_both_runners() {
    let trace = parity(
        &[
            Step::CursorMoved(10.0, 10.0),
            Step::Press(MouseButton::Left),
            Step::CursorMoved(40.0, 10.0),
        ],
        no_arming,
    );
    assert!(trace[2].contains("pointer_moved 40 10"), "{}", trace[2]);
}

#[test]
fn the_resize_border_claims_a_press_before_the_caption_in_both_runners() {
    let trace = parity(
        &[Step::CursorMoved(1.0, 1.0), Step::Press(MouseButton::Left)],
        |app| {
            app.edge = Some(ResizeDirection::NorthWest);
            app.caption = true;
        },
    );
    assert!(
        trace[0].contains("set_cursor NwResize"),
        "the edge owns the cursor: {}",
        trace[0]
    );
    assert!(
        trace[1].contains("drag_resize_window NorthWest"),
        "{}",
        trace[1]
    );
    assert!(
        !trace[1].contains("drag_window") && !trace[1].contains("click"),
        "neither the caption nor the content sees a press on the border: {}",
        trace[1]
    );
}

#[test]
fn a_caption_press_drags_the_window_inside_the_press_in_both_runners() {
    let trace = parity(
        &[Step::CursorMoved(30.0, 4.0), Step::Press(MouseButton::Left)],
        |app| app.caption = true,
    );
    assert!(trace[1].contains("drag_window"), "{}", trace[1]);
    assert!(
        !trace[1].contains("click"),
        "a surface never learns a caption exists: {}",
        trace[1]
    );
}

#[test]
fn click_counting_matches_across_runners_for_timing_and_slop() {
    let trace = parity(
        &[
            // Two presses close in time and space: the second is a double.
            Step::CursorMoved(50.0, 50.0),
            Step::Press(MouseButton::Left),
            Step::Press(MouseButton::Left),
            // A press far away starts again even though it is soon.
            Step::CursorMoved(400.0, 50.0),
            Step::Press(MouseButton::Left),
            // And one that is near but late.
            Step::BackdatePress(Duration::from_millis(1_000)),
            Step::Press(MouseButton::Left),
        ],
        no_arming,
    );
    assert!(trace[1].contains("click 50 50"), "{}", trace[1]);
    assert!(trace[2].contains("double_click 50 50"), "{}", trace[2]);
    assert!(
        trace[4].contains("click 400 50"),
        "four pixels of slop, not four hundred: {}",
        trace[4]
    );
    assert!(
        trace[6].contains("click 400 50"),
        "a late second press is a click of its own: {}",
        trace[6]
    );
}

#[test]
fn triple_clicks_reach_a_rich_surface_and_stop_at_three_in_both_runners() {
    let trace = parity(
        &[
            Step::CursorMoved(50.0, 50.0),
            Step::Press(MouseButton::Left),
            Step::Press(MouseButton::Left),
            Step::Press(MouseButton::Left),
            Step::Press(MouseButton::Left),
        ],
        |app| app.wants_rich_mouse = true,
    );
    assert!(trace[1].contains("count=1"), "{}", trace[1]);
    assert!(trace[2].contains("count=2"), "{}", trace[2]);
    assert!(trace[3].contains("count=3"), "{}", trace[3]);
    assert!(
        trace[4].contains("count=1"),
        "the count is capped at three and then starts over: {}",
        trace[4]
    );
}

#[test]
fn middle_and_right_transitions_are_invisible_without_rich_mouse_in_both_runners() {
    let plain = parity(
        &[
            Step::CursorMoved(5.0, 5.0),
            Step::Press(MouseButton::Middle),
            Step::Release(MouseButton::Middle),
            Step::Release(MouseButton::Right),
        ],
        no_arming,
    );
    for step in &plain[1..] {
        assert!(step.contains("app=[]"), "{step}");
    }
    let rich = parity(
        &[
            Step::CursorMoved(5.0, 5.0),
            Step::Press(MouseButton::Middle),
            Step::Release(MouseButton::Middle),
            Step::Release(MouseButton::Right),
        ],
        |app| app.wants_rich_mouse = true,
    );
    assert!(rich[1].contains("rich_mouse Middle Pressed"), "{}", rich[1]);
    assert!(
        rich[2].contains("rich_mouse Middle Released"),
        "{}",
        rich[2]
    );
    assert!(rich[3].contains("rich_mouse Right Released"), "{}", rich[3]);
}

/// **The thumb buttons walk history in both runners** (koru-ai D102): Back
/// is a step back, Forward a step forward, the release is nobody's, and a
/// rich-mouse surface is not handed them as buttons it cannot report.
#[test]
fn thumb_buttons_walk_history_in_both_runners() {
    for rich in [false, true] {
        let trace = parity(
            &[
                Step::CursorMoved(5.0, 5.0),
                Step::Press(MouseButton::Back),
                Step::Release(MouseButton::Back),
                Step::Press(MouseButton::Forward),
            ],
            |app| app.wants_rich_mouse = rich,
        );
        assert!(trace[1].contains("navigate_history -1"), "{}", trace[1]);
        assert!(trace[2].contains("app=[]"), "{}", trace[2]);
        assert!(trace[3].contains("navigate_history 1"), "{}", trace[3]);
    }
}

// --------------------------------------------------------------- keyboard

#[test]
fn a_quarantined_physical_key_is_withheld_from_both_runners_until_release() {
    let trace = parity(
        &[
            // The app names the key when it opens the thing that must not
            // receive it; the drain happens in settle.
            Step::Settle,
            Step::KeyDown {
                physical: PhysicalKey::Code(KeyCode::KeyK),
                logical: key_of("k"),
                text: Some("k".into()),
                repeat: true,
            },
            Step::KeyUp {
                physical: PhysicalKey::Code(KeyCode::KeyK),
                logical: key_of("k"),
            },
            Step::KeyDown {
                physical: PhysicalKey::Code(KeyCode::KeyK),
                logical: key_of("k"),
                text: Some("k".into()),
                repeat: false,
            },
        ],
        |app| app.quarantine = vec![PhysicalKey::Code(KeyCode::KeyK)],
    );
    assert!(trace[0].contains("take_key_quarantine"), "{}", trace[0]);
    assert!(
        trace[1].contains("app=[]"),
        "the repeat of the chord that summoned the window is swallowed: {}",
        trace[1]
    );
    assert!(
        trace[2].contains("app=[]"),
        "so is its release, which is what ends the quarantine: {}",
        trace[2]
    );
    assert!(
        trace[3].contains("key Character(\"k\")"),
        "and the next real press types: {}",
        trace[3]
    );
}

#[test]
fn modifiers_ride_every_key_and_clear_on_both_focus_edges_in_both_runners() {
    let shift = ModifiersState::SHIFT;
    let trace = parity(
        &[
            Step::Mods(shift),
            Step::KeyDown {
                physical: PhysicalKey::Code(KeyCode::KeyA),
                logical: key_of("A"),
                text: Some("A".into()),
                repeat: false,
            },
            Step::Focus(false),
            Step::KeyDown {
                physical: PhysicalKey::Code(KeyCode::KeyA),
                logical: key_of("a"),
                text: Some("a".into()),
                repeat: false,
            },
            Step::Mods(shift),
            Step::Focus(true),
            Step::KeyDown {
                physical: PhysicalKey::Code(KeyCode::KeyA),
                logical: key_of("a"),
                text: Some("a".into()),
                repeat: false,
            },
        ],
        no_arming,
    );
    assert!(trace[1].contains("SHIFT"), "{}", trace[1]);
    assert!(
        !trace[3].contains("SHIFT"),
        "losing the focus clears the chord nobody will see released: {}",
        trace[3]
    );
    assert!(
        !trace[6].contains("SHIFT"),
        "and so does regaining it: {}",
        trace[6]
    );
}

#[test]
fn a_consumed_physical_press_never_types_its_own_letter_in_either_runner() {
    let trace = parity(
        &[Step::KeyDown {
            physical: PhysicalKey::Code(KeyCode::Space),
            logical: Key::Named(NamedKey::Space),
            text: Some(" ".into()),
            repeat: false,
        }],
        |app| app.consume_physical = true,
    );
    assert!(trace[0].contains("physical_key_pressed"), "{}", trace[0]);
    assert!(
        !trace[0].contains("key Named"),
        "a hold gesture that claimed the press must not also insert a space: {}",
        trace[0]
    );
    assert!(trace[0].contains("invalidate: Some(true)"), "{}", trace[0]);
}

#[test]
fn an_unidentified_altgr_p_is_repaired_for_both_runners_and_only_on_the_press() {
    let unidentified = Key::Unidentified(NativeKeyCode::Unidentified.into());
    let trace = parity(
        &[
            Step::Mods(ModifiersState::CONTROL | ModifiersState::ALT),
            Step::KeyDown {
                physical: PhysicalKey::Code(KeyCode::KeyP),
                logical: unidentified.clone(),
                text: None,
                repeat: false,
            },
            Step::KeyUp {
                physical: PhysicalKey::Code(KeyCode::KeyP),
                logical: unidentified,
            },
        ],
        no_arming,
    );
    assert!(
        trace[1].contains("physical_key_pressed Code(KeyP) Character(\"p\")"),
        "the repair happens before anything sees the key: {}",
        trace[1]
    );
    assert!(
        trace[2].contains("physical_key_released Code(KeyP)") && !trace[2].contains("key "),
        "the release is offered as a physical key and then dropped by the legacy \
         `rich_key` adapter — it is not repaired, because only the press is: {}",
        trace[2]
    );
}

// -------------------------------------------------------------------- IME

#[test]
fn ime_preedit_and_commit_keep_their_order_and_offsets_in_both_runners() {
    let trace = parity(
        &[
            Step::Ime(ImeStep::Enabled),
            Step::Ime(ImeStep::Preedit("に".into(), Some((0, 3)))),
            Step::Ime(ImeStep::Preedit("にほ".into(), Some((0, 6)))),
            Step::Ime(ImeStep::Commit("日本".into())),
            Step::Ime(ImeStep::Disabled),
        ],
        no_arming,
    );
    assert!(trace[0].contains("ime Enabled"), "{}", trace[0]);
    assert!(
        trace[1].contains("Preedit { text: \"に\", cursor: Some((0, 3)) }"),
        "the byte-indexed cursor survives: {}",
        trace[1]
    );
    assert!(trace[2].contains("にほ"), "{}", trace[2]);
    assert!(
        trace[3].contains("Commit { text: \"日本\" }"),
        "{}",
        trace[3]
    );
    assert!(trace[4].contains("ime Disabled"), "{}", trace[4]);
}

// ----------------------------------------------------------------- window

#[test]
fn a_resize_pushes_maximised_before_the_size_in_both_runners() {
    let trace = parity(&[Step::Resize(1024, 768)], no_arming);
    let maximised = trace[0].find("set_maximised false");
    let resized = trace[0].find("resized 1024x768 scale=1");
    assert!(
        maximised < resized && maximised.is_some(),
        "a maximise from Win+Arrow, a snap layout or the taskbar has to reach \
         the caption before the new size does: {}",
        trace[0]
    );
    assert!(
        trace[0].contains("work: Resize(PhysicalSize { width: 1024, height: 768 })"),
        "the surface reconfiguration is the runner's, and it is asked for: {}",
        trace[0]
    );
}

#[test]
fn a_dpi_change_with_no_resize_reaches_both_runners() {
    // The second gap SP-0 found: the loop only ever re-read the scale on a
    // resize, so a window dragged to a display at another scale saw nothing.
    let trace = parity(&[Step::ScaleFactor(1.5)], no_arming);
    assert!(
        trace[0].contains("scale_factor_changed 1.5"),
        "{}",
        trace[0]
    );
    assert!(trace[0].contains("invalidate: Some(true)"), "{}", trace[0]);
}

#[test]
fn a_theme_change_reapplies_the_window_frame_in_both_runners() {
    let changed = parity(&[Step::Theme(true)], |app| app.theme_changes = true);
    assert!(
        changed[0].contains("system_theme_changed dark=true"),
        "{}",
        changed[0]
    );
    assert!(
        changed[0].contains("apply_frame false"),
        "the DWM border colour follows the palette: {}",
        changed[0]
    );
    let ignored = parity(&[Step::Theme(true)], no_arming);
    assert!(
        ignored[0].contains("win=[]"),
        "an app with an explicit light or dark choice re-applies nothing: {}",
        ignored[0]
    );
}

#[test]
fn a_wheel_notch_is_a_line_of_body_text_in_both_runners() {
    let trace = parity(
        &[Step::Mods(ModifiersState::CONTROL), Step::Wheel(1.0)],
        no_arming,
    );
    let expected = koru_draw::theme::text_metrics::DOC_BODY.line_height;
    assert!(
        trace[1].contains(&format!("scroll_by {expected}")),
        "{}",
        trace[1]
    );
    assert!(
        trace[1].contains("CONTROL"),
        "primary+wheel display zoom needs the modifiers: {}",
        trace[1]
    );
}

#[test]
fn window_facts_that_change_nothing_drawn_ask_for_no_frame_in_either_runner() {
    let trace = parity(&[Step::Moved(300, 120), Step::Occluded(true)], no_arming);
    for step in &trace {
        assert!(
            step.contains("invalidate: None"),
            "one event per pixel during a drag must not be one frame per pixel: {step}"
        );
    }
    assert!(trace[0].contains("moved 300 120"), "{}", trace[0]);
    assert!(trace[1].contains("occluded true"), "{}", trace[1]);
}

#[test]
fn a_close_request_settles_without_deciding_anything_in_either_runner() {
    let trace = parity(&[Step::Close], no_arming);
    assert!(trace[0].contains("close_requested"), "{}", trace[0]);
    assert!(trace[0].contains("settle: true"), "{}", trace[0]);
    assert!(
        trace[0].contains("win=[]"),
        "what closing means is the app's, and the runner touches no window for it: {}",
        trace[0]
    );
}

#[test]
fn a_dropped_file_arrives_once_per_path_in_both_runners() {
    let trace = parity(
        &[
            Step::Drop(PathBuf::from("first.txt")),
            Step::Drop(PathBuf::from("second.txt")),
        ],
        no_arming,
    );
    assert!(trace[0].contains("dropped_file first.txt"), "{}", trace[0]);
    assert!(trace[1].contains("dropped_file second.txt"), "{}", trace[1]);
}

#[test]
fn a_window_request_is_drained_inside_the_press_in_both_runners() {
    let trace = parity(
        &[Step::CursorMoved(30.0, 4.0), Step::Press(MouseButton::Left)],
        |app| app.request = Some(WindowRequest::Minimise),
    );
    assert!(
        trace[1].contains("take_window_request Minimise"),
        "{}",
        trace[1]
    );
    assert!(trace[1].contains("set_minimized true"), "{}", trace[1]);
}

#[test]
fn a_passive_window_consumes_focus_requests_without_native_activation() {
    let trace = parity(
        &[Step::CursorMoved(30.0, 4.0), Step::Press(MouseButton::Left)],
        |app| {
            app.activation = crate::WindowActivation::Passive;
            app.request = Some(WindowRequest::Focus);
        },
    );
    assert!(
        trace[1].contains("take_window_request Focus"),
        "{}",
        trace[1]
    );
    assert!(!trace[1].contains("set_minimized"), "{}", trace[1]);
    assert!(!trace[1].contains("focus_window"), "{}", trace[1]);
    assert!(!trace[1].contains("activate_app"), "{}", trace[1]);
}

#[test]
fn a_passive_window_consumes_every_runner_owned_style_request() {
    for request in [
        WindowRequest::Drag,
        WindowRequest::Focus,
        WindowRequest::Minimise,
        WindowRequest::ToggleMaximise,
        WindowRequest::TogglePin,
        WindowRequest::Place(Placement::Reset),
        WindowRequest::PlaceAndLockExact {
            width: 1280,
            height: 720,
        },
        WindowRequest::LockCurrentSize,
        WindowRequest::UnlockSizeAndRestore,
        WindowRequest::StartRecording(crate::RecordingRequest {
            clip: std::path::PathBuf::from("C:\\videos\\koru.mp4"),
            frame_rate: 60,
            cursor: true,
        }),
        WindowRequest::StopRecording,
    ] {
        let trace = parity(
            &[Step::CursorMoved(30.0, 4.0), Step::Press(MouseButton::Left)],
            |app| {
                app.activation = crate::WindowActivation::Passive;
                app.request = Some(request.clone());
            },
        );
        assert!(
            trace[1].contains(&format!("take_window_request {request:?}")),
            "{}",
            trace[1]
        );
        assert!(trace[1].contains("win=[]"), "{request:?}: {}", trace[1]);
    }
}

#[test]
fn a_passive_theme_change_never_rewrites_native_window_styles() {
    let trace = parity(&[Step::Theme(true)], |app| {
        app.activation = crate::WindowActivation::Passive;
        app.theme_changes = true;
    });
    assert!(
        trace[0].contains("system_theme_changed dark=true"),
        "{}",
        trace[0]
    );
    assert!(!trace[0].contains("apply_frame"), "{}", trace[0]);
}

#[test]
fn a_pin_request_toggles_the_level_and_reports_the_state_in_both_runners() {
    let trace = parity(
        &[
            Step::CursorMoved(30.0, 4.0),
            Step::Press(MouseButton::Left),
            Step::Press(MouseButton::Left),
        ],
        |app| app.request = Some(WindowRequest::TogglePin),
    );
    assert!(trace[1].contains("set_always_on_top true"), "{}", trace[1]);
    assert!(trace[1].contains("set_pinned true"), "{}", trace[1]);
    assert!(
        !trace[2].contains("set_always_on_top"),
        "the request is drained, not repeated: {}",
        trace[2]
    );
}

#[test]
fn a_close_or_hide_request_is_declined_by_both_runners() {
    let trace = parity(
        &[Step::CursorMoved(30.0, 4.0), Step::Press(MouseButton::Left)],
        |app| app.request = Some(WindowRequest::Close),
    );
    assert!(
        trace[1].contains("take_window_request Close"),
        "{}",
        trace[1]
    );
    assert!(
        trace[1].contains("win=[]"),
        "whether closing ends the process is a fact about the app: {}",
        trace[1]
    );
}

// ------------------------------------------------------- clock and frames

#[test]
fn a_settle_drains_maintenance_and_reschedules_the_same_way_in_both_runners() {
    let idle = parity(&[Step::Settle], no_arming);
    assert!(idle[0].contains("maintain"), "{}", idle[0]);
    assert!(
        idle[0].contains("flow=Wait") && idle[0].contains("win=[]"),
        "nothing pending is zero CPU and no frame: {}",
        idle[0]
    );

    let animating = parity(&[Step::Settle], |app| app.animating = true);
    assert!(
        animating[0].contains("flow=Poll") && animating[0].contains("request_redraw"),
        "an animation asks for the next frame outright: {}",
        animating[0]
    );

    let deadline = parity(&[Step::Settle], |app| {
        app.deadline = Some(Instant::now() + Duration::from_millis(50));
    });
    assert!(
        deadline[0].contains("flow=WaitUntil"),
        "an app with work in flight names a moment: {}",
        deadline[0]
    );
    assert!(
        deadline[0].contains("win=[]"),
        "naming a moment is not asking for a frame: {}",
        deadline[0]
    );
}

/// Present acknowledgement is the user-visible latency sample, so it must
/// happen once per frame in both runners and only after the frame landed.
///
/// The blit needs a device; what is GPU-free — and what actually differs
/// between the runners — is the routing, so the hook is driven through both
/// adapters directly.
#[test]
fn a_presented_frame_is_acknowledged_through_both_runner_adapters() {
    let mut app = Recorder::new();
    DispatchApp::presented(&mut Single(&mut app));
    assert_eq!(app.taken(), ["presented"]);
    assert!(app.keys.is_empty());

    DispatchApp::presented(&mut WindowTarget {
        app: &mut app,
        key: KEY,
    });
    assert_eq!(app.taken(), ["presented"]);
    assert_eq!(
        app.keys,
        [KEY],
        "the multi-window runner acknowledges for the window that presented"
    );
}

#[test]
fn activation_and_window_generation_have_single_and_multi_runner_parity() {
    let mut app = Recorder {
        activation: crate::WindowActivation::Passive,
        generation: 42,
        ..Recorder::new()
    };
    assert_eq!(
        DispatchApp::window_activation(&Single(&mut app)),
        crate::WindowActivation::Passive
    );
    assert_eq!(WindowApp::window_generation(&app), 42);

    assert_eq!(
        DispatchApp::window_activation(&WindowTarget {
            app: &mut app,
            key: KEY,
        }),
        crate::WindowActivation::Passive
    );
    assert_eq!(MultiWindowApp::window_generation(&app, KEY), 42);
}

#[test]
fn a_redraw_request_asks_the_runner_for_a_frame_and_nothing_else() {
    let mut app = Recorder::new();
    let mut text = TextEngine::new();
    let mut state = WindowInput::default();
    let mut quarantine = KeyQuarantine::default();
    let window = FakeWindow::default();
    let after = deliver(
        &mut Single(&mut app),
        &mut text,
        &mut state,
        &mut quarantine,
        Some(&window),
        Input::RedrawRequested,
    );
    assert_eq!(
        after,
        Aftermath {
            settle: false,
            invalidate: None,
            work: FrameWork::Redraw,
        }
    );
    assert!(app.taken().is_empty());
    assert!(window.taken().is_empty());
}
