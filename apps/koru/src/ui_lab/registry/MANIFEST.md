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
| domain crates | 20 |
| domain crate source files | 360 |
| domain crate lib.rs modules | 247 |
| domain crate lib.rs public modules | 121 |
| domain crate modules excluded | 73 |
| app crates baselined | 3 |
| app source files | 308 |
| app files baselined | 21 |
| app modules classified | 245 |
| app modules excluded | 117 |
| records: app crates | 60 |
| records | 293 |
| records: foundations | 59 |
| records: controls | 80 |
| records: application compositions | 154 |
| records: domain crates | 113 |
| kind: token | 25 |
| kind: primitive | 24 |
| kind: widget | 61 |
| kind: support | 71 |
| kind: domain composition | 57 |
| kind: app-local | 55 |
| token values listed | 174 |
| symbols | 2170 |
| symbols with typed recipes | 2 |
| inspector identities | 60 |
| inspector identity registration sites | 63 |
| disposition: story exists | 1 |
| disposition: no story yet | 211 |
| disposition: nonvisual | 62 |
| disposition: platform-only | 0 |
| disposition: boundary | 19 |
| symbol story: available | 1 |
| symbol story: no story yet | 451 |
| symbol story: nonvisual | 1567 |
| symbol story: boundary | 151 |
| A2UI: component | 11 |
| A2UI: host-styled | 1 |
| A2UI: declarable values | 9 |
| A2UI: host only | 31 |
| A2UI: not supported | 2118 |
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
| `koru-editor` | 49 | 1 | 48 | 0 | 24 | 1 | 33 | 412 | `KoruEntityPicker`, `EmojiList`, `Emoji`, `EmojiCategoryBar` |
| `koru-markdown-view` | 4 | 4 | 0 | 0 | 3 | 0 | 2 | 49 | — |
| `koru-session-view` | 22 | 4 | 18 | 0 | 13 | 0 | 7 | 199 | — |
| `koru-app-shell` | 15 | 1 | 14 | 0 | 14 | 0 | 8 | 138 | — |
| `koru-webview` | 5 | 3 | 2 | 0 | 4 | 0 | 1 | 20 | — |
| `koru-window` | 32 | 1 | 31 | 0 | 31 | 10 | 7 | 76 | — |
| `koru-workspace-host` | 77 | 0 | 77 | 0 | 43 | 33 | 9 | 149 | `CodeBoxes`, `PasswordField`, `EmailAddressField`, `EmailPasswordField`, `NameField`, `ProfileIcon`, `ProfileNameField`, `WorkspaceIcon`, `WorkspaceNameField` |
| **7 crates** | **204** | **14** | **190** | **0** | **132** | **44** | **67** | **1,043** | **13** |

Dispositions, by crate: `koru-editor` 18 no story yet, 11 nonvisual, 4
boundary; `koru-markdown-view` 1 and 1 boundary; `koru-session-view` 3, 2
nonvisual, 2 boundary; `koru-app-shell` 3 and 5 nonvisual;
`koru-workspace-host` 2, 5 nonvisual, 2 boundary; `koru-window` 5
nonvisual, 2 boundary; `koru-webview` 1 boundary — 27 no story yet, 28
nonvisual, 12 boundary. No record has a story or a recipe. Kinds: 29
`DomainComposition`, 4 `Widget`, 34 `Support`. A2UI: 7 symbols host-only
(`H-WORKSPACE-ACCESSIBILITY`), the other 1,036 not supported. Modules,
records and identity sites are separate numbers: 132 `lib.rs` modules hold
67 records; 14 component-identity registration sites name 13 identities
(`KoruEntityPicker` is registered from two painters, and one conditional
site in onboarding names two).

### Records

IDs marked * are minted here in batch 1's style.

- `koru-editor` (read in full: `lib.rs`; the rest by declarations — 60,375
  lines, with inline test modules from `editor.rs` 10,293, `body.rs` 2,068,
  `draw.rs` 2,726, `clipboard.rs` 2,580 and `view_state.rs` 1,676 — with
  `editor.rs`'s `Editor`, `prepare` and the head of `Editing` (300–500),
  `components/mod.rs`'s `Outcome`/`Overlay` (1–175), `components/draw.rs`'s
  every painter entry point and inspector scope, `view_state.rs`'s
  `Embedded` and `ViewState::default`, `policy.rs`'s `Policy`,
  `emoji_picker.rs`'s `Geometry`, and the label tables of the command,
  color, priority, attribute and language panels read): 33 records.
  - The editor and its seams: `C-DOCUMENT-EDITOR`* (`Editor`, `Editing`,
    the frame, `ViewState`, `Body`), `H-EDITOR-EMBEDDED`* (`Embedded` and
    `draw::floating_panels`) and `H-EDITOR-POLICY`* (`Policy`, which
    `ViewState::apply_policy` applies) — the two seams the AI composer
    uses, approved in COMPONENTS.md's 2026-09-24 entry.
  - The document it draws, as boundaries, not widgets: `C-DOCUMENT-TEXT`*
    (paragraph rows, marks, pills, highlights, spelling and search ranges),
    `C-DOCUMENT-IMAGE`* (COMPONENTS.md's "unregistered Document image
    boundary", `draw::draw_image_row`, and the pending-image placeholder),
    `C-DOCUMENT-TABLE`* and `C-DOCUMENT-DIAGRAM`*.
  - A row's own controls, as `Widget`s (the tasks view, the Explore rail
    and `koru-document` draw them too): `C-ROW-GUTTER`*,
    `C-ROW-ATTRIBUTE-CHIPS`*, `C-LINK-CARD`*.
  - The panels that float over a row: `C-EDITOR-OVERLAY`* (the shared
    popover frame and its routing), `C-COMMAND-MENU`*, `C-ENTITY-PICKER`*
    (identity `KoruEntityPicker`, registered by `draw_overlay` and by
    `draw_standalone_entity_picker`), `C-DATE-PICKER`*, `C-EMOJI-LIST`*
    (COMPONENTS.md's `EmojiList` / `Emoji`, family `C-CHOICE`, the emoji
    body of `C-EMOJI-COLOR-PANEL`; it also registers `EmojiCategoryBar`,
    on one line, which the identity scan reads since this batch),
    `C-LANGUAGE-PICKER`*, `C-LINK-PROMPT`*, `C-LIST-PICKER`*,
    `C-NAME-PROMPT`*, `C-PRIORITY-PICKER`*, `C-TAG-PICKER`*,
    `C-ACCENT-COLOR-PICKER`* (not `koru-ui`'s color picker row: nine
    accent tokens in a list) and `C-ATTR-MENU`*.
  - Support: `H-EMOJI-CATALOG`*, `H-EDITOR-ACCESSIBILITY`*,
    `H-EDITOR-KEYMAP`*, `H-EDITOR-TRIGGERS`*, `H-EDITOR-CLIPBOARD`*,
    `H-EDITOR-DICTATION`*, `H-PILL-INFO`*, `H-EDITOR-REMINDERS`*,
    `H-EDITOR-TASKS`*, `H-EDITOR-DATES`*.
  - Public names that are ordinary words are module-qualified as batch 2a
    did (`draw::draw_plus`, `chips::draw_row_chips`, `emoji::catalog`,
    `keymap::resolve`, `when::parse_due`, `tasks::task_ref`).
  - Re-exports of other crates are left out, as batch 2a left them:
    `components::{text_field, text_input}` and `draw::{draw_field,
    baseline_centred_y}` (`koru-ui`), `date_picker`'s grid constants
    (`koru_ui::date_grid`), `diagram::Status` (`koru-diagram`),
    `tasks::{ActivitySink, SINK_LIMIT, TaskActivity, remember, run}`
    (`koru-core`), `utils::zone::*` (`koru-time-zone`), `utils::dates`'
    `koru_ui::dates` items and `input::keymap::recover_modified_editing_key`.
- `koru-markdown-view`: `C-MARKDOWN-VIEW`* (the reader a run's result,
  the Artifacts column's result tab and the instructions view draw with)
  and `C-MARKDOWN-DOCUMENT`* (a boundary: the file read as a Koru document
  and written back as the Markdown a save stores — rich-text content, not
  a control). The AI chat's reply bodies are not this crate's: they are
  `koru-ai-ui`'s own `live_markdown` over `koru-markdown`, recorded on
  `C-AI-CONVERSATION`; batch 2a's note pointing them here was wrong.
- `koru-session-view` (read in full: `lib.rs`, `surface.rs`,
  `interaction.rs`, `pending_input/interaction.rs`; the rest by
  declarations, with `render.rs`, `managed/mod.rs` and
  `pending_input/mod.rs` read through their constructors, `Default` copies
  and draw entry points): the three views `C-SESSION-SURFACE` hosts —
  `C-TERMINAL-VIEW`*, `C-MANAGED-SESSION-VIEW`*, `C-PENDING-INPUT-VIEW`* —
  their look `H-TERMINAL-STYLE`*, the channel they share
  `H-SESSION-INTERACTION`*, and two content boundaries: `C-TERMINAL-CONTENT`*
  (the rows a program wrote, as a session's snapshot carries them) and
  `C-MANAGED-TRANSCRIPT`* (the messages, tool steps and approvals a provider
  sent). Every module is private and `lib.rs` re-exports the API, so every
  symbol is written at the crate root.
- `koru-app-shell` (read in full: `lib.rs`; the rest by declarations, with
  `chrome.rs` read through its carriers and the band and pane-header
  painters, and `zoom_label.rs`, `popover.rs`, `host.rs` and
  `banner_state.rs` through their enums and draw entry points): the chrome
  it paints — `C-SHELL-CAPTION`* (the caption band's pane toggle, identity
  chips and Settings slot, and the pane header band), `C-SHELL-POPOVER`*,
  `C-SHELL-ZOOM-LABEL`* (a `Widget`: one small painter) — and the
  machinery and contracts around them: `H-SHELL-PANEL`*, `H-SHELL-TABS`*,
  `H-SHELL-BANNER-STATE`*, `H-SHELL-HOST`* (`ShellState`, the
  `MultiWindowApp` a host drives, with `ShellHost` and the command
  vocabulary) and `H-SHELL-SURFACE`* (what a hosted view implements).
  **Only `TabStrip`, `TabSlides`, `Panel` and `TabElement` have a
  production consumer** (`apps/koru/src/mainview`, `panes/explore.rs`):
  `ShellState`, the caption and pane-header painters, `Popover`,
  `BannerState` and `ZoomLabel` are exercised only by the crate's own tests
  and `apps/koru/src/mainview/shell_conformance.rs`, which is
  `#[cfg(test)]` (ADR-262's seam, kept compiled until `MainView` becomes
  the shell's surface). The app draws its own zoom flash
  (`apps/koru/src/mainview/zoomlabel.rs`). Their availability stays the
  shared desktop default because the source ships in the linked crate; the
  missing consumer is recorded here rather than as a platform status.
- `koru-webview` (read in full: `lib.rs`, `js.rs`, `debug.rs`; `engine.rs`
  and `stub.rs` by declarations): `C-WEB-SURFACE`*, one boundary record.
  What a person sees in it is a page the system's web engine draws
  (WebView2 on Windows, WKWebView on macOS); Koru places, shows, focuses
  and navigates it and draws none of it, so the open call and the type are
  `Boundary` and the rest is support. Its consumer is the app's Support
  surface (`apps/koru/src/support`). `lib.rs` declares `engine` twice — the
  engine, and `stub.rs` under `no-webview` and on Linux — and the baseline
  lists both.
- `koru-window` (read in full: `lib.rs`; the rest by declarations, with
  `accessibility.rs`'s platform split, `frame.rs`'s Windows/other split and
  `input.rs`'s event enums read): the native window host adapters as
  support — `H-WINDOW-RUNNERS`* (`WindowApp`/`run` and
  `MultiWindowApp`/`run_windows`, their window requests and the private
  dispatch core both obey), `H-WINDOW-ACCESSIBILITY`* (the AccessKit
  adapter's lifetime against one window), `H-WINDOW-INPUT`*,
  `H-WINDOW-RENDERER`*, `H-SYSTEM-APPEARANCE`* — and two boundaries the
  operating system owns: `C-WINDOW-FRAME`* (outline, corners, shadow,
  resize border, snapping, the Mac's traffic lights) and
  `C-IME-COMPOSITION`* (preedit text and the candidate window). The
  workspace app's own loop is `koru-workspace-host`'s, below.
  `dispatch/parity.rs` is `#[cfg(test)]` (`dispatch.rs` 1205) and is left
  out of the sources like every test file.
- `koru-workspace-host` (no file read in full; all 77 by declarations, with
  the module documentation of every top-level module, `lib.rs` 1–420 (the
  paste vocabulary, `ImeTarget` and the head of `View`), `accessibility.rs`
  1–120, `onboarding.rs`'s `OnboardingView` fields, its `impl` and its
  identity sites, `account_ui/switcher.rs`, `menus.rs`'s platform split and
  `native_surface.rs`'s contract read; `app.rs`, 21,795 lines, only
  searched): nine records. Its reusable surfaces: `C-ONBOARDING`* (the
  first-run window — welcome, email and code, Profile, workspace and
  recovery — claiming the nine identities its private parts register) and
  `C-ACCOUNT-SWITCHER`*, with `H-ACCOUNT-STATE`* (the account projection
  both onboarding and the Manage tab draw from). Its contracts:
  `H-VIEW-HOST`* — `View`, `Host`, `run`/`run_with_args` and the paste and
  pane vocabulary; the 21,000-line event loop is orchestration and is
  recorded as this contract, not as a component — `H-WORKSPACE-ACCESSIBILITY`*
  (semantic identity and projection; the platform half is
  `H-WINDOW-ACCESSIBILITY`), `H-NATIVE-SURFACE`* and `H-KEYBOARD-ROUTING`*.
  Two OS-owned boundaries: `C-SYSTEM-MENUS`* (the Mac's menu bar and the
  native popup menus, through `muda`) and `C-FILE-DIALOGS`* (`rfd`). 33 of
  its 43 `lib.rs` modules draw nothing and are excluded with a reason in
  `tests/drift/domain.rs`: IO and workers, data books and parsers, and the
  startup, Profile and task orchestration. These records live in
  `domain/workspace_host.rs`, which is on `RECORD_FILES`: the isolation
  scan reads it with its literals emptied, so it may spell
  `koru_workspace_host::run` as data while still naming nothing it could
  call.

### Platforms

The shared desktop default is batch 2a's (Windows existing, macOS and Linux
the same source unverified, Android and iOS not audited). Exceptions, each
from the source or the page named:

- `NativeOwned`, first used here: the record's visible part is the
  operating system's or a provider's. `C-WEB-SURFACE` on Windows and macOS
  (the engine's page), `C-WINDOW-FRAME` on Windows and macOS (DWM's frame
  and shadow; AppKit's titled window and traffic lights), and
  `C-IME-COMPOSITION` on Windows, macOS and Linux (PLATFORMS.md, P1 native
  input: "the native candidate surface remains OS-owned"). The
  `#[expect(dead_code)]` on `Availability::NativeOwned` said it would fail
  once one was recorded; it is removed.
- `C-WEB-SURFACE` on Linux is `Unavailable`: wry is linked only for Windows
  and macOS (`Cargo.toml` 38–45) and `stub.rs` answers every call with
  `RuntimeUnavailable`.
- `H-WINDOW-ACCESSIBILITY` is `Existing` only on Windows; macOS is
  `Planned` (the adapter is Windows-only by ADR-336 and
  `docs/features/macos/CATCHUP.md` owns the change that adds it); Linux is
  `Unavailable` (`accessibility.rs` 315: an inert stand-in that never builds
  a tree, with no plan recorded).
- `C-WINDOW-FRAME` on Linux, and every Android and iOS cell of these
  records, are `NotAudited`: no page says what the frame or the mobile
  equivalents are, and a desktop window frame's absence on a phone is not
  written down anywhere.
- `C-SYSTEM-MENUS` is `NativeOwned` on Windows and macOS (`muda` is linked
  only there, `Cargo.toml` 279 and 289) and `Unavailable` on Linux, where
  `Popup::show` and `MenuBar::install` are stand-ins (`menus.rs` 156,
  `menubar.rs` 325–341). `C-FILE-DIALOGS` is `NativeOwned` on all three
  desktop platforms: `rfd` is an unconditional dependency
  (`Cargo.toml` 138) whose dialog is the system's or the desktop portal's;
  that it builds and behaves on Linux is not verified by anything here.

### A2UI

No record in this batch is a catalog component. The app-local
`DocumentHost` (batch 3) embeds `C-DOCUMENT-EDITOR` through the app's
`Leaf`, so a declaration names the host, never an editor symbol. The A2UI
hosts do name one thing here: the composition host
(`apps/koru/src/mini_app/composition.rs`) and the mini-app chrome that
hosts the fixed catalog (`mini_app/chrome.rs`) publish the controls they
draw through `koru_workspace_host::accessibility` — `ControlPart`,
`Occurrence`, `SemanticNode`, `ViewSlot` and `WindowSlot` in both,
`ObjectKey` and `Revision` in the chrome. Those seven are `HostOnly` on
`H-WORKSPACE-ACCESSIBILITY`, and the new drift test
`the_a2ui_hosts_publish_their_controls_through_the_workspace_accessibility`
holds each to the host's `use` list; the fixed-catalog search test leaves
them to it.

### Private parts

Visible parts with no public symbol, listed under the record that paints
them:

- `C-EDITOR-OVERLAY`: the panel rows of every picker (`components/draw.rs`
  draws each `Overlay` kind's rows, headers, create rows and swatch faces
  itself), the entity picker's scope footer (an inspector scope at
  `components/draw.rs` 1132), and the body's skin-tone strip
  (`draw.rs` `draw_body_tone_strip`).
- `C-DOCUMENT-EDITOR`: the empty-document and title placeholders and the
  status bar halves the frame draws from `draw::Frame`.
- `C-ONBOARDING`: the email journey's code boxes and password field
  (`onboarding/email.rs`, identities `CodeBoxes`, `PasswordField`), the
  email address and password fields, the name, Profile name, Profile icon,
  workspace name and workspace icon controls (`onboarding.rs`, identities
  as listed), the Profile chooser (`onboarding/profile_selection.rs`), the
  recovery screen (`onboarding/recovery.rs`) and the browser sign-in wait
  (`onboarding/browser_signin.rs`) — all private modules, drawn by
  `OnboardingView::draw`.
- `H-VIEW-HOST`: **the F12 inspector panel** (`inspector.rs` and its
  `hierarchy`, `details`, `spacing`, `history`, `placement`, `scrolling` and
  `detached` children, all private) and the inspector's own native window
  (`app/inspector_window.rs`, `NativeInspector`), which the app's loop
  opens and draws; the first-run window's container scope (`app.rs` 4357,
  kind `container`). The panel opens no scope of its own.
- `C-TERMINAL-VIEW`: the shaped row cache and its box-drawing and
  replacement-glyph painting (`row.rs`), the cursor block/beam/underline and
  the selection fill (`render.rs`), text extraction for a selection
  (`selection.rs`).
- `C-MANAGED-SESSION-VIEW`: the approval card with its Approve, Reject and
  remembered-choice buttons and the dynamic-choice form (`managed/render.rs`
  `draw_approval`, `draw_dynamic_approval`), the Cancel turn button, and the
  item rows; its buttons are a local `draw_button` (`managed/render.rs`
  741), the local implementation COMPONENTS.md lists as UI-011.
- `C-PENDING-INPUT-VIEW`: the draft line with its placeholder, the
  attachment chips with their status and remove control, and the attach and
  send/retry squares, drawn by a local `draw_icon_button`
  (`pending_input/render.rs` 375) rather than `KoruIconButton`.

### What building each surface needs

- **Pure** — `koru_editor::Editor::over`/`rooted_at` (rows over an
  in-memory `WorkspaceDoc`; `ViewState::default` makes its own
  `TextEngine`; no store, window or worker — though a story still needs a
  document to edit, and the editor's attachment and clipboard paths read
  debug switches from the environment when they run); every picker's
  constructor (state only; `EntityPicker` never searches, it hands the host
  a request through `take_request`, and creating a tag or a list is the
  host's); the emoji catalog (a static table; the preferred skin tone and
  recents are process state).
- **Changes OS state when driven** — `koru_editor::clipboard::put`,
  `put_plain` and `put_system_text` write the system clipboard; `take` and
  `one_line` read it. `koru_editor::browser` (excluded) spawns the system
  browser.
- **Pure** — `MarkdownView::new` (no engine, worker or file of its own;
  the host lends its `TextEngine` to lay out and draw);
  `MarkdownDocument::of`/`of_document` (import and export in memory, in a
  workspace nobody owns, with a blob source that holds nothing; the cost is
  the file's, so a host calls it on its own worker).
  `TerminalView::new`, `ManagedSessionView::new` and `PendingInputView::new`
  (in-memory models and render caches; each takes a
  `SessionSurfaceInteraction`, and `()` implements it by dropping every
  intent, so a story needs no session, process or IPC; the pending input
  view never opens the clipboard or a file itself, it asks through an
  intent). `TerminalView::new` builds its own in-memory `SessionStats`.
  `koru-app-shell`'s painters, `Popover`, `ZoomLabel`, `Panel`, the tab
  collections and `BannerState` (geometry and in-memory state; every word
  arrives from the host already localized, and the crate has no
  `koru-i18n` dependency by design).
- **Needs a host, starts nothing itself** — `ShellState::new(host)` takes
  the application's `ShellHost`; it holds windows only once
  `koru_window::run_windows` opens them, and the crate cannot name a
  document, profile, workspace or store (its `Cargo.toml` is the fence).
  `koru_window::run`/`run_windows` take over the thread's event loop until
  the last window closes; the accessibility bridge needs a live window.
- **Pure, with the host doing the work** — `OnboardingView::default`
  (its own `TextEngine`, text inputs and emoji and color panel; every
  sign-in, account, Profile and workspace step leaves as an `Action` the
  host performs); `AccountSwitcher::default` (state over the host's
  `AccountState`); the account projection; the semantic identity values;
  the routing enums.
- **Starts the whole app** — `koru_workspace_host::run`/`run_with_args`
  read settings, find a Profile, connect to the Agent and the Store and
  open windows. The Lab is dispatched before it and its isolation scan
  refuses the path outside a record's literal.
- **OS-owned, on the main thread** — `menus::Popup::new` builds a native
  menu and panics off the main thread (`menus.rs` 208); `MenuBar::install`
  seats the Mac's menu bar; the `dialogs::pick_*` functions block on a
  native modal dialog.
- **Starts an engine process** — `WebSurface::open` needs a live parent
  window handle, blocks the UI thread while WebView2 builds its environment
  (260–560 ms measured, `guide/contributing/deps/WRY.md`), and keeps
  cookies and storage in the caller's data directory; under `no-webview` it
  answers `RuntimeUnavailable` and links no engine. A story of it needs the
  feature or a real window; neither is in the Lab today.
- **Starts a thread or touches disk** — `koru_window::renderer::Warmup::start`
  spawns the device warm-up thread; `RendererBackend::load`/`store` read and
  write a machine-local state file.
- **Changes process state when applied** — `koru_window::Appearance::apply`
  and `CaretAppearance::apply` write `koru_draw::theme`'s process globals.

### Defects found (not fixed)

- `koru-editor`, hard-coded English a person reads: the command menu's row
  labels (`Command::label`, 22 of 24 kinds; its own comment calls it a
  tracked gap), its disabled reasons (`disabled`: "Offline", "Can't be a
  task", "Not a task") and its placeholder ("Insert above", "Insert
  below", "Filter blocks"); `color_picker::LABELS`, `priority_picker::LABELS`
  and `chips::priority_label` (four levels, twice); `attr_menu::ROWS`;
  `chips::DUE_NONE` ("No date"); `editor::TASK_WARNING_TOOLTIP` ("Has
  unfinished items"); the name prompt's label and placeholder ("Image
  name", "Architecture diagram", "Document name", "spec.md",
  `components/draw.rs` 1652–1654); `emoji::group_name`'s category names.
  The document text and image boundaries register no identity (COMPONENTS.md
  calls the image one "unregistered" deliberately); no picker but the
  entity and emoji ones registers one.
- `koru-markdown-view`: the not-Markdown state joins its translated labels
  to their values with a fixed `": "` (`view.rs` 364, 367, 373);
  `MarkdownView::lines` is public and its own doc calls it "what a test
  reads a document out of"; the surface opens no inspector scope.
- `koru-session-view`: `ManagedSessionCopy::default` and
  `PendingInputCopy::default` are public English copy (`managed/mod.rs`
  86–141, `pending_input/mod.rs` 53–80), with an English-ordered
  `"{label}: {value}"` usage line and a fixed `" · "` list separator; the
  one host (`koru-agents-ui`) supplies translated copy, but nothing stops a
  host drawing the defaults. No view in the crate opens an inspector scope,
  so three reusable surfaces have no identity. The managed session's
  buttons and the pending input's squares are local painters (above).
- `koru-app-shell`: `TabKey::for_test` is a public `#[doc(hidden)]` test
  constructor (`keys.rs` 73) with no cfg gate; the crate's painters open no
  inspector scope; `ZoomLabel` duplicates the app's own
  `mainview/zoomlabel.rs`, and the shell's caption painters have no
  production caller (above).
- `koru-workspace-host`: `account_ui::switcher::AccountSwitcher` has no
  caller outside its own tests (the Manage tab's "account switcher" is
  its own intent); `C-ACCOUNT-SWITCHER` is recorded from its source with
  that noted. Its rows, title and the first-run screens are drawn by local
  painters over `scene` rather than through `koru-ui` controls in places
  (`account_ui/switcher.rs` 109–180).
- **Docs and source disagree** — `koru-window`'s `lib.rs` says
  `koru-workspace-host`'s `app.rs` is "5,300 lines" and its own `app.rs`
  says "11,450 lines"; the file is 21,795 lines at `d4ad8af0`.
  `onboarding/accessibility.rs` calls `onboarding.rs` "3,400 lines"; it is
  5,120.

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

`koru-window` (32 non-test files, 0 identities) was in scope but not in the
reconnaissance table. The reconnaissance counted 4 editor identity sites
and 7 in the workspace host with the two-line reading; the one-line and
conditional spellings add `EmojiCategoryBar` and `EmailAddressField`.

The records batch 2a said were owed, reconciled: `EmojiList`/`Emoji` is
`C-EMOJI-LIST`; the Markdown boundary is `C-MARKDOWN-DOCUMENT` (the result
tab and the run views; not the AI chat's replies, above); the session
views are `C-TERMINAL-VIEW`, `C-MANAGED-SESSION-VIEW` and
`C-PENDING-INPUT-VIEW`; the composer seams are `H-EDITOR-EMBEDDED` and
`H-EDITOR-POLICY`; the workspace host's records are in a record file.

### Bounded searches

Run from the repository root, over `crates/<crate>/src` for the seven
crates. Candidates for a person to read, never definitions:

```sh
# Non-test files (the SOURCES lists), and test-only modules a name hides.
find crates/<crate>/src -name '*.rs' | grep -vE '(^|/)tests?\.rs$|_tests\.rs$|/tests/'
grep -rn -A1 '#\[cfg(test)\]' crates/<crate>/src | grep -E '\bmod [a-z_]+;'
# Module baselines.
grep -nE '^(pub )?mod |^#\[cfg' crates/<crate>/src/lib.rs
# Public items before each file's test module, and impl methods.
awk '/^#\[cfg\(test\)\]/{exit} {print FILENAME": "NR": "$0}' FILE | grep -E 'pub (const )?(struct|enum|fn|const|static|trait|type|use|mod)'
# Every identity spelling: the two-line one, one line, and a conditional.
grep -rn -B2 '^\s*"component",' crates/<crate>/src
grep -rnE 'scope(_at|_hoisted)?\(\s*"[A-Za-z]+",\s*"component"' crates apps --include=*.rs
# Who links each crate, and who calls a surface outside its crate.
grep -rln '^<crate> =' --include=Cargo.toml apps crates
grep -rn '<crate_ident>::' apps crates --include=*.rs | grep -v 'crates/<crate>/'
# Threads, processes, files and environment reads at construction.
grep -rnE 'thread::|spawn\(|std::fs|koru_env|Command::new' crates/<crate>/src
# User-facing literals in painters and label tables.
grep -rnE '"[A-Z][a-z]+[ a-z]*"' crates/<crate>/src
# Platform splits behind an availability cell.
grep -n 'cfg' crates/<crate>/src/FILE crates/<crate>/Cargo.toml
# The A2UI hosts' use of these crates.
grep -n 'koru_editor\|koru_workspace_host\|koru_window\|koru_markdown' apps/koru/src/mini_app/*.rs
```

### Exclusions

Every module of the seven roots is in its baseline in
`tests/drift/domain.rs`, with a reason for each excluded one; in summary:

| Crate | Excluded modules | Why |
| --- | --- | --- |
| `koru-editor` | `browser` | IO: hands a URL to the system browser as a child process |
| `koru-window` | `activation`, `console`, `dock`, `open_bytes`, `open_uri`, `system_locale`, `system_version` | IO: asks the OS to activate, attach, set an icon, open or report |
| `koru-window` | `desktop_preferences`, `search_geometry` | Data: a read of `desktop-v1.json`; the global-search row's schema |
| `koru-window` | `window_geometry` | IO: machine-local paths and the window-geometry row store, which the Lab must never touch |
| `koru-workspace-host` | `binding_markers`, `clipboard_html`, `clipboard_rtf`, `diagram`, `environment`, `events_host`, `export`, `image_import`, `images`, `attachdebug`, `macos_urls`, `preferences`, `profile_activation`, `reveal`, `searchdebug`, `spelling`, `telemetry_consent`, `voice_devices`, `voice_install` | IO: files, the clipboard, workers, telemetry, diagnostics, downloads and OS hand-offs |
| `koru-workspace-host` | `clipboard_markdown`, `formats`, `profile_names`, `runpill`, `schedulepill`, `settings`, `url_file`, `voice_prefs` | Data: parsers, registries, books and the session file a surface reads |
| `koru-workspace-host` | `cli`, `profile_lifecycle_decision`, `profile_runtime`, `profile_wait`, `startup`, `tasks` | Orchestration: the command line, Profile lifecycle and startup, task scheduling |

Symbols left out of a record on purpose: re-exports of another crate's
items, recorded at their owner — the editor's listed under its record
above; `koru_app_shell::chrome::{banner_geometry, caption_geometry,
region_geometry, tab_geometry, icon_geometry}` (`koru-shell-chrome`),
`koru_app_shell::{WindowKey, ZOOM_MIN, ZOOM_MAX, ZOOM_STEP}` and
`panel::{SIDEBAR_MIN_W, SIDEBAR_MAX_W}` (`koru-window`, `koru-draw`);
`koru_workspace_host::inputdebug` and `accessibility::{Live, LogicalRect}`
(`koru-ui`, `accesskit`, `koru-draw`). Root re-exports of a crate's own
excluded modules go with the module: `koru_workspace_host::parse_markdown`
(`clipboard_markdown`), and `koru_window`'s `window_geometry`, `open_uri`,
`open_bytes`, `activation`, `dock`, `desktop_preferences` and
`search_geometry` items.

### Drift checks added

- The seven crates' 204 non-test files are in `tests/drift/domain.rs`'s
  `SOURCES` and their roots in `CRATES`, so every check batch 2a lists
  covers them; `every_domain_module_is_classified` now counts 20 crates,
  and the counts table's "domain crate" rows are those 20.
- `registered_identities` reads the one-line `scope("Name", "component", …)`
  and the `if`-chosen name as well as the two-line form; no source compiled
  in before this batch used either, so batch 1's and 2a's identity counts
  did not move. A conditional site counts once per name it can register.
- `the_a2ui_hosts_publish_their_controls_through_the_workspace_accessibility`
  holds the seven host-only accessibility types to the hosts' imports.
- What the checks still cannot see: a public item added inside a module
  already classified, a painter that registers no identity, a trait's
  methods (the declaration scan reads `pub fn` only, so `View`'s and
  `Surface`'s methods are not listed as symbols), and anything in `apps/`.

## Batch 3 — app-local controls and compositions

Reviewed at `81f287d6` (branch `ui-lab/l1b`), 16 November 2026: the UI the
app crates under `apps/` draw for themselves. The records are in `app/`, a
file per area (`app.rs` holds what they share and reuses `domain.rs`'s
helpers); the drift checks are `tests/drift/app.rs`. Landed a folder at a
time; each part below says what is recorded and what is still owed.

### Method and how deeply each folder was read

"Read in full", "declarations" and "searched" mean what batch 2b says
they mean. Most of the app was read by delegated readers working to one
brief (every non-test file, a block each: what it draws, the exact names
that draw it with their visibility, every inspector scope, option enums,
English a person reads, platform splits, docs that disagree); their
reports were then checked against the source for every name a record
uses, and the drift checks below hold each recorded name to a
declaration. The `mini_app` folder, `leaf.rs` and the module root of
`tasks_view` were read by the batch's author directly.

### How app symbols are written and checked

App items are mostly `pub(crate)` or private, and the workspace app is a
binary crate, so a record writes an item by its module path with `koru::`
standing for the `crate::` a caller in the app writes
(`koru::mini_app::chrome::Chrome::open_selector`), and `koru_document::` /
`koru_agent::` for the other two. The path names where the item is
declared, not a promise that anything outside the app can reach it. The
checks in `tests/drift/app.rs` resolve the longest module prefix that is a
compiled-in file and read the declaration **whatever its visibility**
(`struct`, `enum`, `fn`, `const`, a `pub use`), and an app enum's members
the same way; the crates' checks still require `pub`. The records stay
string literals in record files on `RECORD_FILES`, so the Lab still names
nothing in the app it could call.

### Drift checks added

- `tests/drift/app.rs` compiles in every non-test file of `apps/koru`
  (without the Lab), `apps/koru-document` and `apps/koru-agent` — 308
  files; the list grew a folder per commit of this batch — so the symbol,
  option and identity checks cover them and **every component identity an
  app painter registers must be claimed**; `all_sources` reads them.
  Test-only modules under ordinary names (`perf.rs`, `tasks_view/perf.rs`,
  `mainview/harness.rs`, `mainview/accessibility/{integration,
  document/native}.rs`, `manage/profiles/progress/fixtures.rs`,
  `panes/explore/popover_baseline.rs`) and the undeclared
  `mainview/shell_conformance.rs` are left out.
- 21 app files' `mod` lists are baselined: `main.rs` and every folder
  root of the workspace app that declares children, and the two other
  crates' `lib.rs`. Deeper modules are covered by their parent's
  classification.
- `every_app_module_is_classified`: each baselined app file's `mod` list,
  whatever its visibility (test modules named as such left out), against
  its classification; every app record reached from one, of kind
  `AppLocal` or `Support`, with an app file as its source.
- `the_fixed_catalog_is_served_by_the_app_host`: the fixed catalog's five
  components each claimed by exactly one app symbol, still required by
  the parser under their IDs, and still built the way the record says
  (the host source lines are named in the test).
- `the_a2ui_components_are_the_composition_catalogs` now expects the
  composition catalog's node kinds and the fixed catalog's components,
  `DocumentHost` once for both; `only_the_catalogs_symbols_claim_a2ui_support`
  lists the app's host symbols beside the crates'.
- New count rows: app crates baselined, app source files, app files
  baselined, app modules classified and excluded, records owned by an app
  crate, and `kind: app-local` (`Kind::AppLocal`'s `#[expect(dead_code)]`
  is removed now that records use it).

Like the crates' checks they do not discover a painter that registers no
identity, or a new item inside a module already classified.

### A2UI: the catalogs and the app surfaces that back them

| Catalog component | Record | Symbol (A2UI field) | What the host does |
| --- | --- | --- | --- |
| Fixed `TaskDocument` (`root`) | `C-MINI-APP`* | `koru::mini_app::MiniApp` (`Component`) | the tab: header with Save and Pin, the two regions, narrow switch |
| Fixed `TaskListSelector` (`selector`) | `C-TASK-LIST-SELECTOR`* | `koru::mini_app::chrome::Chrome::open_selector` | `koru_ui::Dropdown` over a paged `koru_ui::Menu` of buckets and lists |
| Fixed `TaskCollection` (`tasks`) | `C-TASKS-VIEW`* | `koru::tasks_view::TasksView::collection_only` | the production Tasks view without capture or creation |
| Fixed `DocumentSearch` (`search`) | `C-DOCUMENT-SEARCH`* | `koru::mini_app::chrome::Chrome::changed_query` | `koru_ui::TextInput` (`InputStyle::search`) and a floating `Menu`, reading through `H-ENTITY-SEARCH` |
| Fixed and composition `DocumentHost` | `C-DOCUMENT-HOST`* | `koru::mini_app::MiniApp::land` | the landed document becomes the retained `Leaf` (`H-APP-LEAF`), which embeds `C-DOCUMENT-EDITOR` |
| Composition `Row`, `Column`, `Box`, `Text`, `Spacer`, `Divider` | `P-ROW` … `P-DIVIDER` (batch 1) | `koru_draw::el::*` (`Component`) | built by `C-COMPOSITION-HOST`* |
| Composition `Button` | `C-BUTTON` (batch 1) | `koru_ui::button::ActionButton` (`HostStyled`) | Primary when selected, Secondary otherwise, Save and Pin Ghost |

Host-only app symbols, each the adapter's and never a declaration's:
`MiniApp::from_declaration`, `fixture`, `refine`, `draw_surface`,
`draw_composition`, `set_lists`, `Chrome::poll`, `composition::Surface`
and its `layout`, the four token mappings (`spacing`, `text_role`,
`ink_role`, `radius_role`), `TasksView` and `Leaf`. Platforms for the
five catalog records follow PLATFORMS.md: Windows existing, macOS and
Linux the shared source unverified, Android and iOS **unavailable** ("no
generated-host integration", "unavailable for this catalog").

### Part 1 — `mini_app`, `tasks_view`, `leaf.rs`

| Folder | Non-test files | Read in full | Declarations | Searched | Records | Identities |
| --- | --- | --- | --- | --- | --- | --- |
| `apps/koru/src/mini_app` (+ `mini_app.rs`) | 7 | 7 | 0 | 0 | 5 | — |
| `apps/koru/src/leaf.rs` | 1 | 0 | 1 | 0 | 1 | — |
| `apps/koru/src/tasks_view` | 14 (+ `perf.rs`, test-only) | 1 (`mod.rs`) | 13 | 0 | 1 | — |

`leaf.rs` by declarations: every item, the module and type documentation,
`Leaf::document` and the title-row methods (1–270), the reminder and
entity drains (519–578), the `Editing` and `View` impls (750–910).
`tasks_view` by declarations: every top-level item of every file, the
module documentation of `mod.rs`, `TasksView::new`, `collection_only` and
its `impl View` (view.rs 3519), and the inspector scopes of `draw.rs`
(`row::LIST` = "Task list", kind container, line 284; "Drop indicator",
part, line 349), with the delegated reader's report for the bodies.

Records (* minted here, `C-` and the thing's name):

- `C-MINI-APP`* (`MiniApp`) — the tab both catalogs open in; Save and Pin
  labels and requests (`artifact.rs`, `pin.rs`) and the refinement swap
  (`refinement.rs`) are recorded on it; the region enum `Region`
  (Tasks, Document) is its option set, labelled with the app's own words.
- `C-TASK-LIST-SELECTOR`*, `C-DOCUMENT-SEARCH`*, `C-DOCUMENT-HOST`*,
  `C-COMPOSITION-HOST`* — above.
- `C-TASKS-VIEW`* (`TasksView`) — the whole Tasks page: heading
  (`heading::tree`, an element tree), chips and overflow menu, capture row,
  rows as a local element tree (`row.rs`) whose chips are `C-LABEL-CHIP`,
  and pickers that are the view's own state drawn with `C-MENU` and
  `C-DATE-GRID`, borrowing only the editor priority picker's `LABELS` and
  `LEVELS` (`pickers.rs`). It does not draw `C-ROW-GUTTER` or
  `C-ROW-ATTRIBUTE-CHIPS`; batch 2b's note that it does is wrong.
- `H-APP-LEAF`* (`Leaf`) — support: the app's `impl Editing for Leaf` and
  `impl View for Leaf` over `koru_document::BuiltInDocument`, which embeds
  `koru_editor::Editor`; every draw call is delegated, so it paints nothing
  of its own. It passes the editor's reminder, entity-search and delegate
  requests up (`take_reminder_request` …, `H-EDITOR-REMINDERS`); the
  container drains them.

Excluded modules: `mini_app::declaration` (data), `tasks_view::{echo,
rank}` (data), `tasks_view::query` (io: the store query the container
runs) and `tasks_view::perf` (test-only).

Defects found here: the capture row's placeholder is English
(`tasks_view/capture.rs:36`, "Add a task…  /due  /priority  /list"), so is
the empty state's "Checking your workspaces…" (`tasks_view/states.rs:37`);
the artifact worker's failures are English sentences
(`mini_app/artifact.rs` 292, 355, 369) and `MiniApp::refine` refuses with
English `&'static str` reasons (`refinement.rs` 131, 143); the mini app's
header paints its own focus ring with `scene::stroke` rather than through
the controls it outlines (`chrome.rs` 1019); no part of the mini app
registers a component identity — its F12 ancestry is the element frame's
node names ("Task document surface", "Composition"), which are inspector
names, not translated text.

### Part 2 — the app's own pages and tab views

| Folder or file | Non-test files | Read in full | Declarations | Searched | Records | Identities |
| --- | --- | --- | --- | --- | --- | --- |
| `welcome/` | 7 | 7 | 0 | 0 | 1 | `KoruWelcome` |
| `manage.rs`, `manage/` | 33 (+ `profiles/progress/fixtures.rs`, test-only) | 20 | 6 | 7 | 4 | `KoruManage` |
| `manager_view.rs` | 1 | 0 | 1 | 0 | 2 | — |
| `trash/` | 4 | 4 | 0 | 0 | 1 | — |
| `support/` | 6 | 5 | 1 | 0 | 2 | — |
| `whatsnew.rs`, `website.rs` | 2 | 2 | 0 | 0 | 0 | — |
| `canvas_view.rs`, `canvas_view/` | 6 | 6 | 0 | 0 | 2 | `KoruCanvasTextDialog` |
| `feedback.rs`, `feedback/` | 5 | 5 | 0 | 0 | 1 | — |
| `diff/mod.rs`, `session_offer.rs`, `taskbar.rs` | 3 | 2 | 1 | 0 | 3 | — |
| `settings_view.rs`, `settings_view/reminders.rs` | 2 | 2 | 0 | 0 | 1 | — |
| `ai_chat_view.rs`, `search.rs`, `socials_view.rs`, `run_view.rs`, `result_view.rs`, `result_document.rs`, `session_tab.rs` | 7 | 7 | 0 | 0 | 1 | — |

All by the delegated readers; `manage/profiles/` (orchestration) is the
declarations and searched part, `manager_view.rs` was read 1–457, 650–824
and 2505–2753 with the forwarding impls between searched, and the Windows
half of `taskbar.rs` (182–540) was searched.

Records (* minted): `C-APP-WELCOME`* (the Welcome tab; registers
`KoruWelcome`), `C-MANAGE`* (the Manage tab — page shell on
`koru_ui::PageShell`, nav, profile page, sign-in card, recovery lists, the
new-profile page hosting `C-ONBOARDING`, its confirmations; registers
`KoruManage`), `C-MANAGE-WORKSPACES`* (the Workspaces section, **drawn by
the app over `koru-workspaces-ui`'s `WorkspacesController`**, used as data
only), `C-MANAGE-DEVICES`*, `C-MANAGE-CLOUD`*, `C-TRASH-PAGE`*,
`C-FEEDBACK-PAGE`*, `C-VERSION-COMPARE`*, `C-CANVAS-TAB`* (over
`C-CANVAS-SURFACE`; its rename prompt is `C-DIALOG-FRAME`),
`C-CANVAS-TEXT-DIALOG`* (registers `KoruCanvasTextDialog`),
`C-SUPPORT-VIEW`* (hosts `C-WEB-SURFACE`; asks with `C-CHOICE-DIALOG`),
`C-SUPPORT-FAB`* (its right-click is a `C-SYSTEM-MENUS` popup),
`C-SESSION-OFFER`*, `C-REMINDER-SETTINGS`* (the host section drawn inside
`C-SETTINGS-SURFACE`), `C-POPOUT-CLOSE-DIALOG`*, `H-MANAGER-VIEW`* (the
`ManagerView` enum and `DocumentPane`, both `impl View`), `H-APP-TAB-HOSTS`*
(the tabs that wrap `C-AI-CHAT-SURFACE`, `C-SEARCH-SURFACE` and
`C-SEARCH-MODAL`, `C-SETTINGS-SURFACE`, `C-SOCIALS-SURFACE`,
`C-RUNS-SURFACE`, `C-ARTIFACT-TAB` with a `Leaf` for the result document,
and `C-SESSION-SURFACE`) and `C-JUMP-LIST`* (Windows `NativeOwned`; macOS
and Linux `Unavailable`: every call is a stand-in and the module documents
the Dock menu as not implemented).

Excluded with a reason in `tests/drift/app.rs`: `welcome::{build, hints,
mcp}`, `trash::menu`, `support::url` (data), `manage::profiles`,
`canvas_view::perform` (orchestration), `feedback::{draft, logs, outbox,
submit}` (data and io). `whatsnew.rs` and `website.rs` draw nothing.

**`KoruWelcome` is the app's identity, not `koru-ui`'s.** The only scope
that registers it is the app's Welcome tab (`welcome/draw.rs:55`), which
does not draw through `koru_ui::welcome`; that module's scopes are
`WelcomeContent`, `WelcomeSidebar`, `WelcomeCloudNote` and
`WelcomeFailure`, and onboarding draws it. Batch 1's `C-WELCOME` keeps
`KoruWelcome` as its KoruX public name from COMPONENTS.md and claims only
the identities its source registers, so the two agree once this record
claims the app's.

Defects found here (not fixed):

- **Local painters duplicating shared controls**: the pop-out close
  question (`manager_view.rs` 2513–2720, UI-010) draws its own scrim,
  panel, rounded-rect buttons and checkbox, opens no scope and is English
  ("Close this window?", "Restore tab", "Close", "Remember my choice");
  the feedback form draws its own checkbox, radio buttons, kind cards,
  close button and disclosure (`feedback.rs` `draw_check`, `paint_kinds`,
  `paint_kind_fields`, `paint_shot_close`); the canvas text dialog builds
  its own frame while the rename prompt beside it uses `KoruDialogFrame`;
  the support button's badge, the Welcome tick, command field and links,
  and Manage's platform badge, disclosure triangle and focus rings are
  local paint.
- **Hard-coded English a person reads**: the whole feedback form
  (`feedback.rs`, about forty strings; `feedback/submit.rs` 42–45); every
  Welcome row (`welcome/mod.rs` 69–123); Manage's nav, dialogs, profile
  page, sign-in card, workspaces, devices, history and Cloud constants
  (`manage/nav.rs` 54–58, `dialogs.rs` 129–399, `profile_page.rs` 356–600,
  `sign_in.rs` 40–461, `workspaces.rs` 75–503, `devices.rs` 83–1024,
  `devices/history.rs` 27–340, `cloud.rs` 93–938, `profiles/{removal,
  intent, creation, results}.rs`); the Trash title, columns and fallbacks
  (`trash/view.rs` 108–304, `trash/draw.rs` 89, 225); the Jump List's
  titles and categories (`taskbar.rs` 556–836); Settings rows the app
  hands to `koru-settings-ui` (`settings_view.rs` 70–898); the Agent
  link's trouble lines shown through search (`agent.rs` 3163–3294). Fixed
  separators join translated words in `manage/` and `trash/view.rs` 347;
  `manage/accessibility.rs` 674 names the hidden-profiles row in English
  while the painted line is keyed.
- **Docs and source disagree**: `manage/dialogs.rs` 29–30 and
  `manage/cloud.rs` 33, 968–969, 1227 describe a D258 Cloud question that
  `profiles/cloud.rs` 6–11 says is gone; `welcome/mod.rs` 3 says the page
  greets the owner and nothing draws the owner; `support/view.rs:277`
  returns the hand cursor against the rule `welcome/mod.rs:660` states;
  `run_view.rs:16` cites `workspaces.rs` (it is `manage/workspaces.rs`);
  `search.rs:5` calls `koru-search-ui` 3,600 lines (about 11,600);
  `taskbar.rs`'s module doc says two Jump List categories (three) and a
  Recent entry carries `--profile` (it does not); `settings_view.rs` 47–51
  says `VoiceOff` has two variants (three).
- **Missing identities**: no Manage page below `KoruManage`, and no Trash,
  feedback, comparison, support or pop-out surface, registers a component
  identity.

### Part 3 — the container (`mainview`)

96 non-test files (`harness.rs`, `accessibility/integration.rs` and
`accessibility/document/native.rs` are test-only; `shell_conformance.rs`
is declared by no `mod` line and is not compiled). Read in full by the
delegated readers except: `mod.rs` (18,865 lines) by declarations — its
module docs, every `MainView` field, impl header and the ~575 function
signatures, with `View::draw` and the caption, lane, menu and modal
geometry read and the input handlers searched; `reminders.rs`,
`canvas_chain.rs`, `canvas_runs.rs` by declarations; the two test-only
accessibility files searched. 73 modules are baselined in
`tests/drift/app.rs`.

Records (* minted): `H-MAIN-VIEW`* (support: `impl View for MainView`,
the frame order, accessibility delegation, the container's own confirms
and toasts), `C-LANE`* (COMPONENTS.md's **KoruLane**, WF01, drawn over
`H-LANE-LAYOUT` with `C-ICON-BUTTON` and `C-LOGO`), `C-APP-CAPTION`* (the
caption band, the window buttons through `C-CAPTION`, the branch and
offline chips through `C-BRANCH-CHIP` and `C-STATUS-CHIP`, and each pane
group's strip, drawn by `C-TAB-STRIP`), `C-PANE-SPLITS`*,
`C-APP-ZOOM-FLASH`*, `C-ERROR-DIALOG`* (the error badge, UI-003),
`C-CHAT-FILE-PREVIEW`* (`ChatFilePreview`), `C-LINK-DEVICE-PANEL`*
(`QRFloatingPanel`), `C-DELEGATE-FORM`* (`KoruDelegateForm`; finished
phases on `C-PROMPT`, all on `C-MODAL`), `C-RECORDING-SETUP`*,
`C-AGENT-CLI-CARDS`*, `C-FONT-PICKER`*, `C-IMPORT-DIALOG`*, `C-MOVE-TO`*,
`C-NAME-PROMPTS`* (new document and snapshot on `C-DIALOG-FRAME`, rename
and new list in place), `C-REMINDER-EDITOR`* (the bell's popovers on
`C-POPOVER`, and the Reminders tab), `C-UPDATE-PROMPTS`*,
`C-DICTATION-STRIP`*, `C-PROFILE-WAIT`*, `C-APP-MENUS`* (every container
menu is `C-MENU` over `context_menu::MenuAction`, with `C-LAUNCHER` and
`C-PLACE-CHOOSER`).

Identities: `ChatFilePreview`, `QRFloatingPanel` and `KoruDelegateForm`
are component scopes and are claimed. `KoruLane` is opened as a
**container** scope (`draw.rs` 1103, and again by `panes/explore.rs`
7103), so it is not an identity the checks count, though COMPONENTS.md
names it a component. The tabs the strip draws are named "Tab (legacy
shared)" by `koru_shell_chrome::tabs` (`tabs.rs` 523), recorded in batch 2a.

Defects found here (not fixed):

- **Hand-drawn scrims instead of `koru_ui::modal`** (no `KoruModal`
  scope): the Move-to modal (`mod.rs` 12583–12623), the error dialog, the
  file preview, the import dialog, the font picker, the link-device panel
  and both update modals; the recording setup frames itself too.
- **Local painters duplicating shared controls**: the error dialog's
  badge (UI-003), details disclosure, text "buttons" and scrollbar thumb;
  the link-device panel's own tab strip ("QRPanelTabs"); the delegate
  form's text field and choice rows (`delegate/paint.rs` `draw_text_field`,
  `draw_choice`); the recording setup's preset rows and disabled Record
  stand-in; the lane controls' focus ring, the tear-off pill and the zoom
  flash's popover (`draw.rs` 1230–1239). The zoom flash duplicates
  `C-SHELL-ZOOM-LABEL`, which has no production caller.
- **Hard-coded English a person reads**: the Move-to modal and its toasts
  (`mod.rs` 2287–2541, 13707), the bulk-trash and list-delete confirms
  (5845–5847, 9165–9171), toast actions (14194–14393), the feedback
  failure toast (9607–9608), the screenshot dialog (9632–9634), the
  "Toggle context" tooltip (18193), "Current document" (`draw.rs` 486),
  recording sizes (`recording/paint.rs` 92, 330).
- **Docs and source disagree**: `mod.rs`'s module table names `tabset`,
  `panel` and `tabslide`, which no longer exist, and omits most modules;
  the `accessibility` doc says four `impl View` methods and a comment says
  seven (there are eight); `shell_conformance.rs` calls itself
  `#[cfg(test)]` behind "a `#[cfg(test)] mod` line" that does not exist,
  so **batch 2b's note that it is compiled under test is wrong** — it is
  not compiled at all; `draw.rs`'s module doc says the caption paints
  last, and the columns, cards, menus and modals all draw after it.

### Part 4 — the side columns (`panes`)

28 non-test files (`explore/popover_baseline.rs` is test-only). Read in
full by the delegated readers except `explore.rs` (8,517 lines) by
declarations — every item, `ExploreView`'s fields and draw entry points,
every inspector scope; the rest searched. 23 modules baselined across
`panes/mod.rs`, `panes/explore.rs` and `panes/sidebar/mod.rs`.

Records (* minted): `C-EXPLORE`* (the explore column; its scopes are the
containers `KoruRail`, `KoruLane`, `ExploreTabs`, `ExploreNodes`,
`ExploreRow`, `ExploreFooter`; its ellipsis is `C-ICON-BUTTON`),
`C-APPS-RAIL`* (the sidebar's squares, with the mini-app pin group of
PINS.md), `C-TASKS-RAIL`*, `C-HOME-NAV`* (COMPONENTS.md's WF06
**KoruHomeNav**, which is `HomeNav` in the source), `C-HOME-PAGE`* (WF07
**KoruHomePage**, `HomePage`), `C-EXPLORE-POPOVERS`* (the workspace and
profile switcher pages written as elements, the help popover, the library
actions), `C-CONTEXT-COLUMN`*. None registers a component identity.
`fold.rs` is excluded as dead code.

Docs and source disagree (not fixed): COMPONENTS.md WF02's
`panes/sidebar/apps_row.rs` and WF03's `panes/sidebar/floor.rs` no longer
exist (the apps are the rail squares above; a test asserts no
`KoruAppsRow` or `KoruFloor` record), and `panes/sidebar/floor/tests.rs`
is still tracked though no `mod floor` compiles it; the names
`KoruHomeNav` and `KoruHomePage` appear nowhere in code; COMPONENTS.md's
**Explore ellipsis** (UI-012) is stale — `explore/actions.rs` `draw_dots`
already draws `koru_ui::icon_button::draw_with`; the profile switcher row
described in COMPONENTS.md is stale against `popover_elements/profiles.rs`;
`explore/rail.rs` is the Tasks rail, not the apps rail; there is no
`impl View` for `ExploreView`, a chrome pane drawn by inherent methods.

### Part 5 — the app's roots, the document app and the Agent

`apps/koru/src/main.rs` (read in full) and the 25 remaining root files
(`agent.rs`, `app_menu.rs`, `build_stamp.rs`, `chat_diagrams.rs`,
`console.rs`, `format.rs`, `input_engine.rs`, `reminder_settings.rs`,
`reopen_claim.rs`, `schedule_index.rs`, `schedule_words.rs`,
`terminal_prefs.rs`, `tasks.rs`, `whatsnew.rs`, `website.rs` and the
recorded ones above; `agent.rs`, `reminder_settings.rs` and
`schedule_index.rs` searched, `taskbar.rs` by declarations, the rest in
full). `main.rs`'s 40 modules are baselined, so every top-level module of
the workspace app now has a disposition; `H-APP-HOST`* records the app's
`impl Host for Manager` (`main.rs` 133).

`apps/koru-document` (16 files; `lib.rs`, `view.rs`, `search.rs`,
`context_menu.rs`, `docview.rs` by declarations, `spelling.rs` and
`structure.rs` searched, the rest in full): `C-DOCUMENT-WINDOW`*
(`BuiltInDocument`, its window chrome, confirmations on `C-CONFIRM` and
the lifted `C-MEDIA-VIEWER`), `C-FORMAT-BUBBLE`*, `C-DOCUMENT-MENUS`*,
`C-FIND-REPLACE`*. No identity. `apps/koru-agent` (59 files; `app.rs`,
`run.rs`, `service.rs`, `update.rs` searched with their window and dialog
code read, `tray.rs` by declarations, `voice_window.rs` in full, the rest
classified from declarations): `C-AGENT-SEARCH-WINDOW`* (hosts
`C-SEARCH-SURFACE` under a local drag rail), `C-AGENT-VOICE-WINDOWS`*
(hosts `C-VOICE-SURFACE` and `C-VOICE-HUD`), `C-AGENT-TRAY`*
(`NativeOwned` on all three desktops: tray-icon on Windows and macOS,
ksni on Linux). Both crates' `lib.rs` are baselined; the workspace app
does not depend on `koru-agent`.

The other app crates, by their `Cargo.toml` and a search of their source
for any painter, window or menu: `koru-demo` (a UI Automation runner; its
only winit use is `NamedKey`), `koru-voice` (the voice worker),
`koru-ai-server`, `koru-events-server`, `koru-mcp`, `koru-relay` and
`koru-com-notification` draw nothing. `koru-store-server` owns only
operating-system surfaces — a tray icon and menu (`src/tray.rs`, Windows)
and a fatal message box (`src/log.rs`) — and `koru-notification-owner`
only titles macOS notification actions; they are not compiled into the
checks and have no record, a disposition the native audit can revisit.
`apps/macos/KoruShare/ShareViewController.swift` is the macOS share
extension (AppKit), left to batch 4.

Defects found here (not fixed):

- **Hard-coded English a person reads**: `koru-document`'s confirmations
  (`lib.rs` 1955–1998), "Title" and "Untitled", the bubble's tooltips
  (`bubble.rs` 106–108) and `docview::Command::tooltip`'s literal
  `Ctrl+` chords on every platform (149–173), the page, block, window and
  right-click menus (`page_menu.rs`, `blockmenu.rs`, `window_menu.rs`,
  `context_menu.rs`), the find bar (`search.rs` 932–1666); the Agent's
  window titles, search warnings (`app.rs` 5668, 8522–8524) and most of
  its tray menu (`tray.rs`), its Store backup and update dialogs
  (`service.rs`, `update.rs`); `koru-store-server`'s whole tray menu and
  dialogs (the crate has no `koru-i18n`); the share extension's "Saving to
  Koru…".
- **Local painters beside shared ones**: `koru-document`'s `menu::Menu`
  is a second floating-menu implementation (with its own toggle and
  swatches) beside `koru_ui::menu`; the bubble's buttons and tooltip are
  local, not `C-ICON-BUTTON` / `C-TOOLTIP`; the find bar's option
  checkboxes are local; `page_menu::draw_button` (COMPONENTS.md's
  page-menu ellipsis, UI-012) **has no caller** — the live title `…` is
  drawn by `koru-editor` (`draw.rs` 373–395), neither through
  `KoruIconButton`.
- **Docs and source disagree**: `koru-document`'s `lib.rs` 84–86 and
  `caption.rs` describe a pin button and a toolbar under the caption that
  are gone; `menu.rs` says four menus (nine use it); the Agent's
  `tray.rs` 2148–2149 says every menu string is English (several are
  keyed); `koru-store-server`'s `tray.rs` 401 has `set_start_on_login`'s
  doc on `backup_store`.

### What batches 2a and 2b left for this batch

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

From batch 2b:

- The window and workspace host adapters are recorded at their crates
  (`H-WINDOW-RUNNERS`, `H-VIEW-HOST`, `H-SHELL-HOST`/`H-SHELL-SURFACE`);
  what is owed in the app is its *implementations* of them — `MainView`'s
  `View`, the app's `Host`, the `cfg(test)` `shell_conformance.rs` adapter
  — and the app's own drawings over them.
- `DocumentHost` and the `Leaf` behind it embed `C-DOCUMENT-EDITOR`; the
  mini-app chrome and the composition host use `H-WORKSPACE-ACCESSIBILITY`'s
  seven host-only types (already held by a drift test) and
  `H-ENTITY-SEARCH`'s.
- The app draws its own zoom flash (`mainview/zoomlabel.rs`) beside
  `C-SHELL-ZOOM-LABEL`; the support FAB (`support/fab.rs`) opens a
  `C-SYSTEM-MENUS` popup and hosts `C-WEB-SURFACE` (`support/`); the tasks
  view, Explore rail and `koru-document` draw `C-ROW-ATTRIBUTE-CHIPS`,
  `C-ROW-GUTTER` and the editor's pickers (`tasks_view/pickers.rs`).
- `apps/koru/src/leaf.rs` drains `H-EDITOR-REMINDERS`' requests; the
  reminder editor the app draws over them is app-local.
- `apps/koru/src/leaf.rs` is the `Editing` implementation (`impl Editing
  for Leaf`) the app's documents use; `koru-document` embeds an `Editor`,
  as the editor's crate documentation says.

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

From batch 2b: every Android and iOS cell of these seven crates' records is
`NotAudited`. The ones a native audit can settle first: `C-WEB-SURFACE`
(does a mobile app host its own web view?), `C-WINDOW-FRAME` and
`C-SYSTEM-MENUS` (likely `NotApplicable` or `NativeOwned`, but no page
says so), `C-IME-COMPOSITION` (PLATFORMS.md says the P1 adapter is not
implemented on mobile and equivalence is not audited),
`C-FILE-DIALOGS` (the platform pickers), and the editor and session
views' mobile equivalents.

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
