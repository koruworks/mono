# AccessKit

The native accessibility bridge. `accesskit` 0.24, `accesskit_winit` 0.32
(`accesskit_windows` 0.32.1 underneath), added by
ADR-336.

New to Rust syntax? [RUST.md](../RUST.md) first.

---

## What it is

Koru draws every control itself, so an assistive technology sees whatever we
tell it and nothing else. AccessKit is the thing we tell: a **provider** side
where an application publishes a tree of nodes, and a per-platform adapter that
turns that tree into the platform's own accessibility API — UI Automation on
Windows, NSAccessibility on macOS, AT-SPI on Unix.

Three crates, and the difference matters:

| | |
| --- | --- |
| `accesskit` | the schema only — `Role`, `Action`, `Node`, `TreeUpdate`. Pure data, `uuid` its one dependency, builds on every target. Both Koru crates name this |
| `accesskit_winit` | the adapter, bound to a winit `Window`. `cfg(windows)` in `koru-window`; Apple support needs separate verification |
| `accesskit_windows` | what `accesskit_winit` uses on Windows: a `WM_GETOBJECT` subclass on the HWND. We never name it, but every mapping below is its behaviour |

Where our code lives: `crates/koru-window/src/accessibility.rs` (when a window
has a provider) and `crates/koru-workspace-host/src/accessibility/` (what a Koru
window offers, and what a request means). What a *particular* surface offers
lives beside the thing that draws it —
`apps/koru/src/panes/explore/accessibility.rs`,
`apps/koru/src/tasks_view/accessibility.rs`,
`crates/koru-editor/src/accessibility/`,
`crates/koru-workspace-host/src/onboarding/accessibility.rs`.

## The version pin is not free-floating: it tracks winit

`accesskit_winit` releases follow winit releases. 0.32.2 declares
`winit = "0.30.5"`, which our `winit = "0.30.13"` satisfies — verified by
`grep -c 'name = "winit"' Cargo.lock` returning **1**. A mismatch does not show
up as a version error; it shows up as *two* winit crates in the tree and a type
error where the adapter is handed a `Window`. Check the count, not the manifest.

`default-features = false, features = ["rwh_06"]` on purpose. The defaults pull
the Linux stack *and* switch on `winit/x11` and `winit/wayland`, and Cargo's
feature unification would hand those winit features to every crate in this
workspace to gain nothing on the two platforms we ship. One of `rwh_05` /
`rwh_06` must be enabled or the crate refuses to compile with a `compile_error!`;
`rwh_06` is the `raw-window-handle` major winit 0.30 speaks.

## The three ordering rules, and which of them actually bites on Windows

**The adapter must exist before the window is first shown.**
`Adapter::with_direct_handlers` **panics** — not returns an error — when
`window.is_visible() == Some(true)`. Koru creates every window hidden and
reveals it after the first frame, so the install site is the moment after
`create_window`. `koru_window::accessibility::may_install` is that check kept
as a separate function so it can be asserted without a window.

**Window events go to the adapter before the application handles them.**
Documented on `Adapter::process_event`. On Windows that method is *empty* — the
provider is a subclass on the HWND and never needs winit's view of an event —
but macOS and the Unix adapter do use it, so the call belongs in the code
whether or not it does anything today.

**`DeactivationHandler` never fires on Windows.** `accesskit_winit`'s Windows
`Adapter::new` takes the deactivation handler and drops it on the floor
(`_deactivation_handler`). Our handling of it is correct and dormant; do not
build a Windows behaviour that depends on being told a client went away.

## Activation is asynchronous, and the placeholder tree is by design

A screen reader attaching sends `WM_GETOBJECT`, which arrives on the window
procedure at any moment — including while the loop is idle. The activation
handler runs there, on whichever thread the platform chose.

Koru's handler returns `None` and leaves a note. `accesskit_windows` then serves
a **placeholder tree** — a single `Role::Window` node with `NodeId(0)` — and
swaps it for the first real update. That is its documented path for a handler
that returns `None`, not a workaround, and it is what lets the tree be built on
the loop thread from state the loop owns.

Two consequences:

- **Never publish a node with `NodeId(0)`.** It is the placeholder root.
  `Occurrence::node_id` steps over zero for this reason.
- **A client that reads a property immediately after `ElementFromHandle` reads
  the placeholder.** The interactive probe polls until the `AutomationId`
  starts with `koru:`; a test that asked once would be flaky and would look
  like a mapping bug.

`update_if_active` does not call its closure until a client has attached. That
is the whole of "an ordinary session pays nothing": there is no separate
"is anyone listening" query to get wrong.

## A modal must withhold body deltas as well as the initial body

AccessKit removes subtrees when a parent's child list drops them. Publishing a
removed node again without a parent panics in `accesskit_consumer::State::update`
with "nodes which are neither in the current tree nor a child of another node
from the update". Retaining disconnected nodes in a test client hides this.

The desktop shell publishes only the blocker when Move to or another modal
opens. Subsequent updates with unchanged chrome must also withhold the document
body. Otherwise the body projector sees a withheld body, produces a complete
one, and sends its root as a loose delta under a parent that no longer exists.
Closing the blocker changes the chrome revision and restores the body beneath
its content region. Integration fixtures prune disconnected nodes and reject
new unattached nodes to cover this transition.

## Which UIA property a Koru locator lands on

This is the mapping the roadmap required to be established rather than assumed.
Read out of `accesskit_windows-0.32.1/src/node.rs`'s `properties!` table and
then **confirmed from outside the process** by
`crates/koru-workspace-host/tests/accessibility_native.rs`:

| AccessKit | UIA property | Koru puts there |
| --- | --- | --- |
| `Node::author_id` | `UIA_AutomationIdPropertyId` | the occurrence locator, `koru:w1/v0/s:window#close` |
| `Node::class_name` | `UIA_ClassNamePropertyId` | `KoruWindow` on the root |
| `Node::label` | `UIA_NamePropertyId` | the localized accessible name |
| `Tree::toolkit_name` | `UIA_FrameworkIdPropertyId` | `Koru` |
| `accesskit::NodeId` | `GetRuntimeId` only | nothing a client should persist |

**`AutomationId` is the contract; `NodeId` is not.** A `NodeId` is a `u64`, it
cannot hold the four components of an occurrence, and a client only ever meets
it inside a UIA RuntimeId — which Microsoft documents as valid for one session.
So the locator is a string on `author_id`, and the `NodeId` is a hash of that
string used as an in-process handle.

The root falls back to the OS window title for `UIA_NamePropertyId` when
`label` is unset, and answers `UIA_NativeWindowHandlePropertyId` from the HWND.
Koru sets `label` anyway so every platform says the same thing.

## An advertised action set is a hint, not a filter — found by experiment

`Node::add_action` says what a client *should* offer. It does not stop anything
arriving. `accesskit_windows`' `PlatformNode::do_action` checks only whether the
node is disabled, so a request for any action reaches the application whatever
the node advertised.

The concrete case, observed on Windows 11 while running the probe: one
`IUIAutomationInvokePattern::Invoke` on a node that advertises **only**
`Action::Click` arrives as **two** requests —

```text
ActionRequest { action: Focus, target_node: #16938216679920155350, .. }
ActionRequest { action: Click, target_node: #16938216679920155350, .. }
```

UIA focuses the element before invoking it. So an action router must ignore what
it does not perform rather than assume it will only be asked for what it
offered, and a test that asserts "exactly one request arrived" is asserting the
wrong thing.

## Bounds are physical pixels relative to the client area

`accesskit_windows` computes a UIA screen rectangle as
`node.bounding_box() + ClientToScreen(hwnd)` — and `ClientToScreen` answers in
**physical** screen pixels. Koru lays out in **logical** pixels. So the bounds
handed to AccessKit must be logical × `scale_factor`, relative to the client
area's top-left.

`koru_workspace_host::accessibility::projection::build` is the one place that
multiplies, and nothing else in the feature touches a scale. A node with no
bounds is legal — the adapter answers an empty rectangle.

**A view publishes logical rectangles and nothing else.** Every rectangle in
the tree comes back from the function drawing and hit-testing already share —
`koru_ui::caption::controls`, `koru_shell_chrome::tabs::layout`,
`ExploreView::row_band`, `Menu::item_rect` — so a control cannot be published
somewhere other than where it is. The window's own commands reach the host
through `CaptionGeometry`, because the *view* draws the caption and the *host*
performs the command (ADR-338).

## What the schema does not tell you about state, a name and an announcement

Every item below was found by running a probe against a real UIA client rather
than by reading the crate, and each one changed what Koru publishes. The first
six are the reason `Plan 1a.2`'s projection looks the way it does. The last two
are `Plan U0.1`'s — and the label one was a live defect in the first six until
the first-run probe went looking for something else and found it.

### `position_in_set` is **zero-based**, and the adapter adds the one

`accesskit_windows`' `NodeWrapper::position_in_set` is
`self.0.position_in_set().map(|p: i32| p + 1)`. So a node that stores the
number a person hears — *1 of 3* — is read by a client as **2 of 3**. Store the
index; let the adapter count.

The probe pins it: the fixture's second row stores `1` and the client reads
`2`.

### `size_of_set` is read off the **container**, never off the item

`size_of_set_from_container` is `self.selection_container(filter).and_then(|c|
c.size_of_set())` — the item's own `size_of_set` is not consulted at all on
Windows. And `selection_container` only finds a parent whose role is one
AccessKit counts as a container of selectable children: `ComboBox`,
`EditableComboBox`, `Grid`, `ListBox`, `ListGrid`, `Menu`, `MenuBar`,
`MenuListPopup`, `RadioGroup`, `TabList`, `Toolbar`, `Tree`.

So *"3 of 12"* needs the **12 on the `Tree`** and the **3 on the `TreeItem`**,
and a `Role::Group` wrapping the items would swallow the total silently.

**`Role::List` is not on that list, and `Role::ListBox` is.** The obvious role
for a list of rows is the one that does not work: an item under a `Role::List`
answers `position_in_set` and no `size_of_set` at all, which is *"3"* with no
*"of 12"* and no error anywhere. The Tasks page is a `ListBox` of
`ListBoxOption`s for that reason (Plan 2.1), and the roles are load-bearing
rather than decorative.

### A node that says whether it is selected **cannot be invoked**

`accesskit_consumer::is_invocable` is, in full:

```text
self.is_clickable(parent_filter)
    && !self.is_text_input()
    && !matches!(self.role(), Role::Document | Role::Terminal)
    && !self.supports_toggle()
    && !self.supports_expand_collapse()
    && self.is_selected().is_none()
```

The reasoning is sound — selecting *is* the action, so Invoke would be a
synonym — but the consequence is easy to walk into: **`set_selected(false)` on
every row of a list leaves a client unable to invoke any of them.** A client
uses `SelectionItem` instead, and `accesskit_windows`' `Select` turns straight
back into `Action::Click` — *except* when the value would not change, where it
returns without sending anything. Selecting the already-selected item is a
no-op by construction.

What Koru does with that:

- **Tabs and the navigator's projection buttons** carry `selected` on every
  one, true and false alike. They are what selection is *for*, "not selected"
  is worth announcing about one, and `Select` is the gesture a client should
  use.
- **A document row** carries `selected` only when it is the open one. It is
  AccessKit's own guidance for the flag ("consider whether it would be
  appropriate for a screen reader to announce 'not selected'" — for a library
  row, it is not), and it is what keeps `Invoke` on every row a person might
  want to open. The one row that loses Invoke is the one already showing.
- **A button never carries it.** A `Role::Button` with `selected` would be a
  button that cannot be pressed.

The probe asserts the exclusivity both ways round, so if a future AccessKit
relaxes it the assertion fails and the projection can be simplified.

### `toggled` costs a node its Invoke pattern too — and unlike `expanded`, gives something back

Same clause again: `!self.supports_toggle()`, where `supports_toggle` is
`self.toggled().is_some()`. The difference is what replaces it.
`accesskit_windows` 0.32.1 **does** implement Toggle, and all three halves of it
are there:

```rust
fn is_toggle_pattern_supported(&self) -> bool {
    self.0.toggled().is_some() && !self.is_selection_item_pattern_supported()
}
// (UIA_TogglePatternId, IToggleProvider, …, is_toggle_pattern_supported, (
//     (UIA_ToggleToggleStatePropertyId, ToggleState, toggle_state, ToggleState)
// ), (
//     fn Toggle(&self) -> Result<()> { self.click() }
// ))
```

So a node carrying `toggled` reads back as `ToggleState_{Off,On,Indeterminate}`
and the pattern's own `Toggle()` arrives at the provider as **`Action::Click`**
— the same request the pointer's press produces. That is an even trade rather
than the pure loss `expanded` is: Invoke goes, Toggle replaces it, and the
gesture ends in the same place.

The consequence is the rule rather than the mechanism: **set it on a control
that really is a checkbox, and nowhere else.** A `Role::Button` given `toggled`
would be a button a client cannot invoke, in exchange for a state nobody asked
it for. Koru sets it on exactly two things — a checklist row's box in a document
(`n:<uuid>#check`, carrying `NodeMeta::done`) and the tick on a Tasks page row
(`n:<uuid>#task-check`, carrying the canonical lifecycle) — and on nothing else.
See ADR-345-checklistandtask.

Note the second half of `is_toggle_pattern_supported`: a node that is a
**selection item** gets no Toggle. For `Role::RadioButton` and
`Role::MenuItemRadio` the adapter deliberately reports `toggled` *as*
`SelectionItem.IsSelected` instead, which is the ARIA mapping; for a
`ListBoxOption`, `Tab` or `TreeItem` it is `selected` that decides. Two ways to
say "this one is on", and which one you get is the role's.

### `expanded` costs a node its Invoke pattern and buys nothing on Windows

Same clause: `!self.supports_expand_collapse()`, where `supports_expand_collapse`
is `self.data().is_expanded().is_some()`. And `accesskit_windows` 0.32.1
implements **no ExpandCollapse pattern at all** — its `patterns!` table has no
`UIA_ExpandCollapsePatternId` row — so there is nothing to replace Invoke with,
and `ExpandCollapseState` never reaches a client either.

Setting `expanded` on Windows today is therefore pure loss: the state is
unreadable *and* the control becomes unusable. Koru sets it nowhere. The
`Action::Expand` / `Action::Collapse` requests are still advertised and routed —
they cost nothing and the routing is tested — so the day the adapter grows the
pattern, folding works with no further change.

**Revisit when `accesskit_windows` implements ExpandCollapse.** Until then, a
folded branch is silent to a Windows screen reader; that is the adapter's gap,
not the projection's.

### `level` does not reach a Windows client at all

There is no `UIA_LevelPropertyId` row in `accesskit_windows` 0.32.1's
`properties!` table. `Node::set_level` is correct in the schema and inert on
Windows; the probe records the value coming back as absent rather than
asserting it. Koru publishes it anyway — it costs a `usize`, it is right, and
other platforms may read it.

### `GetFocusedElement` is the desktop's answer, not a window's

UIA's focused element is global. The probe's window is created
`with_active(false)` so it does not take over whoever is running it, so nothing
in that process holds real keyboard focus and the answer is whatever does. The
probe records it and asserts nothing about it. **The published focus is
verified two other ways**: the pure suite asserts the view's focus resolves to
a node in the tree (`Counters::unresolved_focus` stays zero), and a person
checks what a screen reader announces. Record that live result separately from
the pure suite.

## Saying something without being asked: `live` is a name-change diff

Established in **Plan 1b.4**, by reading `accesskit_windows` 0.32.1 and
`accesskit_consumer` 0.35.0 and then confirming every line of it from outside
the process in `crates/koru-workspace-host/tests/accessibility_native.rs`.

**There is no announcement API.** AccessKit has no "say this now" call and
`PaneAccessibility`'s only output is a whole `TreeUpdate`. What there is instead
is `Node::set_live(Live::{Polite, Assertive})`, and `accesskit_windows` maps it
onto `UIA_LiveSettingPropertyId` and raises `UIA_LiveRegionChangedEventId` from
its tree-diffing change handler. So an announcement is *a node whose label
changed*, which has three consequences worth knowing before designing anything
on top of it.

**The event fires on a name change, and on nothing else.** The handler's guards
are, in full: on `node_added`, `wrapper.name().is_some() && node.live() !=
Live::Off`; on `node_updated`, that plus `new_node.live() != old_node.live() ||
old_node_was_filtered_out || new_name != old_wrapper.name()`. A live node
republished with an identical label raises nothing. **That is what makes
"announce once" a property of the platform rather than a rule a projection has
to remember** — and the flip side is that a live node whose label carries a
*status* (one that moves with every keystroke) is spoken on every keystroke.
Koru's save status is therefore two nodes: a silent `Role::Label` that answers
when asked, and a `Role::Alert` marked `Live::Polite` whose label only ever
carries news (`apps/koru/src/mainview/accessibility/persistence.rs`).

**`live` is inherited.** `accesskit_consumer::Node::live` is
`self.data().live().unwrap_or_else(|| self.parent().map_or(Live::Off, |parent|
parent.live()))`. Marking a *container* live makes every descendant live, so it
goes on the leaf that speaks and never on the group around it. The probe reads
`UIA_LiveSettingPropertyId` off both the announcement (`1`, Polite) and the
status beside it (`0`, Off) to hold that.

**A live node with no label says nothing**, because of the `name().is_some()`
guard. Clearing the label withdraws an announcement as effectively as removing
the node — and removing it is the riskier of the two, because the event carries
an element the client reads *afterwards*.

**One change was delivered to a subtree-scoped handler more than once.**
Observed on Windows 11 26200: one label change, `AddAutomationEventHandler` with
`TreeScope_Subtree` on the window root, and the handler ran **twice**. It cannot
be a second publish — a republish with the identical label is silent, which the
same run asserts — so the duplication is UIA's delivery, not the provider's.
The same lesson as `Invoke` arriving as two requests: **assert *at least* one,
never exactly one.** What a provider controls is that nothing is raised when
nothing changed, and that is the assertion worth making.

### A `Role::Label` takes its accessible name from its **value**

The one that cost the most, because the failure is silent and looks like
nothing at all. `accesskit_windows`' `NodeWrapper::name` is:

```rust
pub(crate) fn name(&self) -> Option<WideString> {
    let mut result = WideString::default();
    if self.0.label_comes_from_value() {
        self.0.write_value(&mut result)
    } else {
        self.0.write_label(&mut result)
    }
    .unwrap()
    .then_some(result)
}
```

and `accesskit_consumer::label_comes_from_value` is, in full,
`self.role() == Role::Label`.

So a `Role::Label` carrying only `Node::set_label` has **no name at all** to a
Windows client: `UIA_NamePropertyId` comes back as an empty string, which is
what an inspector shows and what a screen reader reads. The probe found it by
asking a live status node for its name and getting `""` while every other
property on the same node was correct.

`Role::Label` maps to `UIA_TextControlTypeId`, and the reasoning behind the
branch is sound — a static text control's *content* is its name — but nothing
in the schema says so, and `Node::set_label` on a `Role::Label` is the obvious
thing to write.

What Koru does: `koru_workspace_host::accessibility::projection::build` copies a
`Role::Label`'s `label` into its `value` when the caller has not set one. In one
place rather than at every call site, so a projection can go on saying
`.with_label(…)` about a label. `label` is set as well, because other platforms
read that one.

Two consequences worth keeping in mind:

- **A label gets no `ValuePattern`.** `is_value_pattern_supported` is
  `has_value() && !label_comes_from_value()`, so the value a label carries is
  readable only as its name — which is what you want, and is why setting one
  does not accidentally offer `SetValue` on a sentence.
- **Do not put a status message in `value` and expect it announced.** See the
  live-region section below: the announcement follows the *name*, and for every
  role but `Label` the name is the label.

### Live regions do work on Windows, and they follow the **name**

`accesskit_windows` 0.32.1 implements them, which the roles-and-patterns table
does not make obvious because it is a property rather than a pattern:
`(UIA_LiveSettingPropertyId, live_setting)` is in `properties!`, and
`Live::{Off, Polite, Assertive}` maps straight onto `LiveSetting`.

The event comes out of the adapter's tree diff rather than from anything a
provider asks for. From `adapter.rs`:

- **`node_added`** queues `UIA_LiveRegionChangedEventId` when
  `wrapper.name().is_some() && node.live() != Live::Off`.
- **`node_updated`** queues it when the node still passes the filter, still has
  a name, is not `Live::Off`, **and** one of: its live setting changed, it was
  previously filtered out, or its **name changed**.

Three things follow, and each of them shapes what a status node has to look
like:

- **The message has to be the name.** A node whose text lives in `value` and
  whose name never changes is published and never announced. (For a
  `Role::Label` those are the same string — see above — which is the one place
  the two rules meet.)
- **A node that is *added* announces itself**, so a status that appears where
  there was none needs no name change to be heard. Koru's first-run projection
  leans on this: moving to the finishing screen mints a new screen instance, so
  the primary button — whose label has become *Creating your Profile and
  workspace* — is a new node with a live setting, and the add is the
  announcement.
- **Republishing an identical node announces nothing**, which is what makes a
  full-tree publish safe for a live region as well as for text.

Koru sets `live` on exactly two things: the primary action while a creation is
in flight (`Live::Polite` — it is progress), and a creation failure
(`Live::Assertive` — the person is waiting on it). Everything else is `None`,
which is `Live::Off`.

Verified from outside the process by
`crates/koru-workspace-host/tests/accessibility_onboarding_native.rs`, which
reads `UIA_LiveSettingPropertyId` back as `2` (Assertive) and the name back as
the failure message.

## The consumer filter can hide a node you published

`accesskit_consumer`'s `common_filter` is what decides whether a node appears in
UIA's control view:

- `Role::GenericContainer` and `Role::TextRun` are **excluded from the node
  list** (their children still appear) — use `Role::Group` for a container a
  client should see.
- A hidden node excludes its whole subtree.
- If a parent has `clips_children`, a child whose bounding box does not
  intersect the parent's is **excluded**, unless a neighbouring sibling is
  inside it. Off-screen rows stay reachable by that exception, which is worth
  knowing before Plan 1b.1 builds a scrolling document.

**Koru sets `clips_children` nowhere**, which is the simpler half of the same
answer: the explore column publishes *every* row in its cache at the rectangle
it would occupy, scrolled off the top or below the fold included, and a row
that is off screen carries a negative or over-large `y` rather than being
absent. `Action::ScrollIntoView` on one reveals it through the pane's own
scrolling. A tree that culled the way drawing culls would tell a screen reader
the library ends at the fold.

## Running the interactive probe

```text
cargo test -p koru-workspace-host --test accessibility_native -- --ignored --nocapture
```

It opens a real 480×320 window for well under a second, creates a UIA client in
the COM multi-threaded apartment, reads the root's `AutomationId`, finds a
control by `FindFirst` on that property and invokes it.

Since Plan 1a.2 it also publishes a small tree of **document entries with a
duplicate title** and asks the questions the four findings above came from:
which number a client reads back for `position_in_set`, where `size_of_set` has
to live, whether a node carrying `selected` still offers Invoke, and whether
`level` arrives at all. Each of those is an assertion, so a future AccessKit
that changes one fails here rather than silently changing what a screen reader
says. The window is created
with `with_active(false)` so it does not take the foreground.

Two details it depends on, both learned the hard way in AccessKit's own suite:
the **client** initialises COM (`COINIT_MULTITHREADED`) and the **provider**
deliberately does not, and the event loop is built with
`EventLoopBuilderExtWindows::with_any_thread(true)` because the Rust test
harness does not run tests on the process's main thread.

## Running the first-run proof

```text
cargo test -p koru-workspace-host --test accessibility_onboarding_native -- --ignored --nocapture
```

It opens a real 1000×680 window for about eighty seconds, hosts a real
`koru_workspace_host::onboarding::OnboardingView` in it, and drives it from a
UIA client on another thread. What it establishes that the pure suite cannot:

- the **first-run** window answers a client at all — before Plan U0.1 it showed
  one window element with nothing inside it;
- *Continue offline*, the account-free way into Koru, is findable by
  `FindFirst` on `AutomationId` from a locator built out of the window number,
  the view slot, the screen instance and the control's keyword — nothing is read
  out of the tree to find it;
- `Invoke` on it reaches the form and moves it to the Profile step, arriving as
  `Focus` then `Click` exactly as the action-set section above predicts;
- the welcome screen's control is then **not findable by its old locator**,
  which is screen retirement seen from outside;
- a `Role::TextInput`'s contents arrive on `ValuePattern`, with `IsReadOnly`
  false;
- and the two findings above.

Same two harness details as the other probe: the **client** initialises COM
(`COINIT_MULTITHREADED`) and the provider deliberately does not, and the event
loop is built with `EventLoopBuilderExtWindows::with_any_thread(true)`. The
window is created `with_active(false)` so it does not take the foreground.

---

# Text

Everything below was established by experiment in **Plan 1b.1**, against
`accesskit` 0.24.1, `accesskit_windows` 0.32.1 (through `accesskit_winit`
0.32.2) and `accesskit_consumer` 0.35.0, on Windows 11 26200.9445. None of it is
derivable from the crate documentation, and several items contradict the obvious
reading of it. The proof is
`apps/koru-document/tests/accessibility_text_native.rs`; the Koru side is
`crates/koru-editor/src/accessibility/`.

## parley already does most of this, and it is a feature flag away

`parley` 0.11 ships `LayoutAccessibility::build_nodes`, which turns a `Layout`
into AccessKit `Role::TextRun` nodes — per line, per style, bidi-ordered within
a line, with `character_lengths`, `character_positions`, `character_widths`,
`word_starts`, font attributes, line links and **stable node ids across
passes**. It is behind parley's optional `accesskit` feature, which depends on
`accesskit 0.24` — the version already pinned here, so turning it on adds no
second `accesskit` to the tree. The workspace `Cargo.toml` turns it on.

`Cursor::to_access_position` / `from_access_position` and
`Selection::{to,from}_access_selection` come with it. **Koru deliberately does
not use those four**; see "a parley cluster is not a Koru character" below.

## Which nodes get a Text pattern, and which cannot

`accesskit_consumer::Node::supports_text_ranges` is the gate, and it is two
conditions:

```rust
(self.is_text_input() || matches!(self.role(), Role::Label | Role::Document | Role::Terminal))
    && self.text_runs().next().is_some()
```

- **`Role::Paragraph` gets no Text pattern.** It maps to
  `UIA_GroupControlTypeId` and is not in the list. A block that must answer
  `ITextProvider` has to be `TextInput` / `MultilineTextInput` / one of the other
  input roles, or `Label` / `Document` / `Terminal`.
- **The run search descends through anything that is not a run.** `text_runs`
  filters with `ExcludeNode` (not `ExcludeSubtree`) for every role but
  `TextRun`, so a container's runs include the runs of its *grandchildren*. That
  is why Koru's document container is `Role::Group`: a `Role::Document` wrapper
  would silently acquire a Text pattern spanning every paragraph, and Koru would
  be publishing two carets for one editor.
- **A container with no runs has no Text pattern at all.** An empty paragraph
  therefore still has to publish one run — an empty one. Dropping it makes the
  paragraph unreadable rather than merely empty.
- `Role::TextRun` is excluded from the platform's control view (see the
  consumer-filter section above), so a run is never a control a client can be on;
  it is addressed only inside a text position. It needs no `AutomationId`.

## The provider-side invariants a run must satisfy

Break any of these and the failure is a panic inside the consumer or a silently
empty answer, not a diagnostic:

| Property | Rule |
| --- | --- |
| `value` | **Must be set on every run**, even the empty one — `is_paragraph_end` does `value().unwrap()` |
| `character_lengths` | `Box<[u8]>`. Each entry is one character's length in **UTF-8 bytes**; the sum must equal `value.len()` |
| `character_positions` / `character_widths` | Optional, but `GetBoundingRectangles` and `RangeFromPoint` return nothing without them. Same length as `character_lengths`. Offsets are **relative to the run's own `bounds.x0`**, in the same units as `bounds` |
| `word_starts` | `Box<[u8]>`, sorted, character indices. Listing index 0 is permitted |
| `text_direction` | An **inherited** property — setting it on the container covers its runs. Without it anywhere on the chain, `bounding_boxes()` returns empty |
| `next_on_line` / `previous_on_line` | What makes several runs one visual line. A run with no `previous_on_line` **is** a line start |

**"A character" is whatever the editor says it is.** AccessKit defines it as
"the smallest unit of text that can be selected" and says explicitly that it
cannot compute the lengths itself. That is not a footnote — see below.

## Two hard limits, both from `Box<[u8]>`

- **A run may hold at most 256 characters.** `word_starts` is a `Box<[u8]>` and
  `accesskit_consumer` looks a position up in it with
  `binary_search(&(character_index as u8))`, so past 255 the value cannot be
  stored *and* the lookup wraps to a wrong answer. parley enforces the cap by
  starting a new span at 256 characters; anyone writing their own projection has
  to do the same. Reading, character motion and line motion are unaffected; word
  motion inside an over-long run is not.
- **A character may be at most 255 bytes.** `character_lengths` is the same
  type. No real grapheme cluster comes close, but a synthetic one can.

## `GetText` has no run-length limit

`ITextRangeProvider::GetText(maxLength)` **ignores `maxLength` entirely** —
`accesskit_windows` notes that the Microsoft documentation only implies a
provider *may* truncate, and returns the whole range. So there is no adapter cap
on how much text one call returns, and `GetText(-1)` is the ordinary call. The
cost is linear in the range, paid on the client's thread.

## What `accesskit_windows` 0.32.1 does not implement

Verified by reading, and each returns an error rather than a wrong answer:

- `ITextRangeProvider::FindText` — `Err(Error::empty())`.
- `ITextRangeProvider::FindAttribute` — `Err(Error::empty())`.
- `ITextProvider::GetVisibleRanges` — returns a null array.
- `ITextProvider::RangeFromChild` — `not_implemented()`; there is no
  embedded-object support in text, and `ITextRangeProvider::GetChildren` always
  returns an empty array. **An inline object inside a text run is invisible to a
  client except as its anchor character.**
- `AddToSelection` / `RemoveFromSelection` — `invalid_operation()`. AccessKit
  supports exactly one text selection.

## The two events, and exactly what triggers them

Both are raised by `accesskit_windows` from its tree-diffing change handler, so
they are a consequence of publishing an update rather than something a provider
asks for:

- **`UIA_Text_TextChangedEventId`** fires when a **`Role::TextRun` node's
  `value` changes**, or a run is added or removed. It is raised on the run's
  nearest ancestor that supports text ranges, and it is deliberately queued
  *before* any selection event.
- **`UIA_Text_TextSelectionChangedEventId`** fires when the text container's own
  `text_selection` property differs from the previous update's, and only when
  the node supported a text pattern in **both** updates.

A run republished with identical properties raises nothing, which is what makes
a full-tree publish acceptable for *correctness* even though it is wrong for
cost.

## An `Action::SetTextSelection` arrives on the container, in run ids

`ITextRangeProvider::Select()` sends `Action::SetTextSelection` with
`ActionData::SetTextSelection(TextSelection)` and `target_node` set to the **text
container**, not to a run. The `TextSelection` inside names `Role::TextRun` node
ids and character indices, which the provider has to map back itself — the
host's occurrence map is keyed by controls, and runs are not controls. In Koru
that mapping lives in `koru_editor::accessibility::DocumentAccessibility`,
which is why it remembers the runs *as last published* rather than recomputing
them.

`Select()` normalises direction: it sends `start`/`end`, never anchor/focus, so
an AT can never ask for a backwards selection.

`ScrollIntoView` on a range arrives as `Action::ScrollIntoView` with an
`ActionData::ScrollHint`, targeted at the run's node.

## A parley cluster is not a Koru character — and this is where the bug was

parley shapes `a👍🏻e\u{301}b` into clusters of `1, 4, 4, 1, 2, 1` bytes: the
skin-tone modifier is its own cluster and so is the combining acute. Koru's
`Paragraph::before`/`after` step `1, 8, 3, 1` — extended grapheme clusters —
because landing the caret inside a ZWJ sequence and typing splits it into tofu.

Publishing parley's clusters therefore publishes caret positions the arrow keys
can never produce, and lets an AT ask for a selection inside an emoji. Koru
merges parley's clusters back to grapheme boundaries after `build_nodes`
(`merge_to_graphemes`) — the safe direction, because every grapheme boundary is
also a cluster boundary — and keeps its own byte-offset map per run rather than
using `Cursor::to_access_position`, which answers in parley's clusters.

**Word boundaries are close but not identical.** parley's `word_starts` are the
*beginnings* of words, so trailing whitespace belongs to the word before it
(`"Reward "` is one word, `0..7`). Koru's `Ctrl+Right` stops at the *end* of the
word instead (6). Reading is unaffected — a caret at 6 is inside `0..7`, so
"current word" is still right — but an AT that moves by its own `TextUnit_Word`
lands one character further on than the same key would. Recorded rather than
fixed. Treat this as a known difference when verifying word navigation.

## Koru's layout strut has to be removed from the projection

`rowlayout::build_row` shapes `row.text + LAYOUT_STRUT` (a trailing `U+200B`
that pins each line's descent). `build_nodes` slices the string it was given, so
the strut becomes a character in the last run. It is pushed with a transparent
brush, so it is always a *span of its own* — Koru empties that run rather than
deleting it, which keeps the end-of-text position addressable and gives an empty
paragraph the run it needs.

## Running the text proof

```text
cargo test -p koru-document --test accessibility_text_native -- --ignored --nocapture
```

It opens a real 760x620 window for about thirty seconds, hosts a real
`koru_editor::Editor` over a synthetic four-paragraph fixture, and drives it
from a UIA client on another thread — including a real
`IUIAutomationEventHandler` registered with `AddAutomationEventHandler`, because
gate T5 is about *receipt*. Two Windows-binding notes it depends on: there is no
`implement` **feature** on `windows` 0.62 (asking for one is a resolver error;
`windows::core::implement` is unconditional), and the macro expands to
`windows_core::` paths, so `windows-core` has to be a dependency in its own name
as well.

---

# Incremental trees

Everything below was established by experiment in **Plan 1b.2**, against the
same pinned versions, and none of it is in the crate documentation. The proof is
the same `apps/koru-document/tests/accessibility_text_native.rs` plus
`crates/koru-editor/src/accessibility/projection/tests.rs`; the Koru side is
`koru_workspace_host::accessibility::{projection, lifecycle, dump}`.

## A `TreeUpdate` is already a delta, and `tree: None` is legal

`TreeUpdate::nodes` means *the nodes that changed*. The consumer keeps
everything else, and `accesskit_windows` raises its text and selection events by
diffing one update against the previous one — so an incremental provider needs
no platform work at all. A second and later update may set `tree: None`: the
root and the `Tree` were declared once and re-declaring them is optional rather
than required. Verified from outside the process, with a real UIA client reading
and editing across sixteen consecutive delta updates.

The counterpart rule is the one that bites:

## A node is replaced **whole**, so re-sending one drops what you left out

There is no "patch this property". `nodes` is `Vec<(NodeId, Node)>` and each
entry replaces that node entirely. Two consequences that are easy to get wrong
and silent when you do:

- **Re-sending a text container to move its caret drops its runs**, and with
  them the whole Text pattern, unless the update also states its `children`. Koru
  names the run ids without copying the runs
  (`TextBody::Runs::Retained`).
- **Re-sending a container because its *label* changed drops every child** it
  does not list. A rename is not a structural change, so an incremental
  projection has to notice it separately and then supply the full child list
  anyway (`SemanticNode::child_order`). Koru's own interactive proof caught this
  as a failing T1 rather than by reading the docs.

## A dangling `next_on_line` aborts the process, with no diagnostic

`accesskit_consumer::text` walks a visual line with
`node_by_id(next_on_line).unwrap()` (`text.rs`, `line_start`/`line_end`). A run
whose `next_on_line` or `previous_on_line` names an id the update never
published is therefore **not** a wrong answer: it panics inside a
`ITextRangeProvider::Move` call, on a UI Automation thread, in a function that
cannot unwind — which reaches the test harness as

```text
process didn't exit successfully: ... (exit code: 0xc0000409, STATUS_STACK_BUFFER_OVERRUN)
```

with no Rust backtrace at the fault and nothing naming the provider. **A run id
is not only an id; it is also a reference held by its neighbours.** Renaming or
dropping one means fixing every link that names it.
`no_run_links_to_a_run_that_was_never_published` is the assertion that keeps this
from coming back.

## parley's "stable node ids across passes" are stable per *cluster path*

`LayoutAccessibility` remembers a run's id against its `ClusterPath` — which is
`(line index, run index, cluster index within the line)`, all relative. So an id
survives an edit only while the span's **first cluster** stays where it was:

- The run at the start of a line keeps its id through any edit that does not
  move the wrap.
- Every span *after* an insertion point gets a new id, because its cluster index
  moved.
- A span whose path disappears is evicted from the map, so a split and its undo
  do **not** give that span's original id back. A span whose path is unchanged
  by the round trip does keep it — measured: after a split and an undo, the
  paragraph's *first* run had the same id and its last did not.

That matters to Koru because of the layout strut. It is always the last span, an
end-of-text caret position names it, and it therefore acquired a new id on every
keystroke — every end-of-paragraph range a screen reader held went stale, and
nothing said so. `koru_editor::accessibility::projection` gives that one run an
id derived from the paragraph's own (`STRUT_RUN`) and relinks its neighbour;
`the_end_of_a_paragraph_keeps_its_run_id_across_an_edit` pins it.

## A node with no bounds is legal, and it is the answer for something off screen

`Node::clear_bounds` leaves the adapter answering an empty rectangle and
`GetBoundingRectangles` returning nothing. That is the honest report for a
paragraph that is not laid out (Koru shapes only the rows in `reflow`'s band) or
is nowhere near the viewport — and it is also what keeps a scroll cheap, because
a run with no rectangle does not move when the view does and so is not
republished. The node stays in the control view, keeps its `AutomationId`, and
can carry `Action::ScrollIntoView` to be revealed.

## What the 256-character run cap actually does to a long paragraph

Measured, not inferred. A single paragraph of 2,000 identical characters with no
break opportunity in it comes out of `build_nodes` as **nine runs**:

```text
a 2000-character unbroken run became 9 runs of [256, 256, 256, 256, 256, 256, 256, 208, 0] characters
```

Seven full spans at the cap, a remainder, and Koru's emptied strut run. So the
cap is enforced by parley *splitting the span*, not by truncating anything: the
concatenated runs still cover the paragraph's bytes exactly, every `word_starts`
entry stays inside its own run, reading, character motion and line motion are
unaffected, and `GetText` returns the whole 2,000 characters in one call (the
adapter ignores `maxLength` entirely). The only thing lost is a *word* boundary
that would have had to be recorded at an index above 255 inside one span — and
since a span never reaches 256, there is none.

## An inline object has to be a control; it cannot live inside a run

`accesskit_windows` implements no embedded-object support in text at all —
`ITextProvider::RangeFromChild` is `not_implemented()` and
`ITextRangeProvider::GetChildren` always returns an empty array (both listed
above). So there is no way to expose an object *inside* a text range, and a
Koru pill — which reaches the text as one default-ignorable `U+2060` — arrives
at a client as nothing whatever.

The only place left is beside the runs: a **sibling child control** of the
paragraph, carrying the object's own kind and its own label, at the reading
position the anchor occupies. That is what `koru_editor::accessibility::projection::InlineObject`
produces. Two things had to be checked by experiment before it could be relied
on, and both came back clean:

- **A non-run child among the runs does not disturb the text pattern.**
  `accesskit_consumer`'s run search filters by role with `ExcludeNode`, so a
  `Role::Link` or `Role::Label` child is skipped and the runs either side of it
  stay one line. Reading, `TextUnit_Line` stepping and `GetText` are unchanged
  with a pill in the middle of a paragraph.
- **The same-line links are what matter, not child order.** A line is walked
  through `next_on_line`/`previous_on_line`, which only ever name runs.

## A `Role::Label`'s Name comes from its **value**, not its label

`accesskit_consumer::Node::label_comes_from_value()` is exactly
`role() == Role::Label`, and `accesskit_windows`' `name()` branches on it. So a
`Role::Label` node published with `Node::label` set and no `Node::value` answers
`UIA_NamePropertyId` as the **empty string** — the control is in the tree, it is
findable by `AutomationId`, and a screen reader has nothing to say about it.

Found by a real client reading an empty `CurrentName`, not by reading the crate;
nothing in the schema's own documentation suggests it. Koru sets both, and the
rule is written once, in `InlineObject::value`, so a second publisher cannot get
it wrong.

The inverse is worth knowing too: setting a `value` on a node that is *not* a
`Label` gives it a Value pattern nothing performs, so it is not a free thing to
set everywhere.

## There is no way to ask a node which actions it advertises

`Node::supports_action(action)` is the only query; the mask is private and
`Action::n` is `pub(crate)`. Anything that wants to *list* a node's actions — a
dump, an inspector — has to name every variant. There are 22 in 0.24.1;
`koru_workspace_host::accessibility::dump::EVERY_ACTION` is that list, and
`every_accesskit_action_is_listed` fails if the crate grows one.

## Running the incremental proof

The interactive proof now also publishes differences and asks the window to
describe itself:

```text
cargo test -p koru-document --test accessibility_text_native -- --ignored --nocapture
```

It prints the requested dump and the run's own accounting — how many updates
were differences, how many node ids went out in total, and how many paragraph
projections were shaped. The pure counter tests, including the roadmap's
1,000-paragraph fixture, are:

```text
cargo test -p koru-editor --lib accessibility -- --nocapture
```

---

# Editing, actions, and a peer typing somewhere else

Everything below was established by experiment in **Plan 1b.3**, against the
same pinned versions, on Windows 11 26200. The proof is
`apps/koru/src/mainview/accessibility/document/native.rs` — a real UIA client
driving the **shipping container window** rather than a harness around the
editor — plus the pure suites beside
`apps/koru/src/mainview/accessibility/document.rs` and
`crates/koru-editor/src/accessibility/{actions,events}.rs`.

## A text control with no `value` reports `IsReadOnly` — **true** — whatever it is

The one that would have been assumed the other way round. `Node::set_read_only`
reaches a Windows client through exactly one route: `accesskit_windows` lists
`(UIA_ValueIsReadOnlyPropertyId, IsReadOnly, is_read_only, BOOL)` inside the
`UIA_ValuePatternId` entry of its `patterns!` table, and it offers that pattern
only when `is_value_pattern_supported` — `supports_url() || (has_value() &&
!label_comes_from_value())`.

A Koru paragraph publishes no `value`, so it is offered no `ValuePattern`. What
a client asking `UIA_ValueIsReadOnlyPropertyId` on one gets back is not "not
supported" but **`VT_BOOL VARIANT_TRUE`** — UIA's own default for a control
with no `ValuePattern` is *read-only*. Measured:

```text
value_pattern_offered: Some(false)
is_read_only_property:  Some("VARIANT { type: VARENUM(11), value: -1 }")
```

So an editable paragraph reports `IsReadOnly = true` to a client that asks that
property directly. In practice a screen reader decides a text control is
editable from `ControlType = Edit`, the Text pattern and keyboard focusability —
which is why Plan 1b.1's T3/T4 gates passed with real typing — but the property
itself is wrong and there is no honest way to fix it here: giving the paragraph
a `value` to acquire the pattern would also acquire `SetValue`, and no
accessibility request may write a Koru document, so the node would be offering
an operation it refuses. `TextBody::read_only` is published anyway — it is right
in the schema and other platforms read it — and the probe **records** the
Windows answer rather than asserting one, the same way it treats `level`.

**Revisit if `accesskit_windows` grows `ITextEditProvider` or carries
`IsReadOnly` outside `ValuePattern`.**

## A `Scope::Whole` is whole all the way down

Koru's own rule, and it cost a failing test to find. `PaneAccessibility::publish`
**empties** the occurrence map for a whole snapshot — a whole snapshot is
authoritative on its own — while a difference merges into it. So a contributor
that answered a whole snapshot with a difference of its own publishes a focus,
or an action target, naming a node that is in no map anywhere:
`Counters::unresolved_focus` climbs and a screen reader shows a window with no
focus.

The case is not hypothetical: a client detaching and re-attaching leaves every
view's own idea of "what the client is holding" untouched, so a view whose state
says *I published this tab last time* is exactly the view that would take the
difference path. Every contributor has to take `Scope` as an argument and treat
`Whole` as overriding whatever it remembers.

## One select-type-undo raised six text changes and four selection changes

Measured against the shipping window with a real `IUIAutomationEventHandler`
registered `TreeScope_Subtree` on the root:

```text
events_seen: [20014, 20014, 20014, 20015, 20014, 20014, 20015, 20014, 20015, 20015]
```

`20014` is `UIA_Text_TextChangedEventId`, `20015` is
`UIA_Text_TextSelectionChangedEventId`. Three client operations — `Select()` on
a range, typing one character, `Ctrl+Z` — produced ten deliveries between them.
Same lesson as `Invoke` arriving as two requests and a live region firing twice:
**assert *at least* one, never exactly one.** What a provider controls is that
nothing is raised when nothing changed, and that is the assertion worth making.

## The only mutating request a Windows client can send does not reach a paragraph

`accesskit_windows` sends `Action::SetValue` from `IValueProvider::SetValue` and
`IRangeValueProvider::SetValue`, and `Action::ReplaceSelectedText` from nowhere
at all — the string does not appear in the adapter. Both providers are gated on
patterns a Koru paragraph is not offered (see the read-only section above), so
the mutating half of the action set is unreachable from Windows by construction.

Koru refuses both explicitly anyway (`koru_editor::accessibility::actions`),
because an advertised action set is a hint rather than a filter and a provider
that relied on the adapter's gating would be relying on a client being polite.

## Running the shell proof

```text
cargo test -p koru --bin koru accessibility::document::native -- --ignored --nocapture
```

It opens a real 1200×760 container window for well under a minute, hosts a real
`MainView` over an in-memory workspace, and drives it from a UIA client on
another thread: find a paragraph by `AutomationId`, read it through `GetText`,
`Select()` a range, type over it, undo, and receive the events. The window is
created `with_active(false)` so it does not take the foreground.

It lives **inside the binary** rather than in `tests/` because `apps/koru` is a
`[[bin]]`: nothing can depend on it, so a `#[cfg(test)]` module beside the code
is the only place a proof can host the shipping container. Same two harness
details as the other probes: the **client** initialises COM
(`COINIT_MULTITHREADED`) and the provider deliberately does not, and the event
loop is built with `EventLoopBuilderExtWindows::with_any_thread(true)`.

## Windows IsOffscreen does not establish document visibility

The pinned `accesskit_windows` 0.32.1 adapter does not map the UIA IsOffscreen
property. Live M3 inspection returned false for every paragraph, including rows
with no layout or bounds. A false value is therefore not evidence that a
paragraph is visible or safe to click. The runner keeps that raw fact for
diagnostics and uses explicit intersection with the current body viewport for
its document visibility assertions. Pointer focus additionally needs the click
point inside that viewport.

A never-laid-out row deliberately publishes `RowText::Absent`: its paragraph is
discoverable and supports reveal, but it has no TextPattern until layout supplies
text runs. Fixture preparation must reveal and read rows through the existing
scroll path. Reading a fresh, distant paragraph before navigating to it assumes
a stronger contract than this projection provides. Do not synthesize text runs
or reflow the full document from a provider query to satisfy a runner assumption.
## The editor pane origin belongs in text geometry

A shipping MainView draws a leaf under `focused_view_rect()`'s origin. Its editor
layout is local to that pane. Live UIA originally placed an entry at x=855 while
the body began at x=1031: the text projection had omitted the pane translation.
`ViewGeometry::origin` now carries that logical offset separately from the local
scroll, content column and shaping band. Rows, runs, checkboxes and inline objects
receive it once; the host still owns DPI scaling and the adapter owns the screen
origin. The body shares the drawing path's content clip in the focused pane, including in split views, and republishes
when that viewport moves. Zero-origin fixtures alone cannot verify this boundary.
## Published field values must participate in the revision

The reminder provider's revision originally included focus, bounds, validation and
control state, but omitted the phrase/date/time strings published as UIA Values.
Typing another invalid value without moving focus could therefore leave the provider
snapshot unchanged. During the M3 native workflow, the drawn time was `a3:30 PM`
while UIA still returned its earlier empty value; the invalid state had not changed.

`reminder_revision` now folds the existing borrowed `field_text` for all three fields
into the revision. A text-only change must advance the fingerprint even when enabled,
invalid, geometry and focus stay constant. This is separate from whether the value is
valid, and separate from native input delivery: neither a completed SendInput batch
nor a cached UIA Value proves that the app processed the intended text.
