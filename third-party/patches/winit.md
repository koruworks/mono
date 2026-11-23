# winit 0.30.13: native IME lifetime boundaries

`winit.patch` changes the published 0.30.13 crate, under its Apache-2.0 license.
The public event/window API and dependency version are unchanged. The archive's
SHA-256 is `a6755fa58a9f8350bd1e472d4c3fcc25f824ec358933bba33306d0b63df5978d`;
the archive and patch pins are in [fetch.manifest](../fetch.manifest).
Source is fetched into the ignored `third-party/crates/winit`, never vendored.

## Why the host needs this

Public `Ime` events do not identify the editor or selection that owned their
native composition. Disabling and immediately enabling input for a replacement
target could deliver a previously queued `Enabled` and `Commit` to that target.
Waiting for `Disabled` is not a solution: an idle native stream need not produce
one. An event-loop turn is not a cancellation acknowledgement either.

The patch gives each native context and requested permission lifetime a private
generation. It retires the generation before cancellation, stamps native
callbacks/batches, and checks the captured generation immediately before each
application delivery. A clear-preedit callback can therefore retire the commit
that follows it. It never retags old queued work with the current generation.
It uses existing queues; there is no timer, polling loop, UI wait, or text logging.

## Backend behavior

- **Windows:** requested permission retires synchronously, while applied native
  state is tracked separately. Every current permission transition cancels before
  associating the context, including an enable that overtakes a posted disable.
  Obsolete posted work cannot later detach a fresh context. An active composition
  is canceled even if its preedit is empty. Cancellation uses `CPS_CANCEL`, then
  removes only this HWND's pending `WM_IME_STARTCOMPOSITION` through
  `WM_IME_COMPOSITION` messages. Idle/no-context reset needs no callback. Failed
  cancellation keeps delivery closed and detaches native input so ordinary keys
  remain usable; a later transition reassociates behind the gate and retries.
  IMM calls hold no `WindowState` mutex. Reentrant completion cannot change a
  newer applied epoch. The runner preserves stamps in its reentrant event buffer.
- **macOS:** the view retires before `discardMarkedText`; disabled or retired
  callbacks cannot change composition state. Deferred run-loop events keep their
  original stamps. All callbacks within `interpretKeyEvents` share one stamp;
  input-source `Disabled` delivery also checks for a reentrant target change
  before interpreting the rest of that native key event. In pinned winit,
  `maybe_queue_on_main` calls `maybe_wait_on_main`/`get_on_main` synchronously,
  so the permission setter has applied retirement when it returns, including
  when called from another thread. The patch adds no main-thread wait.
- **X11:** each XIC has an immutable context guard paired with its window's
  requested lifetime. It retires before destruction/recreation and XIM teardown.
  Stale requests are ignored; the latest enable recreates the XIC, performing
  cancellation even if an intermediate disable was superseded. Permission
  requests are applied before the next `XFilterEvent`. Direct lookup commits use
  the same guarded channel as context `Enabled`, preserving their order. The
  dispatcher checks again between clear-preedit and commit. Composition ownership
  is per window/context rather than one process-wide boolean.
- **Wayland:** each text-input object has a separate lifetime and commit counter,
  paired with its focused window lifetime. Every text-input `commit` counts,
  including enable/disable, cursor-area, purpose, Enter and Leave. A Done serial
  older than the latest commit remains valid within the current enable; one from
  before that enable is discarded. Arithmetic handles wrap, and rejects an
  ambiguous interval of at least half the serial space. Reset/Leave clears the
  pending batch; every rejected Done consumes its batch too. Both event sinks
  carry guards through delivery. Seat removal retires its context and removes
  its window references. Lock order is always window then text-input data;
  Done needs only the latter.

The native reset operations rely on the platform's cancellation/context/serial
contract. The patch cannot identify arbitrary untagged input newly injected
after successful native cancellation by a nonconforming IME. It does protect
callbacks already observed by winit, queued batches, reentrant delivery, and
native work separated by the native context or Wayland enable serial. This is
not a replacement for platform acceptance testing. Upstream ordinary key-text
behavior, including macOS's distinction between marked text and ordinary
`insertText` during a key event, is retained.

## Host contract

On a real target change, or local interruption of an owned composition, cancel
the old editor's preedit and call `set_ime_allowed(false)`, then `true` if the
replacement target accepts text. Do not wait for `Disabled`. A stable editor
owner across layout/swap changes should keep its composition and update geometry.

Fresh `Enabled` establishes readiness. On Windows it comes from an actual
`WM_IME_STARTCOMPOSITION`, not from the setter: initial enable and idle reset
can produce neither `Enabled` nor `Disabled`. Ordinary key text continues while
there is no native composition. A normal Windows composition end produces
`Disabled`; retain the desired allowed target so the next native start can
produce `Enabled` without another permission toggle.

## Validation and reproduction

Production library checks use the workspace's actual dependency graph:

```powershell
$env:CARGO_BUILD_JOBS = '2'
cargo check -p winit --lib --offline
cargo check -p winit --lib --offline --target x86_64-unknown-linux-gnu
cargo check -p winit --lib --offline --target aarch64-apple-darwin
```

`cargo test -p winit` refuses to test a nonmember dependency. The first-party
[test harness](../../scripts/checks/winit-native-input.ps1) creates only an ignored
manifest/lock under `target/`, points directly at the fetched `src/lib.rs` and
build script, and selects upstream runtime dependencies without example/dev
dependencies. Windows tests select `rwh_06`; cross checks retain the default
X11/Wayland/ab_glyph features and omit unused crossfont. It uses the production
lock as input, never changes it, runs offline, caps jobs at two, and restores the
caller's jobs setting. It does not install targets or change the toolchain.

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/checks/winit-native-input.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/checks/winit-native-input.ps1 -CheckTarget x86_64-unknown-linux-gnu
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/checks/winit-native-input.ps1 -CheckTarget aarch64-apple-darwin
```

The Windows run executes 16 pure tests: five existing event/error tests and
eleven regressions in these modules:

| Module | Regression tests |
| --- | --- |
| `platform_impl::ime_lifetime::tests` | `retiring_rejects_queued_enabled_and_commit_without_waiting_for_disabled`, `reset_from_clear_preedit_callback_retires_the_rest_of_that_batch`, `windows_and_seats_have_independent_lifetimes`, `wayland_accepts_old_but_current_serial_and_rejects_previous_enable`, `wayland_serial_wrap_keeps_context_boundaries` |
| `platform_impl::windows::event_loop::runner::tests` | `buffered_ime_cannot_claim_a_replacement_target`, `reentrant_clear_preedit_reset_discards_buffered_commit` |
| `platform_impl::windows::ime::tests` | `inline_enable_overtaking_posted_disable_resets_before_reuse`, `idle_reset_does_not_depend_on_a_disabled_callback`, `reset_failure_detaches_and_retries_before_enabling`, `native_reentry_cannot_apply_old_completion_to_a_new_epoch` |

Linux test compilation also checks the two X11 dispatcher tests, two Wayland
sink tests and three Wayland pending-batch tests beside those modules. Cross
compilation does not run them and establishes no native runtime result.

Validation performed for this patch:

| Check | Result |
| --- | --- |
| Production library, Windows / Linux X11+Wayland / macOS | All three compile checks passed without warnings |
| Windows fetched-source unit tests | 16 passed |
| Linux / macOS fetched-source test code | Both cross compile checks passed; tests not executed there |
| Verified published archive plus patch | Clean application reproduced all 224 fetched files byte-for-byte |
| Native desktop IME sessions | Not performed; acceptance remains open |

Still required on actual desktop backends: Japanese/Korean/Chinese composition,
direct commit, cancellation with empty/nonempty preedit, ordinary typing after
an idle reset, switching targets and same-control selections during composition,
candidate navigation, focus loss/regain, independent windows/seats, and retained
editor swaps/narrow layouts. Exercise Windows IMM, macOS AppKit, Linux X11 with
IBus/Fcitx and Wayland text-input-v3. Synthetic tests do not establish native
provenance or candidate-window correctness on those systems.

Regenerate with `scripts/setup/fetch-third-party.ps1 -UpdatePatch winit` and pin
the resulting hash. A clean application against the verified published archive
must reproduce every fetched file byte-for-byte. The existing release notice
generator reads the unchanged crate `LICENSE`; no fallback notice or new
dependency/version inventory is needed. The fetched-patch inventories include
winit explicitly.
