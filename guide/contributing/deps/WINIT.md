# winit

Windows, input, and the event loop. `winit` 0.30.

New to Rust syntax? [RUST.md](../RUST.md) first.

---

## What it does, and what it deliberately does not

winit gives you a window and a stream of events. It draws nothing — no widgets,
no text, no canvas. That is the point: it is the same API on Windows, macOS,
Linux/X11, Linux/Wayland, Android and the web, and everything above it is yours.

For Koru that means winit provides the window, the keyboard, the mouse, the DPI
scale factor and the "please redraw" signal — and [vello](VELLO.md) fills it in.

## The inverted control flow

You do not write the loop. You hand winit a struct implementing
`ApplicationHandler` and it calls you:

```rust
// apps/koru/src/main.rs
impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) { … }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) { … }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) { … }
}

fn main() {
    let event_loop = EventLoop::new().expect("create event loop");
    event_loop.run_app(&mut App::new()).expect("run app");
}
```

This shape exists because some platforms *own* the loop — a browser and Android
will not let you block — so winit adopts the most restrictive model everywhere.

The three callbacks that matter here:

| | |
| --- | --- |
| `resumed` | the app became active. **Create windows here, not in `main`** — on Android the surface can be destroyed and recreated |
| `window_event` | something happened to a specific window, named by `WindowId` |
| `about_to_wait` | the queue is empty. Where the sync tick and the caret blink live |

## Windows are identified, not owned by the handler

Every `window_event` carries a `WindowId`. Ignoring it works until there are two
windows — which is why the app keys them:

```rust
// apps/koru/src/main.rs
panes: HashMap<WindowId, Pane>,
primary: Option<WindowId>,
```

Anything per-window belongs in the `Pane`, not in `App`. Cursor position and
last-click time were app-global before there were two windows, and would have
made them fight over the pointer.

Windows are `Arc<Window>` because the GPU surface holds one too.

## Events, and the ones that surprise people

```rust
match event {
    WindowEvent::CloseRequested => …,
    WindowEvent::Resized(size) => …,
    WindowEvent::ScaleFactorChanged { .. } => …,
    WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
    WindowEvent::KeyboardInput { event, .. } => …,
    WindowEvent::CursorMoved { position, .. } => …,
    WindowEvent::MouseInput { state, button, .. } => …,
    WindowEvent::MouseWheel { delta, .. } => …,
    WindowEvent::ThemeChanged(theme) => …,
    WindowEvent::RedrawRequested => self.redraw(id),
    _ => {}
}
```

- **`CloseRequested` is a request.** You decide. Here, closing the primary window
  exits; closing any other just removes that view.
- **`RedrawRequested` is the only place to draw after a window is visible.**
  Everywhere else you call `window.request_redraw()` and wait to be asked. The
  first frame is the deliberate exception: on Windows a hidden window receives
  no `WM_PAINT`, so both onboarding and workspace panes draw once directly,
  reveal only after a successful present, and then enter the event-driven path.
- **Modifiers arrive separately** from key presses, as their own event. You track
  the current state and consult it when a key arrives.
- **`ThemeChanged` is the OS light/dark flip** (Windows Personalisation, etc.).
  Koru applies it only while the welcome preference is **System**. On Windows,
  winit delivers the event **only when `window.set_theme(None)`** — a forced
  `Some(Light|Dark)` preferred theme suppresses it. So System uses `None`;
  sticky Light/Dark use `Some`. **Do not trust the event's `Theme` value** —
  winit resolves it via undocumented `ShouldAppsUseDarkMode`, which has been
  seen inverted vs `AppsUseLightTheme`. Treat the event as a wake-up and
  re-read `system_theme::system_is_dark()` (same registry path as cold start).
  Cold start cannot wait for this event anyway — brushes bake into layouts.

### Keyboard: three things that were each a real bug

A `KeyEvent` carries `logical_key` (what the user meant) and `text` (what they
typed). Both matter, and the platforms disagree about what goes where:

- **Ctrl+I arrives as `NamedKey::Tab` on Windows.** The Tab arm has to be guarded
  with `!ctrl` or it indents instead of italicising.
- **Ctrl+letter is delivered as an ASCII control code** on Windows — Ctrl+K is
  `0x0B`, not `Character("k")`. `edit::ctrl_letter` normalises both forms.
- **Alt+Arrow is swallowed** by the Windows system menu and some Linux window
  managers, so Ctrl+Shift+Arrow is accepted as an equivalent everywhere.

And the API decision that came out of it: **`KeyEvent` cannot be constructed
outside winit**, because of a `platform_specific` field. A key handler taking one
is therefore untestable — which is how two chords shipped broken. `Editor::key`
takes the decomposed pieces instead
(ADR-17).

Those two facts together have a consequence that cost a design: **Ctrl+Tab is
unreachable from a key map.** It is the same logical key as Ctrl+I, which the map
deliberately resolves to italic, and the only thing that can tell them apart is
`event.physical_key` — which lives on the `KeyEvent` no view ever sees. So the
shell reads it and hands the view a direction (`View::cycle_tab(delta)`), keeping
the untestable line where `KeyEvent` already is. Do not "fix" this by adding a
`NamedKey::Tab` arm to a view; that breaks Ctrl+I instead, and quietly.

### Focus transfer can replay a held shortcut

`WindowEvent::KeyboardInput.is_synthetic` distinguishes winit's focus-transfer
events from deliberate key input. On Windows and X11, losing focus synthesizes
releases for held keys; gaining focus synthesizes presses. A synthetic press
can have `repeat == false`, so checking only repeat does not protect a toggle.
See the [pinned winit event contract](https://github.com/rust-windowing/winit/blob/v0.30.13/src/event.rs#L192-L201).

The detached UI inspector exposed this: F12 opened it and moved focus while F12
was held, then its new window treated the replay as another toggle and closed.
Both source and inspector now use `inspector::shortcut_pressed`, accepting only
non-synthetic initial presses. The inspector's controls also ignore synthetic
keyboard events, so held Enter/Space/Escape cannot activate on focus transfer.
There is no time-based debounce; the next deliberate press remains immediate.

`koru-window`'s runners translate a `KeyboardInput` before an application sees
it, and the translated record has `repeat` but not `is_synthetic`. An
application on the multi-window runner that opens or closes a window on a key
therefore reads the flag from `MultiWindowApp::raw_event`, which is handed the
untranslated event first. Read from the source rather than measured: the key
quarantine would not cover this case, because it ends on any release of the
key, and the window losing focus is sent a synthetic *release* before the
synthetic press reaches the window gaining it.

### Queued keyboard modifiers are not the keys held now

The M3 reminder run delivered Ctrl+A as a literal `a`: the native batch was
ordered correctly, but the host replaced winit's queued Control bit with
`GetAsyncKeyState` after the entire down/up batch had already finished.
`GetAsyncKeyState` answers at call time; it cannot reinterpret an earlier key.
[Microsoft's API contract](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getasynckeystate).

Pinned winit 0.30.13 reads `GetKeyState` in `get_agnostic_mods`, emits changed
modifiers before keyboard dispatch, and retains buffered callback order. Use
those reports and the observed key-down/up stream for keyboard handlers.
Replacing the async read with another native read in the callback would still
lose event-time information when callbacks are buffered.
[Winit dispatch](https://github.com/rust-windowing/winit/blob/v0.30.13/src/platform_impl/windows/event_loop.rs),
[buffering](https://github.com/rust-windowing/winit/blob/v0.30.13/src/platform_impl/windows/event_loop/runner.rs).

`koru-workspace-host::app::modifiers` owns this distinction. An empty modifier
report still purges cached modifier keys when a hotkey owner consumed releases;
key-up still clears the last released side, and focus loss clears the whole
stream. Pointer recovery still uses current physical state to recover Ctrl-click
after focus loss and clear missed releases. No key-hold delay or input retry is
needed, and native delivery guards remain separate from event interpretation.

## DPI: two coordinate systems

winit reports sizes in **physical pixels** and a **scale factor**. Everything in
this codebase is in *logical* pixels, converted once at the boundary:

```rust
let s = pane.scale() as f32;
pane.cursor = (position.x as f32 / s, position.y as f32 / s);
```

and applied once as a root transform when drawing. No metric in `theme.rs` ever
knows the scale factor.

Two traps recorded from experience: **the reported viewport and the drawable
surface can disagree** under fractional DPI, so anchor chrome to content or a
known edge; and **screenshots are DPI-virtualised** unless the capturing process
calls `SetProcessDPIAware`, so pixel arithmetic off a capture will mislead you.

## Passive windows: inactive is a lifecycle contract

WindowAttributes.with_active(false) is only the creation request. On Windows a
status popup that must preserve another application's exact focused control
also needs WS_EX_NOACTIVATE applied and read back before the hidden window is
revealed. If that proof fails, do not show the popup. Taskbar policy remains a
separate decision, but a passive HUD that is already specified to stay out of
the taskbar must implement it as a native utility window too: winit's
`with_skip_taskbar(true)` calls `ITaskbarList::DeleteTab` after creating an
ownerless `WS_EX_APPWINDOW`. That removes the present tab but Windows can add
it again when the initially hidden window is revealed. Before reveal, set
`WS_EX_TOOLWINDOW`, clear `WS_EX_APPWINDOW`, and verify those bits beside
`WS_EX_NOACTIVATE`. Do not add the tool-window bits to a passive surface whose
product contract says it belongs in the taskbar.

Skipping the focus call in GPU open is not enough. Both WindowRequest::Focus
and the runners' later take_activation_request path can focus an existing
window, so WindowActivation::Passive consumes those requests in single- and
multi-window runners. Focused remains the default; adding a passive surface
therefore cannot silently change existing applications.

For mixed DPI placement, winit WindowAttributes position is physical while
inner size may be logical. Compute one physical position from the captured
target root's monitor work area and DPI, but pass the HUD's size as logical.
Do not query the cursor, primary monitor, or foreground window later from the
presentation thread: target movement produces a new content-free work-area/DPI
projection from the target-owning worker.

`WindowAttributes::with_transparent(true)` is not sufficient for a Vello HUD on
Windows. wgpu 28's DX12 surface created from an HWND advertises only opaque
composition, so a zero-alpha clear is presented as black. `koru-window` keeps
the semantic transparent-window request but creates an opaque surface on
Windows and clips the HWND itself with a rounded GDI region. Reapply that region
after a resize or DPI change; `SetWindowRgn` owns it after success, while the
caller must delete it after failure. Other platforms retain alpha composition.
The shaped HUD uses the shared `PAGE_RADIUS`, scaled to physical pixels, rather
than a height-sized capsule: the latter cuts away visibly large transparent
corners over dark content. It may request `CornerPreference::Round` for Windows
11's modern compositor shadow, but must not enable
`set_undecorated_shadow(true)`: that classic fallback deliberately creates a
one-pixel top non-client strip. Disable DWM's own border with
`set_border_color(None)` as well, because the scene owns the uniform hairline.
The native region covers the full window, and the scene's single outline sits
on that same edge rather than introducing a second inset shape.

## Control flow: not spinning

```rust
event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + CARET_BLINK / 3));
```

`Wait` sleeps until an event. `Poll` runs flat out — a game loop. `WaitUntil` is
the middle: sleep, but wake by then anyway. Here that gives the caret something
to blink on and the sync tick something to run on, without burning a core.

## Control flow: the one exception

The slow `WaitUntil` above is the rule, and a frame is drawn only when something
changed. An animation is the one kind of state that changes on a *clock* rather
than on input, so `View::animating` lets a view ask for a fast wake (~8 ms) —
for the panes that are animating, and only until they stop. It must return to
`false`, or that becomes a game loop after all.

## `exit()` from `about_to_wait` does not end the loop

**Measured, on winit 0.30.13 / Windows.** `about_to_wait` is not "the queue is
empty and I will look at you again in a moment": it is winit's *last call before
it blocks*, and on Windows the block happens before the exit flag is read.
`run_on_demand` is

```text
loop {
    wait_for_messages();          // prepare_wait() -> about_to_wait, then BLOCK, then wakeup()
    if exit_code() { break }      // only now is exit() noticed
    dispatch_peeked_messages();
    if exit_code() { break }
}
```

so calling `event_loop.exit()` inside `about_to_wait` sets a flag that nothing
reads until `MsgWaitForMultipleObjectsEx` returns — and under the default
`ControlFlow::Wait` that call is passed `INFINITE`. The loop then sits there
until *some unrelated message* happens to arrive for the thread.

That is a wait with no upper bound, and its length is a fact about the desktop
rather than about the program. The same unchanged binary — the accessibility
proof in `apps/koru/src/mainview/accessibility/document/native.rs` — was
measured returning **1.15 s, 1.64 s, 2.52 s and 29.2 s** after its own work had
finished, and the maintainers had seen **361.0 s twice**. Logging the messages through
`EventLoopBuilderExtWindows::with_msg_hook` named the thing that finally ended
one of those waits: `WM_MOUSEMOVE`. Nothing else was pending; the pointer moved
over the window and the process was free to leave. The 361 s readings themselves
were never reproduced here — that would take a desktop quiet enough for six
minutes — and no system idle timer on this machine is set to 360 s (display-off
is 900 s on AC, the screensaver is off), so they are recorded as "a longer quiet
period", not as a named timeout.

The tell is that the work is demonstrably over — every assertion has passed,
every counter is right — and the process still will not return. Do not read it
as a hang in whatever ran last.

**The fix is one line, and it is the control flow, not the exit.** `Poll` maps
to a zero timeout, which winit turns into an early `return` before it ever waits:

```rust
if self.finished {
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop.exit();
}
```

With that, the four interactive accessibility proofs return in ~2 ms after
`exit()` instead of somewhere between a second and six minutes.

**`about_to_wait` is the only callback with this problem.** `new_events` is
emitted by `wakeup()` *after* the block returns, and `window_event`/`user_event`
run inside `dispatch_peeked_messages` — the exit flag is read on the line after
each of those. `koru-window`'s `settle` is reached from `new_events`, `resumed`,
`user_event` and `window_event`, so the shipping quit path is safe. The exposed
ones are `crates/koru-workspace-host/src/app.rs`'s fatal-startup exits inside
`about_to_wait`, which are only tolerable because a window being torn down
generates messages of its own.

## A window with no titlebar

`with_decorations(false)`, so the tabs can share the caption row with the window
buttons. On Windows this is cheaper than it sounds: winit keeps `WS_CAPTION` and
`WS_SIZEBOX` on the window and removes the frame by answering `WM_NCCALCSIZE`
instead, so **aero snap, Win+Arrow, snap layouts and the taskbar all keep
working**.

### The resize border does *not* survive, and the style bit lies about it

`WS_SIZEBOX` is still set. That is not the same as being resizable: the same
`WM_NCCALCSIZE` answer that removes the frame gives the client area the whole
window, leaving no non-client region for `DefWindowProc` to find an edge in.
Probed with `WM_NCHITTEST`, every pixel of an undecorated window comes back
`HTCLIENT`:

```
right edge  -> HTCLIENT      bottom edge   -> HTCLIENT
left edge   -> HTCLIENT      corner BR     -> HTCLIENT
```

So the border is the app's. `crates/koru-window/src/resize.rs` hit-tests a 6px
band (16px at the corners, because two 6px bands crossing is a pixel-hunt) and
the shell claims a press there before the view sees it. `ResizeDirection` has
`impl From<..> for CursorIcon`, so the pointer shape and the direction cannot
disagree. A maximised window reports no border at all — it cannot be resized,
and that is also what leaves the top-right corner free for the close button when
the window fills the screen.

What is left to do, all of it what the titlebar used to:

| | |
| --- | --- |
| `Window::drag_window()` | move the window |
| `Window::drag_resize_window(dir)` | the resize border |
| `set_minimized`, `set_maximized` / `is_maximized` | the other two buttons, and the double-click on the caption |
| `show_window_menu(pos)` | the right-click system menu. **Windows only** — gate it |

Both drag calls **must happen inside the press that asked for them** — winit only
honours them while the button that asked is still down. Deferred to the tail of
the handler, they silently do nothing.

### Opening a URI is also a Windows shell boundary

`explorer.exe <uri>` is not the registered-handler API. It can open a File
Explorer window instead of the default browser, even when the argument is an
`https:` URL. `koru_window::open_uri` uses `ShellExecuteW` with the
`open` verb so Windows dispatches each scheme to its registered handler.
Keep this shared boundary for OAuth, help, update, import and `mailto:` links;
do not grow another process-based opener at a call site.

### A light native caption is not the page background

Setting a decorated window to the light winit theme still leaves Windows free
to paint its standard gray caption. The onboarding page needs the caption and
page to be one continuous surface, so koru_window::frame::apply_caption writes
the page background and text colors through DWM. Unsupported Windows versions
ignore the attributes; other platforms keep their native title-bar behavior.

### The border and the rounded corners are the compositor's

Rounded corners cannot be *drawn*. A window is a rectangle, and painting a
rounded shape inside one leaves the four real corners showing whatever was
painted there — a rounded outline on a square window, which reads worse than no
rounding. Only the compositor can clip the window's actual shape.

So both come from DWM, through `WindowExtWindows` (`crates/koru-window/src/frame.rs`):

| | |
| --- | --- |
| `set_corner_preference(CornerPreference::Round)` | `DWMWA_WINDOW_CORNER_PREFERENCE`. `Round` is the *system* radius — 8px on Windows 11 — rather than a number of ours, which is the point: a window rounded by a different amount from every other window looks broken rather than styled |
| `set_border_color(Some(Color::from_rgb(..)))` | `DWMWA_BORDER_COLOR`, a 1px border that follows the same clip. Takes a colour, so it is `panel_border` from the palette and flips with the theme |
| `set_undecorated_shadow(true)` | Classic aero drop shadow for undecorated windows. winit leaves a 1px NC tweak in `WM_NCCALCSIZE` so DWM has something to hang the shadow on. **Needed for snap**: the modern Round elevation shadow does not survive aero-snap on a full-client undecorated window. Side-effect: a thin 1px line on the top |

Maximised, the corners are set to `DoNotRound` and the undecorated shadow is
off: the window's edges are against the screen's, and rounding them would cut
the corners out of the content. A snapped window stays `Round` with the classic
shadow on. **There is no "maximised changed" event** — it arrives as an ordinary
`Resized` — so the shell remembers the last state and acts only on a real change.

Both attributes are Windows 11 and later. On Windows 10 the calls fail
harmlessly inside winit and the window is square with no border; elsewhere the
whole module is a no-op, because the compositor there is not ours to ask.

macOS is the one platform this shape does not suit: `drag_resize_window` is
hard-unsupported there, and the right design is `with_titlebar_transparent` /
`with_title_hidden` keeping the real traffic lights.

Two things to know before turning it on:

- **winit style changes replace the complete Windows extended-style word.**
  In winit 0.30.13, `WindowFlags::apply_diff` writes all of `GWL_EXSTYLE`;
  frame changes, visibility/reveal, minimise/maximise and window-level changes
  can therefore remove native `WS_EX_NOACTIVATE` / `WS_EX_TOOLWINDOW` flags or
  restore `WS_EX_APPWINDOW` after they were installed. This passive utility
  contract installs and verifies all three facts before the first reveal,
  reapplies and reads them back after winit's reveal and frame mutations, and
  fails closed by hiding the window if verification fails.
  Later runner-owned chrome/style requests are consumed for passive status
  surfaces, and theme changes do not reapply their native frame.
- **A saved window position shifts once.** Geometry is stored from
  `outer_position()`, and undecorated windows collapse outer to inner — so the
  first launch after the change moves the window up by the old caption height.
  Once. Accepted rather than migrated.
- **The cursor is the shell's again over the caption.** `Pane::sync_cursor_icon`
  asks the view, so the caption has to win before the view is asked — the way
  the scrollbar gutter already claims its column.

## macOS: the `NSApplication` delegate is winit's, and taking it aborts the app

winit's docs describe it as a window-and-events library that stays out of the
application's way, and the macOS backend does not read that way at all:
`EventLoop::build` calls `sharedApplication`, installs a
`WinitApplicationDelegate`, and overrides `sendEvent:` to route through it.
Every callback afterwards starts with `ApplicationDelegate::get`, which is

```rust
if delegate.is_kind_of::<Self>() { … } else {
    panic!("tried to get a delegate that was not the one Winit has registered")
}
```

**So `NSApp.setDelegate:` is not something a host may call.** The panic is raised
inside an Objective-C frame that cannot unwind, which is an `abort`, not a
backtrace you can recover from — Koru died on the first event of every launch,
before a window was drawn, for exactly this reason. A subclass does not help
either: winit built its instance already and reads its own ivars off whichever
object is installed, so the *object* has to stay winit's too.

What winit's delegate implements is `applicationDidFinishLaunching:` and
`applicationWillTerminate:`, and nothing else. Anything else AppKit only offers
through the delegate — `application:openURLs:` for `koru://`,
`applicationDockMenu:` for the Dock's right-click menu — is added to *winit's
delegate class* with `class_addMethod`, before `run_app`, because finishing
launching is when AppKit asks the delegate what it answers.
`crates/koru-workspace-host/src/macos_urls.rs` is the worked example.

## What it gives you beyond the basics

Used here: `set_window_level(AlwaysOnTop)` for pinning, `with_decorations(false)`
plus `drag_window` for the custom caption row, `set_maximized` / `is_maximized`
for remembering that the window was maximised, `WindowEvent::DroppedFile` for
images, and `outer_position` / `inner_size` for remembering geometry.

`set_theme` (mapped onto the Windows immersive dark-mode attribute) is still
called, but it now only reaches the parts of the frame we do not draw ourselves.

## Where to look

Koru patches winit 0.30.13's desktop IME lifetime boundaries without changing its
public API. Permission changes retire queued native input before the host reuses
the window for another editor or selection. An idle stream need not emit
`Disabled`; hosts reset with disable/enable and await a fresh native `Enabled`
without blocking ordinary key text. Windows emits `Enabled` on composition
start, not as a setter acknowledgement. The backend rules, cancellation limits,
test commands and native acceptance cases are in
[the patch notes](../../../third-party/patches/winit.md).

- `crates/koru-workspace-host/src/app.rs` — the entire winit surface of this project
- `crates/koru-editor/src/input/keymap.rs` — `ctrl_letter`, and the key map those traps shaped
- [winit docs](https://docs.rs/winit) — upstream
