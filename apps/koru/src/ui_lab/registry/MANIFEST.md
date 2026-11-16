# UI Lab inventory — source manifest

What the registry's records were written from, how the sources were searched,
what was left out and why, and how many of each thing there are. The records
themselves are the Rust in this folder (`foundations.rs`, `controls.rs`,
`compositions.rs`, a file per domain crate in `domain/`, and the Button in
`../registry.rs`); this page is the audit
trail beside them, not a second catalog.

**Hand-reviewed.** Every record below was written by reading the module it
names: its module documentation, its public types, functions and constants,
its enums' members and the inspector names its painters register. The search
commands found the candidates; they did not decide what a candidate is. The
drift checks in `tests/drift.rs` hold the records to the source afterwards,
within the limits stated under [Drift checks](#drift-checks).

## Batch 1 — shared APIs and tokens

Reviewed at `3bd19e56` (branch `ui-lab/l1b`), 16 November 2026.

### Sources reviewed

| Owner | Files read | Records |
| --- | --- | --- |
| `koru-ui` | `crates/koru-ui/src/lib.rs`; every one of its 58 public modules (`badge.rs` … `welcome.rs`) and their non-test children `menu/inspection.rs`, `popover/inspection.rs`, `keys/debug.rs`; the private `typography.rs` | Controls, compositions and input support |
| `koru-draw` tokens | `crates/koru-draw/src/theme/tokens.rs`: `Profile`, `Space`, `Radius`, `IconSize`, `metrics`, `alias`, `roles`, `Surface`, `Ink`, `Line`, `Accent` and their `ALL` tables | `T-PROFILE` … `T-ACCENT` |
| `koru-draw` themes | `theme.rs` (palette, type scale, accent and base choices, named colors, terminal palette, appearance preferences, caret, fonts, document type, layout constants), `theme/themes.rs`, `theme/motion.rs`, `theme/overlay.rs`, `theme/preview.rs` | `T-THEMES` … `H-APPEARANCE-PREVIEW` |
| `koru-draw` element layer | `el.rs` (`row`, `col`, `boxed`, `scroll_y`, `text`, `icon`, `leaf`, `spacer`, `divider`; `Length`, `Align`, `Axis`, `TextMode`, `StateRecipe`, `Look`, `Ring`, `Role`, `Action`, `Name`, `Key`), `el/frame.rs` | `P-ROW` … `H-ELEMENT-FRAME` |
| `koru-draw` drawing and support | `scene.rs`, `checkbox.rs`, `chip.rs`, `pill.rs`, `image.rs`, `tile.rs`, `brand.rs`, `icons.rs`, `icons/vector.rs`, `diagram.rs`, `geometry.rs`, `text.rs`, `anim.rs`, `tween.rs`, `inspect.rs`, `installed_fonts.rs`, `emoji.rs`, `scrollbar.rs` | `P-FLOATING-PANEL` … `H-EMOJI`, `C-SCROLLBAR` |
| A2UI | `crates/koru-ai-core/src/mini_apps/composition.rs` (the declaration vocabulary) and `apps/koru/src/mini_app/composition.rs` (how the host draws it) | The `A2ui` field of every record |

Catalog IDs and public names are copied from the component catalog where it
gives them (`C-BUTTON`, `C-COPY`, `C-DIALOG-FRAME`, `C-MEASUREMENT-BADGE`,
`C-MEDIA-VIEWER`, and the KoruX names); its family IDs (`C-CHOICE`,
`C-OVERLAY`, `C-LIST`, `C-LAYOUT`, `C-CHROME`, `C-STATUS`, `C-PICKER`,
`C-ICON`, `C-INPUT`, `H-INPUT`, `P-DRAW`) are kept as each record's
`catalog_family`. Where the catalog gives only a family, the record's ID is
minted the same way the catalog's own component IDs are: `C-` and the
component's name (`C-CHIP`, `C-TOGGLE`). Tokens are `T-`, primitives `P-`,
support code `H-`. Records the catalog has no row for say so with
`catalog_family: None`: `C-SPLIT-BUTTON`, `C-LABEL-CHIP`, `C-CONTEXT-CHIP`,
`C-PROGRESS-RING`, `C-LAUNCHER`, the element layer and four `koru-draw`
support modules.

### Bounded searches

Run from the repository root. Each lists candidates for a person to read; none
is a definition of a component.

```sh
# The module baselines.
grep -nE '^(pub )?mod ' crates/koru-ui/src/lib.rs crates/koru-draw/src/lib.rs crates/koru-draw/src/theme.rs
# Top-level public items of every shared widget module.
grep -nE '^pub (struct|enum|fn|const|static|trait|type)' crates/koru-ui/src/*.rs crates/koru-ui/src/menu/inspection.rs crates/koru-ui/src/popover/inspection.rs crates/koru-ui/src/keys/debug.rs
# Associated items of the shared widgets.
grep -nE '^    pub (const )?fn |^    pub const ' crates/koru-ui/src/*.rs
# The members of every public enum.
awk '/^pub enum /{p=1;print FILENAME": "$0;next} p&&/^}/{p=0} p' crates/koru-ui/src/*.rs crates/koru-draw/src/*.rs crates/koru-draw/src/theme/*.rs
# Component identities the painters register with the inspector.
grep -rn -B1 '^\s*"component",' crates/koru-ui/src crates/koru-draw/src --include=*.rs
# Every koru-draw module's documentation and public items.
grep -nE '^//!|^pub (struct|enum|fn|const|static|trait|type|mod|use)' crates/koru-draw/src/*.rs crates/koru-draw/src/el/frame.rs crates/koru-draw/src/icons/vector.rs
# User-facing literals in shared painters (the defects listed below).
grep -rnE '(simple_layout|layout|text)\(\s*"[A-Za-z][a-z]+' crates/koru-ui/src crates/koru-draw/src --include=*.rs
```

### Exclusions

| What | Why it has no record |
| --- | --- |
| `koru-ui::typography` (private) | The provenance helper shared painters record their text through. Nothing outside `koru-ui` can call it; it appears inside every text-drawing component's inspector record instead. Listed in the module baseline as excluded. |
| `koru-draw::theme::{token_guards, ui_scan}` | Test-only (`#[cfg(test)]`) modules with names the baseline cannot tell from API: guards on the token tables and the source scan of migrated surfaces. Listed in the theme baseline as excluded. (`doc_size_tests` is a test module by name and is skipped like every other.) |
| `koru-draw::icons::data` (`icon_pack_data.rs`) and `icons::vector` | Private: the compiled icon geometry. Its public face (`IconPack`, `draw_vector_icon`, the icon constants) is `P-ICONS`. |
| `koru-draw::el::{record, solve}`, `diagram::{measure, svg, walk}` | Private parts of the element frame and the diagram painter; their public items are re-exported and recorded there. |
| Escape-path members `Space::Px`, `Radius::Metric`, `Radius::Px`, `Length::Px` | Authored raw values the source scan holds to an allowance. They are not in the tables the records list; `Length`'s record lists `Px` because it is a member of that enum. |
| Every `tests.rs`, `*_tests.rs` and `#[cfg(test)]` item | Test code. |
| `crates/koru-ui/examples/` (typography, measurement badge, popover, menu, dialog frame inspection-cost harnesses) | Evidence harnesses over existing production calls, not components. Their evidence belongs to the stories of the components they exercise (L4b). |
| `koru-draw::theme::{CONTACTS, Contact, WEEKDAYS}` | Picker data rather than design values. `CONTACTS` has no consumer outside `theme.rs` (see Oddities); `WEEKDAYS` is the date picker's weekday header. |
| Brand statics in `koru-draw::brand` (`ALL_BRANDS`, `SIGN_IN_MARKS` and each mark) | `P-BRAND` lists the painters, not each product's mark, so no product is named in the registry. |
| Individual `theme.rs` layout constants | `T-LAYOUT-CONSTANTS` is one record for the named sizes and timings outside the token tables; `grep -c '^pub const' crates/koru-draw/src/theme.rs` gives their number at a given commit. |

### Option sets

A record lists the values a production type really takes, a set per type:
an enum's members (`ActionStyle`), associated constants (`Metrics::STANDARD`,
`INLINE`, `COMPACT`), constructor families (`InputStyle::search`, `inline`,
`form`, `bare`) or a token table (`Space::ALL`). Never a universal variant
field. A widget's style, tone, size, state, placement and mode sets carry a
translated label per member because a story's properties panel offers them
to a person; token tables, stored preference values and kinds of data are
shown as their identifiers. Boolean states (enabled, hovered, focused) are
not option sets: they arrive with each story's state matrix (STATES.md).

### Counts

Computed from the registration and the compiled-in source; the drift checks
fail until this table says what they compute.

<!-- counts:begin -->
| Count | Value |
| --- | --- |
| koru-ui public modules | 58 |
| koru-ui private modules | 1 |
| koru-draw public modules | 19 |
| koru-draw theme public submodules | 5 |
| domain crates | 14 |
| domain crate source files | 160 |
| domain crate lib.rs modules | 118 |
| domain crate lib.rs public modules | 26 |
| domain crate modules excluded | 29 |
| records | 168 |
| records: foundations | 59 |
| records: controls | 62 |
| records: application compositions | 47 |
| records: domain crates | 48 |
| kind: token | 25 |
| kind: primitive | 24 |
| kind: widget | 57 |
| kind: support | 32 |
| kind: domain composition | 30 |
| token values listed | 174 |
| symbols | 861 |
| symbols with typed recipes | 2 |
| inspector identities | 41 |
| inspector identity registration sites | 43 |
| disposition: story exists | 1 |
| disposition: no story yet | 129 |
| disposition: nonvisual | 32 |
| disposition: platform-only | 0 |
| disposition: boundary | 6 |
| symbol story: available | 1 |
| symbol story: no story yet | 260 |
| symbol story: nonvisual | 562 |
| symbol story: boundary | 38 |
| A2UI: component | 6 |
| A2UI: host-styled | 1 |
| A2UI: declarable values | 9 |
| A2UI: host only | 9 |
| A2UI: not supported | 836 |
<!-- counts:end -->

A module is not a component: `koru-ui`'s 58 public modules hold the records
above, some modules two (`card`, `menu`, `nav_list`, `text_field`), one a
re-export (`scrollbar`, recorded at its `koru-draw` owner). **Occurrences**
here are the places a shared painter registers a component identity with the
inspector (`KoruButton` is registered from three painters). Where those
components are used across the apps — consumer occurrences — is not an M1
field; it arrives with each story's consumer routes and is verified at L8.

### Drift checks

`tests/drift.rs`, all in memory (`include_str!` at compile time):

- the `pub mod` lists of `koru-ui/src/lib.rs` (and its one private module),
  `koru-draw/src/lib.rs` and `koru-draw/src/theme.rs` against the baselines
  in that file, so a module added or removed fails until it is classified;
- every token record's members against the production `ALL` tables and name
  lists, read as values;
- every enum option set against the enum's members in the source, and
  constant and constructor sets against their declarations;
- every symbol a record names against a declaration under its path;
- every component identity a `koru-ui` or `koru-draw` painter registers
  against the records' identities, both ways;
- the A2UI components, token values and host mappings against the
  composition catalog's own enums (matched exhaustively) and the host
  adapter's source;
- the table above.

This is a module and registered-identity drift check. It does not discover
a new public function inside an existing module, a painter that registers no
identity, or anything outside `koru-ui` and `koru-draw` (batch 2a extends it
to the domain crates, below).

## Batch 2a — the domain UI crates

Reviewed at `bc6c3afc` (branch `ui-lab/l1b`), 16 November 2026: the thirteen
crates the inventory's "Domain and host inventory scope" names first. The
records are in `domain/`, a file per crate; `domain.rs` holds the helpers
they share. `koru-editor`, `koru-markdown-view`, `koru-session-view`,
`koru-app-shell`, `koru-workspace-host` and `koru-webview` are batch 2b
(below).

### Sources reviewed

Every non-test `.rs` file of each crate was read for its module
documentation, its public types, functions and constants, its enums'
members, its constructors (for what building a surface does) and every
`koru_draw::inspect` scope it opens. In `koru-ai-ui/src/lib.rs` (16,578
lines, the inline test module from line 10,754) and `koru-search-ui/src/lib.rs`
(7,326 lines, tests from 4,898) the type declarations, constructors, public
methods and draw entry points were read and the private interaction code
between them was searched rather than read line by line; `koru-socials-ui`'s
`paint.rs` was read in part and outlined by its signatures, all of its items
being private.

| Crate | Non-test files | `lib.rs` modules | Excluded | Records | Symbols | Component identities | Dispositions |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `koru-ai-ui` | 59 | 35 | 3 | 16 | 113 | `ComposerQueue` | 12 no story yet, 3 nonvisual, 1 boundary |
| `koru-agents-ui` | 4 | 3 | 0 | 2 | 20 | — | 2 no story yet |
| `koru-appearance-ui` | 1 | 0 | 0 | 1 | 13 | `EmojiColorPanel` | 1 no story yet |
| `koru-artifacts-ui` | 12 | 9 | 3 | 2 | 40 | — | 2 no story yet |
| `koru-canvas-ui` | 11 | 10 | 1 | 3 | 31 | — | 1 no story yet, 1 nonvisual, 1 boundary |
| `koru-chats-ui` | 9 | 8 | 3 | 2 | 28 | — | 1 no story yet, 1 nonvisual |
| `koru-runs-ui` | 11 | 10 | 3 | 2 | 47 | — | 1 no story yet, 1 nonvisual |
| `koru-search-ui` | 12 | 11 | 6 | 3 | 54 | `SearchModal` | 2 no story yet, 1 nonvisual |
| `koru-settings-ui` | 9 | 8 | 2 | 1 | 40 | — | 1 no story yet |
| `koru-shell-chrome` | 11 | 10 | 0 | 10 | 59 | `KoruBranchChip` | 5 no story yet, 5 nonvisual |
| `koru-socials-ui` | 5 | 4 | 2 | 2 | 30 | — | 2 no story yet |
| `koru-voice-ui` | 2 | 1 | 0 | 2 | 19 | — | 2 no story yet |
| `koru-workspaces-ui` | 10 | 6 | 6 | 0 | 0 | — | — |
| **13 crates** | **156** | **115** | **29** | **46** | **494** | **4** | |

The records, by crate (IDs marked * are minted here, in batch 1's style:
`C-` and the component's name for a surface or widget, `H-` for support,
`P-` for a primitive; the AI crate's carry `C-AI-`/`H-AI-` so its many parts
cannot collide with another crate's):

- `koru-ai-ui`: `C-AI-CHAT-SURFACE`*, `C-AI-CONVERSATION`*,
  `C-AI-TOOL-CALL-DETAIL`*, `C-AI-NEW-DIVIDER`*, `C-AI-COMPOSER`*,
  `C-AI-COMPOSER-DOCUMENT`* (the composer's text, a boundary: rich text
  `koru-editor` edits), `C-AI-COMPOSER-PLUS-MENU`*, `C-AI-ATTACHMENT-STRIP`*,
  `H-AI-VIEWER-LIFT`*, `C-AI-MODEL-PICKER`*, `C-AI-MICROPHONE-PANEL`*,
  `C-AI-DICTATION-ROW`*, `C-AI-PROVIDER-SETUP`*, `H-AI-CHAT-STATUS`*,
  `H-AI-CHAT-DIAGRAMS`*, `H-AI-AGENT-PROVIDERS`*.
- `koru-agents-ui`: `C-SESSION-SURFACE`*, `C-WATCH-BAND`*.
- `koru-appearance-ui`: `C-EMOJI-COLOR-PANEL`* (COMPONENTS.md names the
  panel and gives it no ID).
- `koru-artifacts-ui`: `C-ARTIFACTS-COLUMN`*, `C-ARTIFACT-TAB`*.
- `koru-canvas-ui`: `C-CANVAS-SURFACE`*, `H-CANVAS-CAMERA`*,
  `C-CANVAS-OBJECTS`* (a boundary: the cards, images, workflow nodes and
  arrows the surface draws as its own content).
- `koru-chats-ui`: `C-CHATS-LIST`*, `H-CHAT-STATUS`*.
- `koru-runs-ui`: `C-RUNS-SURFACE`*, `H-RUN-SUMMARY`*.
- `koru-search-ui`: `C-SEARCH-MODAL` (COMPONENTS.md's ID, family
  `C-OVERLAY`), `C-SEARCH-SURFACE`*, `H-ENTITY-SEARCH`*.
- `koru-settings-ui`: `C-SETTINGS-SURFACE`*. It stands for the owed
  "Settings page builder" too: the builder and its segmented-or-dropdown
  rule (`page::Builder`, `row_choice`) are crate-private, so the rule is
  reached only through the surface.
- `koru-shell-chrome`: `C-BRANCH-CHIP` (COMPONENTS.md's ID, family
  `C-CHROME`, Android and iOS not applicable), `C-STATUS-CHIP`*,
  `C-STATUS-MARK`*, `C-TAB-STRIP`*, `P-PAGE-EMOJI`*, `H-BANNER-SLOTS`*,
  `H-CAPTION-LAYOUT`*, `H-LANE-LAYOUT`*, `H-WINDOW-REGIONS`*,
  `H-DRAG-REORDER`*.
- `koru-socials-ui`: `C-SOCIALS-SURFACE`*, `C-SOCIALS-NAVIGATOR`* (the
  "Socials navigator composition" COMPONENTS.md describes).
- `koru-voice-ui`: `C-VOICE-SURFACE`*, `C-VOICE-HUD`*.
- `koru-workspaces-ui`: none; the crate is a controller that draws nothing
  (COMPONENTS.md says so), and its drawing is the app's Manage tab (batch 3).

**Kinds.** A surface a domain crate owns is `DomainComposition`. A small
painter with its own paint that two hosts draw — the branch chip, the
caption status chip, the status mark — is a `Widget`, filed under controls;
the chrome's geometry and the nonvisual code a host drives directly are
`Support`; the page emoji painter is a `Primitive`. No domain record has a
recipe or a story; every one is "no story yet", nonvisual support, or a
boundary.

**Public names.** The source's own name, except where it is an ordinary word
the record's translated name contains (the test refuses an identifier inside
translated text): the tab strip, caption layout and drag reorder records use
a module-qualified painter or function (`tabs::draw`, `caption::tab_strip`,
`reorder::drop_index`), the Socials navigator its outcome type
(`NavigationOutcome`; its state struct is private) and the run summary its
type (`Summary`).

**Symbol paths** are the paths a caller writes. Where a module is private and
`lib.rs` re-exports its items, the record's module is the crate root
(`koru_voice_ui::VoiceHud`), and the drift check finds the declaration in
any of the crate's files only if `lib.rs` declares or re-exports the name.
`koru-ai-ui`'s 35 modules are all private and its one public type is
`ChatSurface`, so each of its records lists the surface's public methods that
are declared in that part's own file.

### Private parts of the AI chat surface

A record must name a public symbol, and fourteen visible parts of
`ChatSurface` have none that puts them on screen: everything they own is
`pub(crate)` and they are drawn only by `ChatSurface::draw`. They have no
record of their own; the few public queries some carry are listed on
`C-AI-CHAT-SURFACE`, and the queue's `ComposerQueue` identity is claimed
there. They are: the permission request card (`lib.rs`), the agent
questions card (`questions.rs`, an element tree), the link card
(`link_card.rs`), the agent work summary (`work_view.rs`, `tool_groups.rs`),
the queued messages (`queue.rs`, `ComposerQueue`), the composer notice line
(`footer.rs`), the context window report (`context_window.rs`), the
approval mode picker (`approval_modes.rs`), the tool approval picker and the
workspace access panel (`privacy.rs`), the skill and command list
(`skills.rs`), the save chat panel (`lib.rs`), the jump-to-latest button and
the add-to-chat pill (`quote_pills.rs`). The reply bodies are
`koru-markdown-view` content (batch 2b) and have no boundary record here.

### What building each surface needs

INVENTORY.md warns that a public export is not a promise of side-effect-free
construction. Read from each constructor:

- **Pure** — `KoruBranchChip`, the status chip and mark, the tab strip, the
  page emoji and all the chrome geometry; `Watching`; `VoiceSurface` and
  `VoiceHud` (a host-built view model, no device); `EmojiColorPanel` (the
  editor's in-memory emoji search and a color row); `CanvasSurface` (its own
  `TextEngine`, cloned from the process font prototype; no disk or network
  anywhere in the crate) and its camera; `ChatStatus`; `SearchModal` (a rect
  from the viewport; `draw` borrows the host's surface); `RunsSurface::new`
  (an inert link; `RunLinks::scripted` is the service-free seam its own suite
  drives).
- **Needs a host's services, starts nothing itself** — `SessionSurface::new`
  takes the host's handle to the session backend and shared `SessionStats`;
  `RunsSurface::with_links` and `SettingsSurface::with_links` adopt a link the
  host built.
- **Starts threads that dial a service** (skipped under `cfg(test)` and the
  `no-reader` feature) — `ChatsSurface::new`, `ArtifactsSurface::new` and
  `ArtifactTab::new` (gateway readers; `without_reader` builds each inert, for
  a test or a host that feeds it); `SearchSurface::new`/`new_at`/
  `for_workspace` and `EntitySearch::new_at` (two store reader threads;
  `SearchSurface` also seeds itself from the process search session);
  `Surface::new` in `koru-socials-ui` (a Store worker thread);
  `RunsSurface::for_host` and `SettingsSurface::for_terminal` (a gateway
  worker each). `ChatSurface::new`/`new_at` build an in-memory composer
  workspace and read the system locale; its service client starts only at
  `activate()`.
- **Changes process state when driven** — `SettingsSurface`'s preference
  rows read and, when pressed, write `koru_draw::theme`'s process globals, so
  pressing a control in a story would change the app's theme state.

### A2UI

Neither catalog names a domain crate's surface. The fixed catalog's
`DocumentSearch` (`crates/koru-ai-core/src/mini_apps.rs`,
`("search", "DocumentSearch")`) is the mini-app chrome's own field and
floating results in `apps/koru/src/mini_app/chrome.rs` — an app-local
surface, batch 3's to record with that mapping — and it reads through
`koru_search_ui::EntitySearch`. `H-ENTITY-SEARCH` marks `EntitySearch`,
`new_at`, `request`, `poll`, `pending` and `EntitySearchUpdate` host-only,
and `tests/drift.rs` holds each to the line of the chrome that uses it.
`TaskDocument`, `TaskListSelector`, `TaskCollection` and `DocumentHost` are
backed by `apps/koru` (`mini_app`, `tasks_view`, `leaf`): batch 3.
`koru-ai-ui`'s `mini_apps` module only hands a validated declaration from
the chat's stream to the host, and `koru-artifacts-ui`'s `OpenedArtifact`
opens a saved mini-app artifact as a Markdown tab; neither serves a catalog
component.

### Platforms

Every domain crate is linked by `apps/koru` (the voice crate by
`apps/koru-agent`), so a record's default is the shared desktop one: Windows
existing, macOS and Linux the same source unverified, Android and iOS not
audited. Exceptions, each from PLATFORMS.md or COMPONENTS.md: the branch
chip is not applicable on Android and iOS; the search modal, the Settings
surface and both Socials records are unavailable there ("no consumer of
this desktop modal", "no implementation of these desktop contracts", "no
host adapter"). Linux has no column in those records and keeps the shared
source default.

### Bounded searches

Run from the repository root, over `crates/<crate>/src` for each of the
thirteen crates. Candidates for a person to read, never definitions:

```sh
# Module baselines.
grep -nE '^(pub )?mod |^#\[cfg' crates/koru-*-ui/src/lib.rs crates/koru-shell-chrome/src/lib.rs
# Public items and the public methods of each impl block.
grep -nE '^pub (struct|enum|fn|const|static|trait|type|use)|^    pub (const )?fn ' crates/koru-<crate>/src/**/*.rs
# Every inspector scope, its name and kind ("component" ones are identities).
grep -rnE 'inspect::(scope|scope_at|scope_hoisted|leaf)\(' crates/koru-<crate>/src -A3
# Constructors that start threads or dial a service.
grep -rnE 'thread::Builder|spawn\(|Reader::start|Backend::new|::open\(' crates/koru-<crate>/src
# Who links each crate.
grep -rln 'koru-<crate>' --include=Cargo.toml apps crates
# User-facing literals in painters (the defects below).
grep -rnE '(simple_layout[a-z_]*|label|ActionButton::new|Confirm::new|text)\(\s*"[A-Za-z][a-z]+' crates/koru-<crate>/src
# The A2UI hosts' use of a domain crate.
grep -rn 'koru_[a-z_]*_ui::' apps/koru/src/mini_app apps/koru/src/mainview/mini_app*.rs
```

### Exclusions

Every module a crate's `lib.rs` declares is in its baseline in
`tests/drift/domain.rs`, as records or as an exclusion with its reason. The
excluded ones:

| Crate | Module | Why it has no record |
| --- | --- | --- |
| `koru-ai-ui` | `mini_apps` | Orchestration: hands a validated mini-app declaration from the chat's stream to the host; draws nothing |
| `koru-ai-ui` | `chats` | Orchestration: what the surface tells its host about the Chats list, which is `koru-chats-ui`'s |
| `koru-ai-ui` | `reader` | IO: the background client thread every AI service call crosses |
| `koru-artifacts-ui` | `gateway`, `reader` | IO: the gateway client and the read and register threads |
| `koru-artifacts-ui` | `result` | IO: `read_result`, a blocking gateway read a host calls from its own worker |
| `koru-canvas-ui` | `dependency` | Test-only dependency fence |
| `koru-chats-ui` | `gateway`, `reader` | IO: the gateway client and the list and delete threads |
| `koru-chats-ui` | `seen` | Data: the per-device book of what has been seen, one host-stored line |
| `koru-runs-ui` | `dependency` | Its only items are `#[test]` functions (not `cfg(test)`-gated; see defects) |
| `koru-runs-ui` | `index` | IO: the run index worker; its rows and motion are data a navigator draws from |
| `koru-runs-ui` | `links` | IO: the run views' gateway worker; `RunLinks::inert`/`scripted` are its service-free seams |
| `koru-search-ui` | `catalog` | Data: the workspace and profile catalog read from the store |
| `koru-search-ui` | `filters` | Private helper: the `/key` filter grammar |
| `koru-search-ui` | `reader`, `store` | IO: the reader threads and the store client |
| `koru-search-ui` | `state` | IO: the session state and the persisted choices file; `persist_choices_in` is startup plumbing (`apps/koru/src/main.rs`) the Lab is dispatched before |
| `koru-search-ui` | `debug` | IO: routing diagnostics to stderr |
| `koru-settings-ui` | `links` | IO: the typed settings client a host hands the surface |
| `koru-settings-ui` | `dependency` | Test-only dependency fence |
| `koru-socials-ui` | `backend` | IO: the Store worker; public and hidden only for an example |
| `koru-socials-ui` | `preview` | Private helper: synthetic fixtures under `no-reader` (ready-made story states later) |
| `koru-workspaces-ui` | `action`, `controller`, `pending`, `state` | Orchestration: the workspace controller, which draws nothing |
| `koru-workspaces-ui` | `links` | IO: the gateway thread and its fixtures feature |
| `koru-workspaces-ui` | `dependency` | Test-only dependency fence |

Symbols left out of a record on purpose: re-exports of another crate's items
(`koru_shell_chrome::title_icon::split_leading_emoji`,
`koru_chats_ui::status::FRAME`, the `koru_ai_core`/`koru_markdown_view` types
the chats and artifacts roots re-export), recorded at their owner or in their
own batch.

### Counts

The table under Batch 1's counts is the whole registration's and now
includes these crates; its domain rows (crates, source files, `lib.rs`
modules, excluded modules, records owned by a domain crate) are computed
from `tests/drift/domain.rs`. Modules, records and occurrences are separate
numbers: 115 `lib.rs` modules hold 46 records; 23 inspector scopes are
opened across the crates, of which 4 register a component identity, each at
one site; where these surfaces are drawn across the apps is not counted here
(consumer routes arrive with stories, L8).

### Drift checks added

`tests/drift/domain.rs` compiles in all 156 files and each crate's module
baseline. With them, `tests/drift.rs` now also checks:

- every module of each domain crate's `lib.rs` against its baseline, and
  every record of that crate reached from a module or from `lib.rs` itself;
- every symbol a domain record names against a declaration where its path
  says (a crate-root path against `lib.rs`'s declarations and re-exports);
- every option set against the enum, constants or functions it lists;
- every `"component"` identity registered anywhere in these files against
  the records, both ways;
- the host-only search symbols against the mini-app chrome's source.

Like batch 1's, it does not discover a new public item inside a module
already classified, a nested child module, or a painter that registers no
identity.

### Defects found (not fixed)

Recorded for the owners; nothing in a crate was changed.

- **Hard-coded English a person reads.** `koru-appearance-ui` does not use
  `koru-i18n` at all ("COLOR", "EMOJI", "Save", "Cancel", "Clear emoji" in
  `lib.rs`). `koru-search-ui` has no `koru-i18n` dependency: sort labels,
  placeholders, section headings (PINNED, RECENT SEARCHES, CREATE), the
  no-results lines, "Searching…", a hand-written "Ctrl+Space" hint and the
  due picker's "Before"/"After"/"Within 24 hours"/"Which side?".
  `koru-ai-ui`: about twenty-five status-line messages, the clear-history and
  clear-usage confirmations (`lib.rs` 3384–3391), a web-search tooltip and
  "Searching the web…", the whole provider setup view, the permission card's
  detail words, "Copy" in the reply menu (`quote_pills.rs:57`), "Sources: "
  (`sources.rs:47`), the usage report (`usage.rs` 107–193), two service
  messages shown through `failure_text`'s fallback (`reader.rs` 769, 948) and
  the stored "New chat" title sentinel (`lib.rs:232`). `koru-voice-ui` spells
  key and modifier names ("Space", "Ctrl+Shift") itself rather than through
  `koru-keys`. `koru-socials-ui` draws literal network badge letters on raw
  RGB colors (`navigation.rs` 455–457), builds a third-party product name into
  two labels with `format!` (`paint.rs` 208, 1353), and joins translated
  labels with fixed separators in about ten places; `koru-runs-ui` joins two
  translated phrases with a fixed em dash (`words.rs` 45, 113) and shows a
  Rust `Debug` string as a custom agent's name (`views.rs:640`).
- **Test helpers in the public API.** `koru-ai-ui`:
  `microphone_for_host_test`, `microphone_chooser_for_host_test`,
  `dictation_buttons_for_host_test` (`#[doc(hidden)]`, no cfg gate).
  `koru-runs-ui`: `press_label`, `has_control`, `lines`, `control_labels`,
  `RunFolder::unchecked`. `koru-search-ui`: `has_reader` on both types.
  `koru-canvas-ui`: `counters`/`Counters`, which exist for the culling test.
  `koru-socials-ui`: `preview_fixture` and the hidden public `backend`
  (feature-gated). `koru-workspaces-ui`: `WorkspaceFixture` (feature-gated).
- **Missing identities.** No domain surface except the four above registers a
  component identity. COMPONENTS.md says inspector ownership is still open
  for the Socials composition, which opens no scope at all; the canvas,
  session, voice and run surfaces open none either, and no catalog row asks
  them to yet.
- **Docs and source disagree.** COMPONENTS.md's WF05 describes
  `SearchSurface`'s CREATE seam as the workspace command bar's, while its
  SearchModal section records the creation options removed on 2026-11-07;
  the seam (`CreateRow`, `Create`, `set_create_rows`, `take_create`,
  `set_placeholder`) has no caller outside the crate. `EntitySearch::new`'s
  doc says it never dials; in a normal build it does. `koru-runs-ui`'s docs
  say "three views" in four places (`Tab` has four) and "both surfaces" in
  three (there is one). `koru-settings-ui`'s Cargo.toml and `dependency.rs`
  claim three host apps (only `apps/koru` links it) and English literals it
  no longer has; `for_terminal`/`Sections::terminal` serve a Terminal app
  its own module calls deleted. `koru-workspaces-ui`'s controller links the
  deleted `WorkspacesSurface`. The search modal's inspector metadata names
  an `apps/koru` type ("MainView search routing").
- **Smaller.** `SearchSurface::is_global_search` always answers true;
  `EntityOutline`, `EntityHeading`, `EntityDocumentFace` and
  `parse_kind_prefix` are `pub` but unnameable outside the crate though
  public methods return them; misplaced doc comments (`koru-ai-ui`
  `agents.rs` 775–789, `koru-settings-ui` `lib.rs` 2108); `ComposerContextBand`
  is both an element and a part name; `RunsSurface::cursor` and
  `cursor_icon` duplicate each other; `koru-runs-ui`'s `mod dependency` is
  not `cfg(test)`-gated; `koru-socials-ui` reads the global language at paint
  time (no `set_lang`), has an empty `invalidate`, wakes every 100 ms while
  idle, reads the clock while painting, and finds its Channels header row by
  `index == 2`; the AI provider setup view is dormant by its own doc;
  `koru-shell-chrome` names each tab "Tab (legacy shared)" in the inspector.

### Isolation scan

`apps/koru/src/ui_lab/tests.rs` refused any Lab code line naming
`koru_settings`, `koru_workspace::`, `spelling`, `maintenance_deadline` and
the rest of its list, and a record's string literals were part of what it
read. The registry's record data files (`compositions.rs`, `controls.rs`,
`foundations.rs`, `domain.rs` and every `domain/*.rs`) are now read with
their string literals emptied, so a record may spell the path it records
(`"koru_settings_ui::SettingsSurface::set_spelling"`) while a `use`, a path
expression or a call of a forbidden crate in those files is still refused.
Every other Lab file, `registry.rs` included, is scanned whole as before. A
record file may not hold a raw string, a `'"'` character literal or a
literal that runs past its line, so nothing can hide from the reading; a
test pins the exemption to exactly those files. The symbols batch 2a's
drafts had left out only because of the old scan
(`maintenance_deadline` on four surfaces, `SettingsSurface::set_spelling`)
are recorded.

## Batch 2b — editor, document and host crates

Reviewed at `d4ad8af0` (branch `ui-lab/l1b`), 16 November 2026. The records
are in `domain/`, a file per crate, beside batch 2a's; each crate's files
are in `tests/drift/domain.rs`'s `SOURCES` and its `lib.rs` baseline in
`CRATES`, so every check batch 2a describes now covers these crates too.

### Sources reviewed

"Read in full" is every line of a file's non-test code. "Declarations"
is its module documentation, every public type, function, constant and
enum member, its constructors and every inspector scope it opens, with
the bodies between them searched rather than read. "Searched" is a file
reached only through the bounded searches below.

| Crate | Non-test files | Read in full | Declarations | Searched | `lib.rs` modules | Excluded | Records | Symbols | Component identities |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `koru-markdown-view` | 4 | 4 | 0 | 0 | 3 | 0 | 2 | 49 | — |

### Records

IDs marked * are minted here in batch 1's style.

- `koru-markdown-view`: `C-MARKDOWN-VIEW`* (the reader a run's result,
  the Artifacts column's result tab and the instructions view draw with)
  and `C-MARKDOWN-DOCUMENT`* (a boundary: the file read as a Koru document
  and written back as the Markdown a save stores — rich-text content, not
  a control). The AI chat's reply bodies are not this crate's: they are
  `koru-ai-ui`'s own `live_markdown` over `koru-markdown`, recorded on
  `C-AI-CONVERSATION`; batch 2a's note pointing them here was wrong.

### What building each surface needs

- **Pure** — `MarkdownView::new` (no engine, worker or file of its own;
  the host lends its `TextEngine` to lay out and draw);
  `MarkdownDocument::of`/`of_document` (import and export in memory, in a
  workspace nobody owns, with a blob source that holds nothing; the cost is
  the file's, so a host calls it on its own worker).

### Defects found (not fixed)

- `koru-markdown-view`: the not-Markdown state joins its translated labels
  to their values with a fixed `": "` (`view.rs` 364, 367, 373);
  `MarkdownView::lines` is public and its own doc calls it "what a test
  reads a document out of"; the surface opens no inspector scope.

### Reconnaissance

Candidate owners from the reconnaissance, counts at `3bd19e56`:

| Crate | `pub mod` in `lib.rs` | Non-test `.rs` files | Identity sites | Distinct identities |
| --- | --- | --- | --- | --- |
| `koru-editor` (with `src/components`) | 24 | 49 | 4 | 3 |
| `koru-markdown-view` | 1 | 4 | 0 | 0 |
| `koru-session-view` | 0 | 22 | 0 | 0 |
| `koru-app-shell` | 14 | 15 | 0 | 0 |
| `koru-workspace-host` | 31 | 77 | 7 | 7 |
| `koru-webview` | 0 | 5 | 0 | 0 |

Known records owed: `EmojiList`/`Emoji` (`C-CHOICE`, `koru-editor`
`components/emoji_picker.rs`, the emoji body of `C-EMOJI-COLOR-PANEL`); the
rich-text boundary of `koru-markdown-view`'s `MarkdownView` (the artifact
tab's document, the AI chat's reply bodies, the run views' result); the
session views `C-SESSION-SURFACE` hosts (`TerminalView`,
`ManagedSessionView`, `PendingInputView`); the editor seams the AI composer
uses (`Embedded`, `Policy`). `koru-workspace-host` is the one crate whose
paths the isolation scan's `koru_workspace_host` rule meets; its records go
in a record file, where only literals are exempt.

## Batch 3 — app-local controls and compositions (not started)

| Owner | Non-test `.rs` files | Identity sites |
| --- | --- | --- |
| `apps/koru/src/tasks_view` | 15 | 0 |
| `apps/koru/src/mini_app` | 6 | 0 |
| `apps/koru/src/mainview` | 98 | 3 |
| `apps/koru/src/panes` | 28 | 0 |
| `apps/koru/src/manage` | 32 | 0 |
| `apps/koru-document/src` | 16 | 0 |
| `apps/koru/src` (all of it) | 250 | 6 |

Known records owed: the composition catalog's `DocumentHost` (the app's
retained editor through `Leaf`), the fixed catalog's `TaskDocument`,
`TaskListSelector`, `TaskCollection`, `DocumentHost` and `DocumentSearch`
(`mini_app`, `tasks_view`, `leaf`, and the mini-app chrome's search field
and floating results, which read through `H-ENTITY-SEARCH`), the Workspaces
drawing of the Manage tab over `koru-workspaces-ui`'s controller, the
KoruLane painter over `H-LANE-LAYOUT` (WF01, `mainview/draw.rs`), the workspace and profile switcher rows
(element recipes), the local implementations COMPONENTS.md lists for
migration (popout CloseDialog buttons, page-menu and Explore ellipses, the
error badge), and the window and workspace host adapters.

## Batch 4 — native platform adapters (not started)

| Owner | Count |
| --- | --- |
| `apps/ios` Swift files | 32 |
| `apps/ios` `struct … : View` occurrences | 22 |
| `apps/android/app/src/main/java/app/koru/mobile/ui` Kotlin files | 17 |
| Its `@Composable` occurrences | 101 |

Known records owed: iOS `KoruPrimaryButton`, `KoruEmptyState` and `KoruRow`
(`apps/ios/KoruMobile/Theme.swift`), and Android's Compose/Material base and
theme. Their availability goes in the record of the shared component they
stand in for where one exists (`C-BUTTON` already says `NativePartial` on
iOS), and in a `NativeAdapter` record of their own otherwise.

## Notes for later batches

- Add records to the group file they are shown under, with `Kind`,
  `Source::owner` and per-platform `Availability`; add a module baseline to
  `tests/drift.rs` for any crate root a batch covers, and its files to
  `SOURCES` there.
- `apps/koru/src/ui_lab/tests.rs` scans the Lab's own sources for names the
  Lab must never use. Since batch 2a it reads the registry's record data
  files with their string literals emptied (see Batch 2a, *Isolation scan*):
  a new record file goes on its `RECORD_FILES` list as well as on
  `LAB_SOURCES`, and nothing else is exempt.
- A crate's records go in `domain/<crate>.rs` (or the group file of an app
  batch), its non-test files in `tests/drift/domain.rs`'s `SOURCES`, and its
  `lib.rs` baseline in `CRATES` there.
