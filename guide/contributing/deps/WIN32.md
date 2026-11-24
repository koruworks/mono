# Win32 global input

The Agent's global-input boundary uses several APIs whose zero and success
values are unsafe to interpret without the surrounding state.

## A hotkey press is not a key stream

RegisterHotKey posts WM_HOTKEY to the registering thread's queue. It is the
press mechanism and uses MOD_NOREPEAT. Koru does not install WH_KEYBOARD_LL,
Raw Input, or another monitor. Release is observed separately with
GetAsyncKeyState, on a dedicated thread, for only the virtual key belonging to
the active registered dictation chord. The observer blocks on a channel while
idle and stops sampling after release, cancellation, timeout, shutdown, or
resume.

The high bit of GetAsyncKeyState is the current down bit. A zero is not always
release: Windows also returns zero when the caller cannot observe the active
input desktop. Before reading the bit, WindowsKeyState opens the input desktop
and compares its UOI_NAME with the Agent thread's ordinary desktop. Failure or
a different desktop means unavailable, which cancels the hold. It must never be
turned into release when UAC, the lock screen, or another secure desktop ends.

## A window handle is not an editing target

Two web fields and many native controls can share one HWND. Capture therefore
keeps an opaque IUIAutomationElement and commit asks
IUIAutomation::CompareElements against the focused element again. The HWND
only corroborates that the focus window did not change; it is not the target
identity. A stale element is destroyed; a different element is changed.
CurrentIsPassword=true is Protected. A provider failure is Unavailable and
fails closed. No title, process name, path, or time is an identity.

SendInput with KEYEVENTF_UNICODE preserves the UTF-16 stream,
including surrogate pairs and bidirectional controls. A zero accepted count may
not be retried through another mechanism. A partial count also never retries
because doing so could duplicate text already accepted. Both outcomes are typed
separately from a refusal where no input event was accepted.

## Clipboard fallback is deferred

ADR-238 still applies: only koru-clipboard may call clipboard APIs. Global input
does not call it. SendMessageTimeout returning zero does
not cancel a message whose window procedure already began. Immediate restore
could make that late paste insert the prior clipboard; leaving the transcript
does not meet privacy or restoration guarantees and can also publish dictated
text to clipboard history or cloud synchronization. The Windows adapter
therefore refuses before any clipboard read or mutation. Q1 of an internal
question log (not published) records
the SendMessageCallback/message-pump option and the permanently hung receiver
contradiction. Do not remove this preflight refusal merely because the exact
clipboard formats look restorable.

Real UAC/lock transitions, UI Automation providers, and cross-application
Unicode behavior remain exclusive-machine acceptance. Clipboard and paste are
deferred under Q1, not acceptance cases for the current mechanism. Automated
tests use fakes and never register a hotkey, inspect the desktop, or open the OS
clipboard.

## The client half: finding a control, and typing into one you own

Everything above is the Agent's *insertion* mechanism. Plan 3.1 added a second
consumer of the same platform — `apps/koru-demo`, which inspects a Koru window
from outside the way Narrator does — and four things it established are not in
`ACCESSKIT.md`, because that page is about the provider.

### `VARIANT` is gated behind two features, and the symptom names neither

A property condition takes a `VARIANT`, so `IUIAutomation::CreatePropertyCondition`
is compiled out unless **both** `Win32_System_Variant` **and** `Win32_System_Ole`
are on. With only the first, `windows` 0.62.2 reports:

```text
error[E0599]: no method named `CreatePropertyCondition` found for struct `IUIAutomation`
help: there is a method `CreateNotCondition` with a similar name
```

which reads like a binding-version problem and is not one. The three
accessibility proofs in the tree do not hit it because they are compiled with
the workspace's feature unification behind them.

`SetFocus` is also not where it looks: it lives in
`Win32::UI::Input::KeyboardAndMouse`, beside the key-state calls, rather than in
`Win32::UI::WindowsAndMessaging`.

### There is no prefix match on a property, so a wildcard locator is a scan

`koru:w*/v1/s:body#root` cannot become a condition: UIA property conditions are
equality only. A fully specified locator is one `CreatePropertyCondition` plus
one `FindAll`; anything with a wildcard in it is `CreateTrueCondition` +
`FindAll(TreeScope_Descendants)` + a filter on the client, which is `O(the
subtree)` — real over a thousand-paragraph document.

`koru-demo` therefore reads the window number out of the root element's own
`AutomationId` and binds `w*` to it *before* searching, which puts the ordinary
case back on the one-condition path. `FindAll` is used even where `FindFirst`
would do, because *how many matched* is the answer: a locator that names two
controls is a defect, and `FindFirst` cannot report one.

### `TreeScope_Descendants` cannot find the element you started from

An element is not a descendant of itself, so a `FindAll(TreeScope_Descendants,
…)` rooted at a window can never match that window's own
`koru:w<n>/v0/s:window#window` locator. **Measured 2026-09-11** against a live
Koru window: `Descendants` reported *no control has that locator* for the one
control the caller was most certain existed, and `TreeScope_Subtree` — root plus
descendants — found it immediately, `Window at 586,269 1000x667`.

The four interactive proofs in this tree all use `Descendants` and none of them
is wrong, because each searches for something strictly *inside* the window it
was handed. A runner is asked a different question — *which control does this
locator name* — and the window itself is a valid answer, so `koru-demo` uses
`Subtree`.

### Windows' foreground lock is what makes a self-owned input proof safe

`SendInput` goes to whatever has keyboard focus, so a test for the insertion
mechanism has to own the window it types into. Owning it is not enough on its
own — the window also has to *have the foreground*, and asking for it is where
the safety actually comes from.

**Measured 2026-09-11**, on Windows 11 26200, running
`cargo test -p koru-agent --lib global_input::windows::native -- --ignored` from
a console that was **not** in front: `SetForegroundWindow` was retried for five
seconds against a window the test process had just created, and
`GetForegroundWindow` never returned it. The proof stopped at its own guard and
sent nothing.

That is the documented rule working as designed — a process that does not own
the foreground, and was not started by the process that does, cannot take it —
and it means an interactive input proof left in the tree **cannot** type into
somebody's editor by being run at the wrong moment. It fails instead. The
corollary is the instruction in the proof's own failure message: run it from a
console that is already in front, on a machine you have to yourself.

Two smaller notes from the same run:

- `SetForegroundWindow`'s `BOOL` says whether the attempt was honoured, not
  whether focus is yours now. Read it back from `GetForegroundWindow` — the same
  distinction `InsertBackend::restore_target` already makes.
- `KEYEVENTF_UNICODE` events arrive at a window as `WM_KEYDOWN` with
  `VK_PACKET`. A message pump that does not call `TranslateMessage` never
  produces the `WM_CHAR` an edit control acts on, and the control stays empty —
  which looks exactly like a delivery failure and is not one.

### Plan 3.1 extraction: same instrument, new home

The native target mechanism now lives in `crates/koru-windows-input/src/windows.rs`
after the accepted extraction. Agent's `global_input/windows.rs` retains the hotkey
and key-state adapters and maps native results to the existing policy types.
`koru-demo` uses the shared thread-bound UIA client; its traversal and waits
remain in the runner. `CUIAutomation` (Agent) and `CUIAutomation8` (runner) remain
distinct choices. Every captured target retains its apartment until its element
is released, on the owning thread.

The ignored proof moved to `crates/koru-windows-input/src/windows/native.rs`;
the historical command above records the before-run location. Its current
invocation is:

```sh
cargo test -p koru-windows-input --lib windows::native -- --ignored --nocapture
```

Its observations and assertions remain directly comparable. Normal tests never
run it. The maintainers reported the third-party after-run and supplied the matching
native proof on 2026-09-11; the filled table is historical,
native-input-before-after.md.
Pure classifier and policy tests alone do not establish native behavior.

**Do not prove dictation or global input on `koru.bat root`.** `--root` turns
off machine-wide integration: startup and URL registration are unavailable,
start-at-login is omitted, and the Voice surface hides its startup control.
That is correct, not a defect. A `debug` build is not what ships. Speech
models are cached per root, so a fresh root can sit at *starting* while the
worker fetches them. The shape that ships is an installed package (see
koru-agent ACCEPTANCE A.1).

**Stop every other Koru first.** `koru.bat stop-all`, then check no
`koru-agent.exe` remains from another worktree or an install. Windows
arbitrates the dictation chord between agents; a second owner looks exactly
like dictation being broken.

**The person who ran a check writes its cell.** An agent summarising a
session cannot be told from invention; this is why the before/after table is
not regenerated.

### M3 cancellation must revoke consequences, not merely abandon answers

The inspection supervisor originally abandoned a blocked COM thread and discarded
its late reply. Input changes the consequence: that thread could wake after a timeout
and still type. Each call now carries a `DeliveryPermit`. Timeout and cancellation
revoke it independently of UIA; all provider reads happen before its short locked
consequence. A resumed worker therefore cannot submit a new input batch. Never move
a UIA call under that lock, because a stuck provider would then block cancellation.

Focus in the runner is deliberately an onscreen native click into an Edit control.
It does not call provider `SetFocus`: an already-dispatched asynchronous provider
focus request cannot be recalled by revoking a local flag. Offscreen text remains
readable; document navigation uses PageDown from a currently proved paragraph,
waits for observed geometry progress, then clicks the revealed paragraph. Generic
buttons are clicked explicitly; a focus request never silently activates them.

Mouse delivery is one absolute virtual-desktop move/down/up `SendInput` batch.
The worker enters per-monitor-v2 DPI awareness so UIA physical screen bounds and
virtual-screen metrics describe the same coordinates, including negative origins.
Bounds and UIA hit identity are reread before delivery; root/PID, foreground,
ordinary desktop and retained process liveness are checked at the native consequence.
A changed desktop or minimized window refuses input, as do user-held modifiers, mouse buttons and virtual keys the next chord would press.

A partial batch releases only the key/button downs in its accepted prefix, in
reverse order, without a provider call between downs and cleanup. Cleanup rejection
is a failure, never a success or a reason to retry text. Text batches are capped at
4096 UTF-16 units; chords at eight distinct valid virtual keys. This does not promise
that an OS refusing all input can accept release events. Report that failure and stop.

Expected text is compared inside the COM worker and is not returned in normal
reports. `accesskit_windows` currently ignores `GetText(maxLength)` (see
[ACCESSKIT.md](ACCESSKIT.md)); the supplied maximum is not a hard provider allocation
bound. The worker watchdog still bounds caller regain of control.

### M3 scoped content reads

`koru-demo` keeps content snapshots separate from ordinary element facts. A
snapshot requires the exact handle returned by BindWindow, a live retained owner
process, a model `#text` target, and RawViewWalker ancestry to that window before
and after the content call. Password targets are refused. Rooted fixture discovery
uses ControlViewWalker in tree order, caps matched nodes at 256 and traversal at
4096, and fails rather than treating a cap as a complete manifest.

The retained snapshot limit is 1–16384 UTF-16 units. This is an output limit,
**not a provider-allocation bound**: the pinned AccessKit Windows adapter ignores
`GetText(maxLength)` (see the AccessKit primer). Reads stay on the supervised
worker; timeout recovers the caller even if the provider keeps working.

Explore omits SelectionItem on unselected document rows to preserve Invoke.
Fixture capture reads `IsSelectionItemPatternAvailable` first: absence means
selected=false; a failed property read remains failure. It must not mistake an
unselected neighboring document for an unavailable provider.

The pinned windows-core 0.62.2 nullable-interface conversion returns
`Error::empty()` with HRESULT 0 for a null child/sibling, not E_POINTER.
`uia::snapshot::tests` exercises `Type::from_abi(null_mut())` directly, without COM
or a window. Fixture traversal treats only that proven sentinel as branch end;
real E_POINTER, E_FAIL and destroyed-element errors remain failures. Treating
E_POINTER as the null sentinel aborted discovery at the first leaf.

## Custom message buttons and Common Controls v6

`rfd` 0.15 defaults to `MessageBoxW` on Windows. That backend accepts
`YesNoCancelCustom` but displays system Yes/No/Cancel labels and returns
`Yes`/`No`, not `Custom(label)`. Enabling `common-controls-v6` selects a static
`TaskDialogIndirect` import throughout the unified dependency graph. Application
executables have the v6 manifest from `koru-winres`; some library/integration
test executables do not, and fail before main with `STATUS_ENTRYPOINT_NOT_FOUND`.

The document-removal choice follows `koru-updater`'s established dynamic
`TaskDialogIndirect` lookup instead. It uses translated Keep/Delete/Cancel
buttons, Keep as the safe default, and RTL layout for Arabic. A missing API or
failed dialog retains the source and reports pending deletion. Other platforms
use rfd, accepting typed Yes/No as well as custom-label results; cancellation
and unknown results never release the source. The app's selected language owns
these labels, not the machine locale.

The pinned [rfd Windows implementation](https://github.com/PolyMeilex/rfd/blob/0.15.4/src/backend/win_cid/message_dialog.rs)
contains both upstream paths. Pure decision tests do not establish native
labels or accessibility; those remain Windows acceptance checks.
