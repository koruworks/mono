# UI Lab inventory — source manifest

What the registry's records were written from, how the sources were searched,
what was left out and why, and how many of each thing there are. The records
themselves are the Rust in this folder (`foundations.rs`, `controls.rs`,
`compositions.rs`, a file per domain crate in `domain/`, a file per app area
in `app/`, a file per native platform in `native/`, and the Button in
`../registry.rs`); this page is the audit trail beside them, not a second
catalog.

**Hand-reviewed.** Every record below was written by reading the module it
names: its module documentation, its public types, functions and constants,
its enums' members and the inspector names its painters register. The search
commands found the candidates; they did not decide what a candidate is. The
drift checks in `tests/drift.rs` hold the records to the source afterwards,
within the limits stated under [Drift checks](#drift-checks).

## What the gallery shows, and what it runs

Added with the gallery (ROADMAP.final.md's L3b, 16 November 2026). The
inventory below is unchanged by it; this section says how the Lab reads it.

**The gallery is desktop only.** `registry::desktop()` is the one function
the navigator, the search, the counts beside a group and the story lookup
read the catalog through. It keeps a record when any of its Windows, macOS
or Linux cells says the record has a role there, so the 34 records written
from the Android and iOS apps are in the registry and in none of those, and
no page shows a phone's cell. The macOS share extension and the desktop's
system surfaces are native and are listed.

**Twenty-one records have a story**: the nine the gallery opened with, and
the shared widgets plan L4b has reached (see *L4b batch 1* below). A record
with a story names the symbol the
story drives as a `Recipe`: its typed styles, the inputs the properties
column offers, and the option sets the symbol takes. Its Windows cell is
`Available`, which the registry's tests hold equal to having a story.

| Story | Record | What runs |
| --- | --- | --- |
| `C-BUTTON/dialog` | Button | `koru_ui::button::draw_button`: label, style, press count |
| `C-TEXT-INPUT/field` | Text input | `koru_ui::text_input::TextInput`: typed in; placeholder; the `form`, `search` and `inline` boxes |
| `P-ROW/spacing` | Row | `koru_draw::el::row`: gap and padding from `Space::ALL` |
| `P-COLUMN/spacing` | Column | `koru_draw::el::col`: the same |
| `T-SPACE/scale` | Spacing | `Space::ALL`: each step, its value, a bar that long |
| `T-SURFACE/roles` | Surfaces | `Surface::ALL`: each role painted on the ground and on a raised surface |
| `T-INK/roles` | Ink | `Ink::ALL`: each role set as text |
| `T-LINE/roles` | Lines | `Line::ALL`: each hairline drawn |
| `T-TEXT-ROLES/roles` | Text roles | `roles::ALL`: a sample set in each role, with its size and line height |
| `C-TOGGLE/switch` | Toggle | `koru_ui::toggle::Toggle`: flipped; disabled, pending, leading glyph |
| `C-CHIP/choice` | Chip | `koru_ui::chip::Chip`: three, one chosen; the first one's label; the leading slot |
| `C-SEGMENTED/choice` | Segmented control | `koru_ui::segmented::Segmented`: three segments, one chosen; the arrows choose |
| `C-DROPDOWN/field` | Dropdown | `koru_ui::dropdown::Dropdown`: the field and the list it opens; disabled |
| `C-SWATCH/choice` | Swatch | `koru_ui::swatch::Swatch`: a row of four, the first hollow; one chosen |
| `C-COLOR-PICKER-ROW/row` | Color picker row | `koru_ui::color_picker_row::ColorPickerRow`: its label, its swatches, one chosen |
| `C-STEPPER/scale` | Stepper | `koru_ui::stepper::Stepper`: a value from 1 to 5, each button dimmed at its end |
| `C-PREVIEW-TILE/choice` | Preview tile | `koru_ui::preview_tile::PreviewTile`: two theme tiles, one chosen |
| `C-ICON-BUTTON/button` | Icon button | `koru_ui::icon_button::draw_focused`: tone, metrics, selected, disabled; press count |
| `C-BUTTON-GROUP/cluster` | Button group | `koru_ui::button_group::ButtonGroup`: the caption's three, the button pressed last; disabled |
| `C-SPLIT-BUTTON/split` | Split button | `koru_ui::split_button::SplitButton`: tone, disabled action; each half's presses |
| `C-COPY/confirm` | Copy affordance | `koru_ui::copy_affordance::CopyAffordance`: the icon confirms; what it would have copied |

The text input's story drives three of `InputStyle`'s four constructors;
`bare` draws no box and waits for a story of its own, as do `invalid` and the
`Reveal` end icon. `theme::Accent`, `Radius`, `IconSize`, the metrics and the
aliases have no story yet: the element layer has no setter that paints an
accent, and the others are L4b's.

## State of the inventory

Closed at the end of batch 4 (16 November 2026, branch `ui-lab/l1b`): the
M1 inventory of ROADMAP.final.md's L1b, the union of the four batches
reconciled.

**What is complete.** Every source owner the inventory's scope names has
been read and every candidate it turned up has a disposition — a record, a
place inside another record (the *private parts* lists), or an exclusion
with its reason: `koru-ui` and `koru-draw` (batch 1); the thirteen domain
UI crates (2a); the editor, document, session, shell, webview, window and
workspace-host crates (2b); the workspace app, the document app and the
Agent (3); the Android app, the iOS app and its share extension, the macOS
share extension, the Store's and the notification owner's system surfaces
and the notification boundary crate (4). No list in this page is left "to
do" or "for the next batch"; where an earlier batch said a later one owed
something, the later section says what became of it. Every record has a
localized name and one-line purpose in all eight languages. Every platform
cell has an answer except three phone cells and one desktop cell, each
left `NotAudited` with its reason: `C-TASKS-VIEW` on Android and iOS,
`C-BUTTON` on Android (see *Reconciliation*), and `C-WINDOW-FRAME` on Linux
(batch 2b: no page says what the frame is there).

**What is machine-checked**, by the tests beside the records, all in memory:
the module lists of `koru-ui`, `koru-draw` and its theme, 21 crate roots
and 24 app files, each module a record or a reason; the token tables, read
as values; every option set against its Rust, Kotlin or Swift declaration;
every symbol against a declaration where its path says; every component
identity a compiled-in painter registers, claimed exactly once; the A2UI
catalogs, the host's mappings and the hosts' imports; every composable and
view a compiled-in native file declares, recorded; the doors into the
native UI (the Android manifest's components, its routes and Gradle
modules, the Xcode targets, the iOS routes, what the macOS packager
compiles); the two tables below; the isolation scan of the Lab's own
sources; every displayed string present in all eight languages, names
distinct and no identifier inside translated text.

**What is hand-reviewed**, and only as good as the reading: which symbols a
record holds and what kind and family it is; its dispositions and
construction notes; every platform cell and its reason; A2UI statuses
beyond the catalogs' own components; the mobile-equivalent mappings; the
exclusion reasons; the defects. And what no test can see: a public item
added inside a module already classified, a painter that registers no
identity, a trait's methods, a new native file (Gradle and Xcode list none
at compile time) behind an existing door.

**Known reading-depth limits.** Most files were read in full; these were
read at declaration level (types, functions, constructors, draw entry
points, inspector scopes) with the bodies searched, or only searched:

- `koru-ai-ui/src/lib.rs` (16,578 lines) and `koru-search-ui/src/lib.rs`
  (7,326): declarations; `koru-socials-ui`'s `paint.rs`: in part.
- `koru-editor`: 48 of 49 files by declarations (60,375 lines, `editor.rs`
  10,293 before its tests); `koru-session-view` 18 of 22, `koru-app-shell`
  14 of 15, `koru-window` 31 of 32, `koru-webview` 2 of 5.
- `koru-workspace-host`: all 77 files by declarations; `app.rs` (21,795)
  only searched.
- `apps/koru`: `mainview/mod.rs` (18,865) and `panes/explore.rs` (8,517) by
  declarations; `manager_view.rs` in parts; `manage/profiles/`, the Windows
  half of `taskbar.rs`, `agent.rs`, `reminder_settings.rs` and
  `schedule_index.rs` searched. Most of the app was read by delegated
  readers whose reports were checked name by name (batch 3).
- `apps/koru-document`: five files by declarations, two searched;
  `apps/koru-agent`: `app.rs`, `run.rs`, `service.rs`, `update.rs` searched
  with their window and dialog code read, `tray.rs` by declarations, the
  rest classified from declarations.
- Batch 4: the 44 Android and 10 iOS files that draw nothing, searched (and
  read where a record depended on them); `AppUpdates.kt` to line 150 of
  212; the Store's 30 worker files by module documentation and search;
  four of `koru-native-notifications`' five files by declarations; the
  mobile host and its shims searched.

### Defects found, all batches (not fixed)

One list, deduplicated; each batch's section below has the lines.

- **Hard-coded English or fixed formats a person reads**:
  `koru-appearance-ui` and `koru-search-ui` (neither links `koru-i18n`);
  `koru-ai-ui`'s status lines, confirmations, provider setup, usage report
  and service messages; `koru-voice-ui`'s key names; `koru-socials-ui`'s
  badge letters, `format!`-built labels and separators; `koru-runs-ui`'s
  joined phrases and a `Debug` name; `koru-editor`'s command menu, color,
  priority and attribute tables, name prompt and emoji groups;
  `koru-markdown-view`'s `": "` joins; `koru-session-view`'s public English
  default copy; the workspace app's Tasks capture and empty state, mini-app
  failures and refusals, feedback form, Welcome rows, Manage pages, Trash,
  Jump List, Settings rows, Agent trouble lines and the container's modals,
  toasts, tooltip and quick switch; `koru-document`'s confirmations, menus, bubble and
  find bar; the Agent's windows, tray and dialogs; the Store's tray menu,
  backup dialogs and failure box; `koru-native-notifications`'
  `ActionLabels::default()`, against its own doc; the macOS share
  extension's notice; iOS's device history, details sheet and developer
  screen; Android's event-kind fallback, exception text in a sentence,
  developer screen and ASCII list ordinals; fixed `" · "`, `" — "`, `", "`
  and `": "` separators on both phones and across the desktop.
- **Local painters beside shared controls**: `koru-session-view`'s managed
  session buttons (UI-011) and pending-input squares; the workspace host's
  account switcher and first-run rows over `scene`; the pop-out close
  dialog (UI-010); the feedback form's checkboxes, radios and cards; the
  canvas text dialog's own frame; the error badge (UI-003), its disclosure
  and scrollbar; hand-drawn scrims in place of `KoruModal` (Move to, error,
  file preview, import, font picker, link device, both update modals,
  recording setup); the link-device panel's own tab strip; the delegate
  form's field and choices; the lane's focus ring, tear-off pill and zoom
  popover; two zoom labels (the app's and `C-SHELL-ZOOM-LABEL`, which has no
  production caller); `koru-document`'s second floating-menu
  implementation, its bubble buttons and tooltip and find-bar checkboxes;
  the mini-app header's own focus ring; Android's raw highlight and code
  backgrounds and iOS's raw green and red beside each phone's theme.
- **Missing inspector identities**: no identity for the Socials, canvas,
  session, voice and run surfaces, the document text and image boundaries,
  every editor picker but the entity and emoji ones, `koru-markdown-view`,
  `koru-session-view`'s three views, `koru-app-shell`'s painters, the mini
  app, Manage's pages below `KoruManage`, Trash, feedback, comparison,
  support and pop-out surfaces, and the side columns; `KoruLane` opens a
  *container* scope though COMPONENTS.md calls it a component; the shell
  chrome names each tab "Tab (legacy shared)". The phones have no Koru
  inspector by design (COMPONENTS.md).
- **Test helpers in public API**: `koru-ai-ui`'s three `*_for_host_test`;
  `koru-runs-ui`'s `press_label`, `has_control`, `lines`,
  `control_labels`, `RunFolder::unchecked` and an ungated `mod dependency`;
  `koru-search-ui`'s `has_reader`; `koru-canvas-ui`'s `counters`;
  `koru-socials-ui`'s `preview_fixture` and hidden `backend`;
  `koru-workspaces-ui`'s `WorkspaceFixture`; `MarkdownView::lines`;
  `koru-app-shell`'s `TabKey::for_test`.
- **Docs and source disagree**: COMPONENTS.md's WF05 CREATE seam, WF02
  `apps_row.rs`, WF03 `floor.rs`, the Explore ellipsis (UI-012) and the
  profile switcher row are stale; `EntitySearch::new` says it never dials;
  `koru-runs-ui`'s "three views" and "both surfaces";
  `koru-settings-ui`'s three host apps and its Terminal sections;
  `koru-workspaces-ui` links a deleted surface; line counts quoted in
  `koru-window`, `onboarding`, `search.rs` and `app.rs` are stale; the
  Manage tab's D258 Cloud question, Welcome's owner greeting, the support
  view's cursor, `taskbar.rs`'s categories, `settings_view.rs`'s
  `VoiceOff`; `mainview/mod.rs`'s module table; `shell_conformance.rs`'s
  claim to be compiled under test; `koru-document`'s pin button and
  toolbar; the Agent's "every tray string is English"; and, from the
  native audit, mobile CATCHUP's *Auto-update — never* against the Android
  nightly's update notice, its "Android draws its own reminder surfaces"
  against its *Reminders* row, its "both phones restore" a trashed
  document (neither phone's UI lists or restores one), its "no phone UI
  offers" creation and the README's "iOS editor foundation remains
  disabled" against iOS's creation and editing controls, and the README's
  "equivalent Koru semantic tokens" against the phones' different accents.
- **The keyboard's mark on shared controls** (found by the gallery; see
  *L4b batch 1* below for what was fixed): `ActionButton::draw_focused`
  rings in the accent at a border's weight, two pixels outside the button;
  the dialog button rings in `focus_ring` at twice that weight, also
  outside; `Chip`, `Toggle`, `Segmented`, `Dropdown` and the icon button
  stroke their own edge in `focus_ring` at twice a border's weight. Three
  spellings of one idea. The shared search box (`InputStyle::search`)
  draws no focus state at all, so the Lab still rings its search field
  itself. `Stepper`, `PreviewTile`, `CopyAffordance::draw_icon` and
  `IconButton::draw` (so `ButtonGroup`) draw none either: in their stories
  the Lab's own ring stands in, and says so.
- **English in a shared painter** (L4b batch 1): `CopyAffordance`'s
  tooltip is the literal `"Copy"` and its `TOAST` the literal
  `"Copied to Clipboard"`; `button::draw_help_button` draws the literal
  `"Help"`.
- **Controls with nothing behind them**: every create and edit control in
  the iOS app (the store services refuse each one) and its Add image,
  which inserts a fixed 1×1 picture; `SearchSurface::is_global_search`
  always true; `AccountSwitcher`, the shell's caption painters and
  `page_menu::draw_button` with no production caller; `panes/fold.rs` dead.

### Counts

Computed from the registration and the compiled-in source, for the whole
inventory; the drift checks fail until these tables say what they compute.
A module is not a component and an identity is not an occurrence: see each
batch's counts.

<!-- counts:begin -->
| Count | Value |
| --- | --- |
| koru-ui public modules | 62 |
| koru-ui private modules | 1 |
| koru-draw public modules | 21 |
| koru-draw theme public submodules | 5 |
| domain crates | 21 |
| domain crate source files | 373 |
| domain crate lib.rs modules | 275 |
| domain crate lib.rs public modules | 126 |
| domain crate modules excluded | 93 |
| app crates baselined | 5 |
| app source files | 370 |
| app files baselined | 24 |
| app modules classified | 294 |
| app modules excluded | 158 |
| records: app crates | 65 |
| native files: Android | 24 |
| native files: iOS | 10 |
| native files: macOS | 1 |
| native composables | 89 |
| native views | 30 |
| native entry points baselined | 30 |
| records: native platforms | 35 |
| records | 335 |
| records: foundations | 61 |
| records: controls | 89 |
| records: application compositions | 185 |
| records: domain crates | 114 |
| kind: token | 25 |
| kind: primitive | 24 |
| kind: widget | 61 |
| kind: support | 73 |
| kind: domain composition | 57 |
| kind: app-local | 60 |
| kind: native adapter | 35 |
| token values listed | 177 |
| symbols | 2400 |
| symbols with typed recipes | 22 |
| inspector identities | 57 |
| inspector identity registration sites | 61 |
| disposition: story exists | 21 |
| disposition: no story yet | 193 |
| disposition: nonvisual | 63 |
| disposition: platform-only | 33 |
| disposition: boundary | 25 |
| symbol story: available | 21 |
| symbol story: no story yet | 433 |
| symbol story: nonvisual | 1684 |
| symbol story: platform-only | 98 |
| symbol story: boundary | 164 |
| A2UI: component | 11 |
| A2UI: host-styled | 1 |
| A2UI: declarable values | 9 |
| A2UI: host only | 31 |
| A2UI: not supported | 2348 |
<!-- counts:end -->

Platform cells, by platform and availability: each record says one thing
about each platform.

<!-- cells:begin -->
| Platform | available | existing | shared source | native partial | planned | unavailable | not applicable | native-owned | not audited |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Windows | 21 | 268 | 0 | 0 | 0 | 0 | 36 | 10 | 0 |
| macOS | 0 | 1 | 288 | 0 | 1 | 3 | 34 | 8 | 0 |
| Linux | 0 | 0 | 288 | 0 | 0 | 7 | 36 | 3 | 1 |
| Android | 0 | 18 | 0 | 73 | 13 | 198 | 28 | 3 | 2 |
| iOS | 0 | 14 | 0 | 55 | 13 | 213 | 33 | 6 | 1 |
<!-- cells:end -->

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

The whole inventory's counts, this batch's rows among them, are at the top
of this page ([Counts](#counts)).

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
  (COMPONENTS.md says so), and its drawing is the app's Manage tab, recorded
  by batch 3 as `C-MANAGE-WORKSPACES`.

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
the add-to-chat pill (`quote_pills.rs`). The reply bodies are this crate's
own `live_markdown` over `koru-markdown`, recorded on `C-AI-CONVERSATION`
(this batch first pointed them at `koru-markdown-view`; batch 2b corrected
it).

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
surface, recorded with that mapping by batch 3 as `C-DOCUMENT-SEARCH` — and
it reads through
`koru_search_ui::EntitySearch`. `H-ENTITY-SEARCH` marks `EntitySearch`,
`new_at`, `request`, `poll`, `pending` and `EntitySearchUpdate` host-only,
and `tests/drift.rs` holds each to the line of the chrome that uses it.
`TaskDocument`, `TaskListSelector`, `TaskCollection` and `DocumentHost` are
backed by `apps/koru` (`mini_app`, `tasks_view`, `leaf`), recorded by batch 3.
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
source default. Batch 4 settled every Android and iOS cell left not audited
here against the native sources, and moved the Settings surface to native
partial (see *Reconciliation*).

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

The table at the top of this page is the whole registration's and
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
  - A row's own controls, as `Widget`s (the Explore rail and
    `koru-document` draw them too; this batch also named the Tasks view,
    which batch 3 found does not): `C-ROW-GUTTER`*,
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
  and `apps/koru/src/mainview/shell_conformance.rs` (ADR-262's seam until
  `MainView` becomes the shell's surface), which no `mod` line declares,
  so it is not compiled at all — batch 3 found this; the file's own comment,
  and this batch first, said it was compiled under test. The app draws its
  own zoom flash
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
- `C-WINDOW-FRAME` on Linux is `NotAudited`: no page says what the frame
  is there. This batch left every Android and iOS cell `NotAudited` too;
  batch 4 settled them from the native sources (see *Reconciliation*:
  the window frame is unavailable on both phones, pending PLATFORMS.md's
  owed applicability decision).
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
time; each part below says what it recorded. Nothing it found is left
owed: the two OS-surface crates and the macOS extension it handed on were
recorded by batch 4.

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
only titles macOS notification actions; this batch left them unrecorded
for the native audit, and batch 4 recorded both (`C-STORE-TRAY`,
`C-STORE-FAILURE-DIALOG`, `C-MAC-NOTIFICATION-OWNER`, with their files now
in the checks). `apps/macos/KoruShare/ShareViewController.swift`, the
macOS share extension (AppKit), is batch 4's `C-MACOS-SHARE-EXTENSION`.

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

All of it is recorded above, as each part says; the list is kept as the
hand-over it was. Two of its claims were corrected on the way: the Tasks
view does not draw `C-ROW-GUTTER` or `C-ROW-ATTRIBUTE-CHIPS` (Part 1), and
`shell_conformance.rs` is not compiled at all, under test or otherwise
(Part 3); the app's `MainView` and `Host` implementations are `H-MAIN-VIEW`
and `H-APP-HOST`.

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

## Batch 4 — native platform adapters

Reviewed at `f44f0a99` (branch `ui-lab/l1b`), 16 November 2026, by the
batch's author directly, with no delegated reader. The records are in
`native/`, a file per platform (`native.rs` holds what they share); the
checks are `tests/drift/native.rs`.

### How native symbols are written and checked

A native record's module is the platform's own name for the code — the
Android Gradle namespace `app.koru.mobile`, or an Xcode target / `swiftc`
module qualified by its app folder (`ios/KoruMobile`, `ios/KoruShare`,
`macos/KoruShare`; the iOS and macOS share extensions are both the module
`KoruShare`) — and `::` separates it from the declaration:
`app.koru.mobile::ui.editor.DocumentViewer` is the Kotlin package
`ui.editor` below the namespace, `ios/KoruMobile::DevicesScreen` a type in
the target. Every visible symbol is `Story::PlatformOnly` (the Lab is a
Windows window and cannot draw a Compose screen or a SwiftUI view), what
the system draws for the app is `Story::Boundary`, and the code beside a
view is nonvisual. No native symbol is in an A2UI catalog. Each native
record is `Existing` on its own platform (`NativeOwned` for the system
surfaces) and `NotApplicable` everywhere else: the other platforms have
their own records for the same role, or none, and this code has no role
there. The `#[expect(dead_code)]` on `Kind::NativeAdapter` and
`Story::PlatformOnly` said they would fail once used; both are removed.

`tests/drift/native.rs` compiles in every native file a record is written
from and reads it in its own syntax:

- every native symbol against a Kotlin `fun`, `class`, `object` or
  `interface` in the files of its package (each file's `package` line must
  agree), or a Swift `struct`, `class`, `enum`, `func` or top-level `let`
  in its target's folder;
- every native option set against the members of its Kotlin `enum class`
  or Swift `enum`;
- every `@Composable` function and every Swift type conforming to `View`,
  `App`, `ViewModifier`, a `…Representable` or a view controller in a
  compiled-in file against a record that names it, so a new composable or
  view in a classified file fails until it is recorded;
- every native record a native adapter on a native file, with no
  inspector identity, no A2UI claim and exactly one platform it ships on;
- the doors a new native surface has to come through, against baselines:
  the Android manifest's components (activities, aliases, services,
  receivers, providers — so a new activity, share target, widget receiver
  or notification service fails), its navigation routes (each also opened
  by the navigation host) and its Gradle modules; the iOS project's
  targets (a widget or notification extension would be one), the root's
  routes; and every Swift path the macOS packager compiles.

**What it cannot see.** A native file that is renamed or deleted stops the
suite compiling; a file that is *added* is not seen, because Gradle lists
no sources, Xcode's synchronized folders list no files and the suite reads
nothing at run time. A new screen in a new file behind an existing route is
therefore hand review, as a new painter in an app is.

### Android (`apps/android`)

68 non-test Kotlin files under `app/src/main/java/app/koru/mobile`. 24 are
compiled into the checks and were **read in full**: all 17 under `ui/`
(`AccountScreen`, `BlobTransferPanel`, `CloudInventoryScreen`,
`CloudProgressScreen`, `CloudStorageSection`, `EntityTileColors`,
`KoruApp` — 2,203 lines —, `OpenSourceLicensesScreen`, `QrScanButton`,
`SettingsScreen`, `editor/{CloudCopyBody, CloudCopyScreen, DocumentEditor,
NativeRichEditor}`, `run/{RunPresentation, RunScreen}`, `theme/Theme`),
the three activities (`MainActivity`, `ShareActivity`,
`BrowserSignInReturnActivity`), and the four services that hand a person to
a system surface (`AndroidGoogleAccountDialog`, `AndroidQrScanner`,
`BrowserSignIn`, and `AppUpdates`, read to line 150 of 212 with its restore
and parsing helpers searched). The other 44 were **searched** for any
Compose, `android.widget`, dialog, notification or widget use (none
outside the files above; `BlobTransfer.kt` names Compose only in a comment)
and draw nothing: `AppContainer` and `AppViewModel` (the service container
and the screens' state), `domain/` (4, the model), `logging/AppLogger`,
the other 33 `services/` (account, vault, blob, Cloud presentation,
contract, store and run adapters, mocks), and `sharing/` (4, the share
intent reader, staging, recent destinations and text normalization). The
47 test files (`app/src/test`, `app/src/androidTest`) are left out. The
manifest (`AndroidManifest.xml`, and the nightly overlay, which adds only
the installer permission) declares no service, receiver, widget, alias or
notification channel; the app posts no notification.

89 `@Composable` functions are declared in the compiled-in files (the
"101 occurrences" of the reconnaissance counted the annotation where it
types a parameter, too). Records (`*` minted here, `C-`/`T-` and the
platform's name):

| Record | Draws | Desktop records it stands beside |
| --- | --- | --- |
| `T-ANDROID-THEME`* | Material theme in Koru's colors, `KoruSpacing` (xs…xl, five steps), theme and font choices, workspace and callout colors | `T-SPACE`, `T-SURFACE`, `T-INK`, `T-ACCENT`, `T-APPEARANCE`, `T-FONTS`, `T-NAMED-COLORS` |
| `C-ANDROID-APP`* | `MainActivity`, the `NavHost` and its routes, the error dialog | `H-MAIN-VIEW`, `H-MANAGER-VIEW` |
| `C-ANDROID-ENTITY-LIST`* | the list scaffold every list screen uses, entity tiles, action rows, empty state, create dialog | none: each desktop list is its surface's own rows |
| `C-ANDROID-LINK-DEVICE`* | link tabs (show QR, enter code, linked devices), relay switch, device history sheet, identity reset, QR scan button | `C-LINK-DEVICE-PANEL`, `C-MANAGE-DEVICES` |
| `C-ANDROID-PROFILES`* | the start screen, the profile sheet with Cloud sync switch | `C-MANAGE`, `C-EXPLORE-POPOVERS` (profile switcher) |
| `C-ANDROID-UPDATE-NOTICE`* | the nightly's update card and installer hand-off | `C-UPDATE-PROMPTS` |
| `C-ANDROID-WORKSPACES`* | workspaces with filters, sort and the action and debug sheets | `C-MANAGE-WORKSPACES`, `C-EXPLORE-POPOVERS` (workspace switcher) |
| `C-ANDROID-DOCUMENTS`* | Documents/Files tabs, file sheet, rename, trash | `C-EXPLORE` |
| `C-ANDROID-FILE-TRANSFERS`* | the Files tab's transfer card | `C-CONTEXT-COLUMN` (its Cloud sync section) |
| `C-ANDROID-ACCOUNT`* | sign-in, held accounts, profile enrollment, Cloud storage | `C-ONBOARDING` (sign-in), `C-ACCOUNT-SWITCHER`, `C-MANAGE-CLOUD` |
| `C-ANDROID-CLOUD-INVENTORY`* | the paged Cloud listing | `C-MANAGE-CLOUD` |
| `C-ANDROID-CLOUD-COPY`* | one Cloud copy, read-only, with file removal | `C-MANAGE-CLOUD` |
| `C-ANDROID-CLOUD-PROGRESS`* | a profile's Sync status screen | `C-MANAGE-CLOUD` (the progress disclosure) |
| `C-ANDROID-DOCUMENT-VIEWER`* | a document's blocks, edit mode, pictures, reference and contact cards | `C-DOCUMENT-EDITOR`, `C-DOCUMENT-TEXT`, `C-DOCUMENT-IMAGE`, `C-DOCUMENT-TABLE` |
| `C-ANDROID-RICH-EDITOR`* | the `EditText` one block is edited in | `C-DOCUMENT-EDITOR`, `C-IME-COMPOSITION` (the platform IME) |
| `C-ANDROID-RUN`* | a run's Overview and Activity, read-only | `C-RUNS-SURFACE` (without Result; CATCHUP *The run's three views*) |
| `C-ANDROID-SETTINGS`* | theme, language, font, transfers, licenses, diagnostics | `C-SETTINGS-SURFACE` |
| `C-ANDROID-SHARE`* | `ShareActivity` and the destination screen | none on Windows; the share extensions on Apple (below) |
| `C-ANDROID-SYSTEM-SURFACES`* | Credential Manager's account sheet, Google Play services' code scanner, the browser for provider sign-in and the no-display return activity, the system app chooser | the OS-owned desktop boundaries `C-FILE-DIALOGS` and `C-SYSTEM-MENUS` in kind; the browser sign-in `C-ONBOARDING` starts |

Option sets, each held to its Kotlin enum: `KoruThemePreference`,
`KoruFontChoice`, `WorkspaceAction`, `WorkspaceSort`,
`DocumentProjection`, `CloudCopyPhase`, `RunGlyph`. They are listed as
their identifiers: the Lab never offers a platform-only choice to a person.
Dispositions: 18 platform-only, 1 boundary (`C-ANDROID-SYSTEM-SURFACES`).

Defects found here (not fixed):

- **Hard-coded text and fixed formats a person reads**: `deviceEventLabel`
  shows an unknown event kind as its identifier with `_` replaced and an
  eight-character id (`KoruApp.kt` 976–996); the link-code failure puts an
  exception's message, or `unknown_error`, inside a translated sentence
  (405, 465); the developer screen's `Android {release}` and `koru.v1`
  (2170–2172); fixed separators join translated parts — `" · "` and a
  newline in the linked-device rows (704, 806, 829), `" — "` in the relay
  line (658–660), `" · "` and `", "` on the Sync status screen
  (`CloudProgressScreen.kt` 168, 183–184, 191, 231–232) and in the run pill
  (`RunPresentation.kt` 101); list ordinals are Kotlin templates with ASCII
  digits (`"$ordinal."`, `DocumentEditor.kt` 734) — the number trap the
  mobile README names.
- **Local values beside the theme**: the highlight and inline-code
  backgrounds are raw colors, twice (`DocumentEditor.kt` 683, 687;
  `NativeRichEditor.kt` 207); the headings use Material's ramp, a size
  apart from the desktop's (TECHDEBT 84, already filed).
- **Docs and source disagree**: mobile CATCHUP files *Auto-update* under
  "Decided: never — phones update through the App Store and Play Store",
  while the nightly Android app carries its own update notice, download
  and installer hand-off (`AppUpdates.kt`, `UpdateBanner`); CATCHUP's
  accessibility section says "Android draws its own reminder surfaces",
  while its *Reminders* row says neither phone schedules or delivers
  reminders and the Android source has no reminder UI.

### iOS (`apps/ios`)

20 non-test Swift files: `KoruMobile` 18, `KoruShare` 1, `KoruShared` 1
(the reconnaissance's 32 counted the 12 test files too). The 10 that
import SwiftUI or UIKit are compiled into the checks and were **read in
full**: `KoruMobileApp`, `Theme`, `Views` (753 lines), `DevicesScreen`,
`DocumentRendering`, `NativeRichTextEditor`, `SettingsScreen`,
`ShareDestinationScreen`, `OpenSourceLicensesScreen` and
`KoruShare/ShareViewController`. The other 10 draw nothing and were
**searched** (for SwiftUI, UIKit, `UIApplication`, alerts, notifications,
widgets and pickers — none) and read where a record's claim depended on
them: `AppModel` (444 lines; 150–194, the creation and editing entry
points, read), `KoruStoreServices` (1,305; the creation, upload and edit
refusals at 901–976, 1067 and 1260 read), `ContractMapper`, `Domain`,
`KoruSecretVault`, `MockServices`, `Native/KoruApiNative`,
`Native/KoruNative`, `Services` (the model, the store adapter over the C
shim and its mocks) and `KoruShared/ShareInbox` (the inbox format both
extensions and the app compile; its declarations read). The project has
three targets — the app, its unit tests and the share extension — and no
widget, notification-service or other extension.

29 views are declared in the compiled-in files (24 `View` structs, two of
them generic, which is why the reconnaissance's `struct … : View` count was
22; the `App`, a `ViewModifier`, two representables and the extension's
view controller).

| Record | Draws | Desktop records it stands beside |
| --- | --- | --- |
| `T-IOS-THEME`* | `KoruSpace` (the same five steps as Android), `KoruColor`, `KoruType`, `KoruEntityColor` | `T-SPACE`, `T-SURFACE`, `T-INK`, `T-ACCENT`, `T-TEXT-ROLES`, `T-NAMED-COLORS` |
| `C-IOS-PRIMARY-BUTTON`* | `KoruPrimaryButton` (catalog family `C-BUTTON`) | `C-BUTTON` |
| `C-IOS-EMPTY-STATE`* | `KoruEmptyState` | none: PLATFORMS.md's *KoruEmptyState (native name)* row, desktop not audited |
| `C-IOS-ROW`* | `KoruRow` | none: PLATFORMS.md's *KoruRow (native name)* row, desktop not audited |
| `C-IOS-APP`* | the `App`, the root stack and profiles list, creation sheet, error alert, share prompt | `H-MAIN-VIEW`, `H-MANAGER-VIEW`, `C-MANAGE` |
| `C-IOS-ENTITY-LIST`* | the list every list screen uses | none |
| `C-IOS-WORKSPACES`* | workspaces, filters, the details sheet | `C-MANAGE-WORKSPACES` |
| `C-IOS-DOCUMENTS`* | documents and files behind a segmented picker | `C-EXPLORE` |
| `C-IOS-DOCUMENT-VIEW`* | blocks, the desktop's heading ramp (ADR-412), markers, tables, pictures, the add-block bar | `C-DOCUMENT-EDITOR`, `C-DOCUMENT-TEXT`, `C-DOCUMENT-IMAGE`, `C-DOCUMENT-TABLE`, `T-DOCUMENT-TYPE` |
| `C-IOS-RICH-TEXT`* | the TextKit 2 block view with tappable pills | `C-DOCUMENT-EDITOR`, `C-IME-COMPOSITION` |
| `C-IOS-DEVICES`* | this device, relay, link by code or QR, pairing offer, linked devices, history | `C-LINK-DEVICE-PANEL`, `C-MANAGE-DEVICES` |
| `C-IOS-QR-SCANNER`* | VisionKit's `DataScannerViewController`, narrowed to QR (`NativeOwned`) | none: the desktop shows a code, it does not scan one |
| `C-IOS-SETTINGS`* | appearance, the system language link, licenses, diagnostics | `C-SETTINGS-SURFACE` |
| `C-IOS-SHARE-DESTINATION`* | where a staged share lands, in the app | none (Android's is `C-ANDROID-SHARE`) |
| `C-IOS-SHARE-EXTENSION`* | the `KoruShare` target's UIKit picker and staging | `C-MACOS-SHARE-EXTENSION` (one inbox format) |

Option sets, each held to its Swift enum: `RootView.Creator`,
`SettingsScreen.Appearance`, `OpenSourceLicenses.Document`.
Dispositions: 14 platform-only, 1 boundary (`C-IOS-QR-SCANNER`). The
drift checks hold the project's targets and the root's six routes (each
still opened by the navigation destination) to baselines.

Defects found here (not fixed):

- **Hard-coded English a person reads**: the device history rows build
  every sentence in English — "Untitled", "Updated “…”", "op"/"ops",
  "Reconciled …" and eight event names (`DevicesScreen.swift` 420–446),
  where Android's twin uses resources; the workspace details sheet's
  "yes"/"no" (`Views.swift` 223, 229); the developer screen's "iOS" and
  "koru.v1" (731–733); fixed `" — "` and `" · "` separators and `"-"`
  fallbacks (`DevicesScreen.swift` 145–147, 155, 445; `Views.swift` 713).
- **Controls that always fail**: the iOS screens offer Create on every
  list, add-paragraph/task/list/image, indent, outdent, delete, task
  toggles and typing, while the shipping store services refuse each one
  with `unsupported` (`KoruStoreServices.swift` 902, 922, 976, 1067, 1261)
  and the error alert is the only answer; Add image inserts a fixed 1×1
  PNG (`AppModel.swift` 179–186), a prototype path left in production UI.
- **Local values beside the theme**: presence and link failure use the
  system's raw green and red (`DevicesScreen.swift` 156, 187) beside
  `KoruColor`.
- **Docs and source disagree**: mobile CATCHUP says profile and workspace
  creation is desktop-only and "no phone UI offers them", and the mobile
  README says "the iOS editor foundation remains disabled" — the iOS UI
  offers both, and they fail (above); the README says Compose and SwiftUI
  use "equivalent Koru semantic tokens", but iOS's accent is a green
  (`KoruColor.accent`, `Theme.swift` 14) and Android's primary a blue
  (`Theme.kt` 123).

### macOS (`apps/macos/KoruShare`)

One Swift file, `ShareViewController.swift` (293 lines), **read in full**
and compiled into the checks: `C-MACOS-SHARE-EXTENSION`* (AppKit; a
one-line "saving" view while the items are staged into the channel's
inbox, then the app is brought forward). `koru-release`'s packager is the
only list of the Mac's native sources — it compiles this file with
`apps/ios/KoruShared/ShareInbox.swift` (the shared inbox format, excluded:
it draws nothing) — and the drift checks hold every Swift path it names to
a baseline. The desktop app draws nothing else in AppKit; its Mac-native
menus and window chrome are batch 2b's `C-SYSTEM-MENUS` and
`C-WINDOW-FRAME`. Desktop counterpart: none on Windows; the iOS extension
`C-IOS-SHARE-EXTENSION` writes the same inbox. Disposition: platform-only.

Defect (not fixed): "Saving to Koru…" is a hard-coded English literal
(`ShareViewController.swift` 37), where the iOS extension's is a
localized string; the extension has no catalog.

### Operating-system surfaces of the desktop

The two candidates batch 3 left to this one, now recorded (the files are
compiled into `tests/drift/app.rs`, whose checks cover them as app crates;
`app crates baselined` is five):

- `apps/koru-store-server` (34 non-test files; `lib.rs`, `main.rs`,
  `tray.rs` and `log.rs` **read in full**, the other 30 by their module
  documentation and **searched** for `rfd`, `MessageBox`, `tray-icon`,
  `muda`, `winit` and inspector scopes — none outside those two files;
  `daemon/fake_cloud.rs`, behind the test-support feature, is left out like
  a test). Its `lib.rs` is baselined: `tray` and `log` hold records, the
  other 18 modules are the Store worker and are excluded with a reason.
  `C-STORE-TRAY`* — the notification-area icon and menu (open the Store's
  folder, *Backup Store…* through the system's save and message dialogs,
  *Start on Windows startup*, *Quit*) and `C-STORE-FAILURE-DIALOG`* — the
  `MessageBoxW` a Store started without a console shows when it cannot
  start. Both `AppLocal` with their visible part a boundary, as batch 3's
  `C-AGENT-TRAY` is: `NativeOwned` on Windows, `Unavailable` on macOS and
  Linux (the tray is a stub that answers `None`, the dialog a stub that
  shows nothing), `NotApplicable` on the phones (they embed the Store and
  run no Store process; CATCHUP files the tray under *Deliberately
  desktop*).
- `apps/koru-notification-owner` (`main.rs`, 92 lines, **read in full**;
  baselined with no modules): `C-MAC-NOTIFICATION-OWNER`* — the macOS
  helper app that owns Koru's notifications and gives their three actions
  their titles from `koru-i18n` in the system's language. `NativeOwned` on
  macOS; not applicable elsewhere (on Windows it prints that it is
  Mac-only, and the package's COM activator owns activation).

And one crate the brief named, recorded for the surface it hands the
system:

- `crates/koru-native-notifications` (5 non-test files: `lib.rs` **read in
  full**; `sounds.rs`, `mac_owner.rs`, `windows.rs` and `macos.rs` by
  declarations, with the toast XML and the `UNUserNotificationCenter`
  calls searched). `C-SYSTEM-NOTIFICATIONS`* — a reminder as the operating
  system draws it: the title and body Koru supplies, the sound (the
  `BundledTone` table is an option set), and the Open, Snooze and Handled
  actions. `Support`, its visible part a boundary, as batch 2b's OS-owned
  records are. `NativeOwned` on Windows and macOS; `Unavailable` on Linux
  (`UnavailableNotifications` answers every call `Unsupported`) and on the
  phones (mobile CATCHUP, *Reminders — Wanted, phase 2*: neither phone
  schedules or delivers reminders). Its `lib.rs` is baselined in
  `tests/drift/domain.rs` with the domain crates, which are now 21.

Not recorded, each for its reason:

- `apps/koru-com-notification` (`main.rs`, 50 lines, read in full): the
  Windows COM activation broker the package starts for a notification's
  button; invisible and short-lived, it draws nothing.
- `crates/koru-mobile-host` (12 non-test files, 11,349 lines),
  `koru-mobile-native` (3, the Android JNI shim, a `cdylib`) and
  `koru-mobile-ios` (1, the iOS C ABI, a `staticlib`) — the only crates
  with a native-library edge; there is no UniFFI or Swift bridge crate.
  Their module documentation was read and their sources **searched** (no
  `koru-draw`, `koru-ui`, vello, parley or winit anywhere in them): they
  marshal generated Protobuf requests between the phones' native screens
  and the embedded Store, and draw nothing. The UI-facing shapes they carry (the run view, the document
  projection, the transfer and Cloud states) are drawn by the native
  records above, which is where they are counted.
- `crates/koru-share-inbox` (1 file): the Rust reader of the share inbox
  both share extensions write; draws nothing.

Defect (not fixed): `ActionLabels::default()` supplies English "Open",
"Snooze" and "Handled" (`koru-native-notifications` `lib.rs` 241–249)
while the type's own documentation says adapters "must not embed an
English fallback" (230–232). The Store's tray menu, backup dialogs and
failure box are English throughout (`tray.rs` 119, 252–255, 409–433;
`log.rs` 182; the crate has no `koru-i18n`), as batch 3 recorded.

## Reconciliation — the phone cells of every earlier record

Batches 1–3 left 262 Android and 261 iOS cells `NotAudited`. With every
native UI file read (above), each was settled from the native source or a
platform page, by four rules:

- **Native partial** — a screen or control in that phone's own source plays
  the record's role; the table names it. Never `Existing` for a desktop
  record: the phones link no Koru drawing crate (`koru-mobile-host`,
  `koru-mobile-native` and `koru-mobile-ios` use none of `koru-draw`,
  `koru-ui`, vello, parley or winit), so a phone never ships the record's
  own code. The phone's counterpart is its own record (`Existing` there).
- **Native-owned** — what stands in its place there is the system's own
  (a toast, the keyboard's composition, a system menu, scroll indicators).
- **Not applicable** — only where a page decides it never crosses: mobile
  CATCHUP's *Deliberately desktop* (the tray, the global shortcut, multiple
  windows), *Starting a delegation* (never on the phone), *Auto-update*
  (iOS only — the Android nightly has its own notice), and the platform
  half of accessibility.
- **Unavailable** — a page says the phone does not have it yet (search,
  artifacts, reminders, voice, sessions, importers, iOS's account, Cloud
  and run screens), or the full read of the phone's UI source finds no
  counterpart. Unavailable records the fact of absence; where a page owes
  a decision that would make a cell not applicable instead (PLATFORMS.md's
  *KoruCaption* row, "native applicability decision owed"), the row says
  so and leaves the decision to its owner.

Earlier cells that pages had already settled (the A2UI catalog records,
the element layer's `Planned`, the Settings controls, the search modal,
Socials, the branch chip) were kept, with one correction:
`C-SETTINGS-SURFACE` moves from `Unavailable` to native partial on both
phones — batch 2a read PLATFORMS.md's sentence about the 2026-09-28
Settings *controls* as the whole surface's, while each phone has its own
Settings screen.

**Moved: 260 Android and 260 iOS cells** out of `NotAudited` (plus the
Settings correction on each). **Left `NotAudited`: 2 Android, 1 iOS** —
`C-TASKS-VIEW` on both (CATCHUP says Android shows a Tasks page's rows,
but whether a Tasks page reaches either phone's document view was not
established), and `C-BUTTON` on Android: Material's `Button`,
`OutlinedButton` and `TextButton` (with an error-colored text button for
destructive answers) would make it native partial, but the Button was the
Lab's one storied record and its cells are pinned by
`ids_are_unique_and_every_platform_has_an_answer`, so the change is left
to the Button's owner.

In the code, a shared-desktop record now names its phone cells with
`registry.rs`'s `desktop_and` and its five named combinations
(`DESKTOP_PHONES_PARTIAL`, `DESKTOP_ANDROID_PARTIAL`,
`DESKTOP_IOS_PARTIAL`, `DESKTOP_NOT_ON_PHONES`, `DESKTOP_ONLY`);
`DESKTOP_SHARED` remains for what is still not audited. The application
compositions' helper takes the platforms as every other helper does.

<!-- phones:begin -->
| Record | Android | iOS | What stands in its place, or why nothing does |
| --- | --- | --- | --- |
| `H-APP-HOST` | native partial | native partial | the app's own process host: `MainActivity` and its container (C-ANDROID-APP), the SwiftUI `App` (C-IOS-APP) |
| `C-DOCUMENT-WINDOW` | not applicable | not applicable | CATCHUP *Deliberately desktop*: the tray, the global shortcut and multiple windows are decided never |
| `C-FORMAT-BUBBLE` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `C-DOCUMENT-MENUS` | unavailable | native partial | Android: absent (its document screen has no block or page menu); iOS: the block context menu (indent, outdent, delete) in C-IOS-DOCUMENT-VIEW |
| `C-FIND-REPLACE` | unavailable | unavailable | CATCHUP *Global search*: no search on a phone at all |
| `C-AGENT-SEARCH-WINDOW` | not applicable | not applicable | CATCHUP *Deliberately desktop*: the tray, the global shortcut and multiple windows are decided never |
| `C-AGENT-VOICE-WINDOWS` | not applicable | not applicable | CATCHUP *Deliberately desktop*: the tray, the global shortcut and multiple windows are decided never |
| `C-AGENT-TRAY` | not applicable | not applicable | CATCHUP *Deliberately desktop*: the tray, the global shortcut and multiple windows are decided never |
| `H-APP-LEAF` | native partial | native partial | the document screen a document opens in: C-ANDROID-DOCUMENT-VIEWER, C-IOS-DOCUMENT-VIEW |
| `H-MAIN-VIEW` | native partial | native partial | the navigation host: C-ANDROID-APP, C-IOS-APP |
| `C-LANE` | unavailable | unavailable | the phones show one screen at a time (`NavHost`, `NavigationStack`); no tabs, panes or sidebar in their source |
| `C-APP-CAPTION` | unavailable | unavailable | no window chrome in either phone's source; PLATFORMS.md still owes the native applicability decision (KoruCaption row), which would make this not applicable |
| `C-PANE-SPLITS` | unavailable | unavailable | the phones show one screen at a time (`NavHost`, `NavigationStack`); no tabs, panes or sidebar in their source |
| `C-APP-ZOOM-FLASH` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-ERROR-DIALOG` | native partial | native partial | the error dialog of C-ANDROID-APP (`AlertDialog` over `localizedFailure`) and the error alert of C-IOS-APP |
| `C-CHAT-FILE-PREVIEW` | unavailable | unavailable | no AI chat in either phone's UI source |
| `C-LINK-DEVICE-PANEL` | native partial | native partial | C-ANDROID-LINK-DEVICE, C-IOS-DEVICES |
| `C-DELEGATE-FORM` | not applicable | not applicable | CATCHUP *Starting a delegation*: decided never on the phone |
| `C-RECORDING-SETUP` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-AGENT-CLI-CARDS` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-FONT-PICKER` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-IMPORT-DIALOG` | unavailable | unavailable | CATCHUP *The other importers*: a phone has single-item share import only |
| `C-MOVE-TO` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-NAME-PROMPTS` | native partial | native partial | the rename and create dialogs of C-ANDROID-PROFILES, -WORKSPACES, -DOCUMENTS and C-ANDROID-ENTITY-LIST; the rename alerts and creation sheet of C-IOS-APP, -WORKSPACES, -DOCUMENTS |
| `C-REMINDER-EDITOR` | unavailable | unavailable | CATCHUP *Reminders*: wanted, phase 2; neither phone schedules or delivers |
| `C-UPDATE-PROMPTS` | native partial | not applicable | Android: the nightly's C-ANDROID-UPDATE-NOTICE (stable builds and store installs have none); iOS: CATCHUP *Auto-update*: decided never; phones update through their stores |
| `C-DICTATION-STRIP` | unavailable | unavailable | CATCHUP *Voice input*: wanted; neither phone has capture |
| `C-PROFILE-WAIT` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-APP-MENUS` | native partial | native partial | the long-press action sheets (Android) and context menus (iOS) the list screens' own source calls each row's actions; PLATFORMS.md warns against *assuming* right-click becomes long-press, and this is the source's mapping, not an assumption |
| `C-APP-WELCOME` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-MANAGE` | native partial | native partial | Android: C-ANDROID-PROFILES and C-ANDROID-ACCOUNT; iOS: the profiles list of C-IOS-APP (no sign-in) |
| `C-MANAGE-WORKSPACES` | native partial | native partial | C-ANDROID-WORKSPACES, C-IOS-WORKSPACES |
| `C-MANAGE-DEVICES` | native partial | native partial | C-ANDROID-LINK-DEVICE, C-IOS-DEVICES |
| `C-MANAGE-CLOUD` | native partial | unavailable | Android: C-ANDROID-ACCOUNT's Cloud storage, C-ANDROID-CLOUD-INVENTORY, -CLOUD-COPY and -CLOUD-PROGRESS (no subscription: CATCHUP); iOS: CATCHUP *Manage Profiles' Cloud sync progress*: iOS has the bindings and no screen |
| `C-TRASH-PAGE` | unavailable | unavailable | neither phone lists or restores trashed documents (CATCHUP says both restore; see the defects) |
| `C-FEEDBACK-PAGE` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-VERSION-COMPARE` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-CANVAS-TAB` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full): the phones draw a canvas block as its stored picture |
| `C-CANVAS-TEXT-DIALOG` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-SUPPORT-VIEW` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-SUPPORT-FAB` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-SESSION-OFFER` | unavailable | unavailable | CATCHUP *Watching a run's session*: not on a phone (D15, follows M8) |
| `C-REMINDER-SETTINGS` | unavailable | unavailable | CATCHUP *Reminders*: wanted, phase 2; neither phone schedules or delivers |
| `C-POPOUT-CLOSE-DIALOG` | not applicable | not applicable | CATCHUP *Deliberately desktop*: the tray, the global shortcut and multiple windows are decided never |
| `H-MANAGER-VIEW` | native partial | native partial | the routes of C-ANDROID-APP and C-IOS-APP |
| `H-APP-TAB-HOSTS` | unavailable | unavailable | the phones show one screen at a time (`NavHost`, `NavigationStack`); no tabs, panes or sidebar in their source |
| `C-JUMP-LIST` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-EXPLORE` | native partial | native partial | the documents and files lists: C-ANDROID-DOCUMENTS, C-IOS-DOCUMENTS |
| `C-APPS-RAIL` | unavailable | unavailable | PLATFORMS.md *Mini-app pin group*: no Android or iOS adapter |
| `C-TASKS-RAIL` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-HOME-NAV` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-HOME-PAGE` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-EXPLORE-POPOVERS` | native partial | native partial | the profile and workspace screens that switch between them: C-ANDROID-PROFILES/-WORKSPACES, C-IOS-APP/-WORKSPACES |
| `C-CONTEXT-COLUMN` | native partial | unavailable | Android: C-ANDROID-FILE-TRANSFERS, the Cloud sync section's equivalent (CATCHUP *The Cloud sync tab as the one place*); iOS: no counterpart anywhere in the phone's UI source (read in full) |
| `C-TASKS-VIEW` | not audited | not audited | CATCHUP says Android shows a Tasks page's rows; both phones draw task blocks in their document view, but whether a Tasks page reaches them as a document was not established |
| `C-CAPTION` | unavailable | unavailable | no window chrome in either phone's source; PLATFORMS.md still owes the native applicability decision (KoruCaption row), which would make this not applicable |
| `C-WELCOME` | native partial | unavailable | Android: C-ANDROID-ACCOUNT's sign-in choices and its continue-offline action; iOS: CATCHUP *Hosted account parity*: iOS account surfaces remain I6; none in source |
| `C-LAUNCHER` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-MEDIA-VIEWER` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full): pictures stay inline; Android hands a file to another app |
| `C-PAGE-SHELL` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-ICON-BUTTON` | native partial | native partial | Material `IconButton` (options, note, edit) / borderless glyph buttons and toolbar labels |
| `C-BUTTON-GROUP` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-SPLIT-BUTTON` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-COPY` | native partial | native partial | the copy buttons for a link code and a workspace id (Android confirms with a system toast, iOS does not confirm) |
| `C-CHIP` | native partial | unavailable | Android: Material `FilterChip` (theme, language, font); iOS: no counterpart anywhere in the phone's UI source (read in full) |
| `C-TOGGLE` | native partial | native partial | Material `Switch` / SwiftUI `Toggle` (relay, transfers, Cloud sync, filters) |
| `C-COLOR-PICKER-ROW` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-PROFILE-PICKER` | native partial | native partial | the profile rows with initials tiles (C-ANDROID-PROFILES, the share screens' profile choice; `ProfileBadge`) |
| `C-BADGE` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-LABEL-CHIP` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-CONTEXT-CHIP` | unavailable | unavailable | no AI chat in either phone's UI source |
| `C-KEY-CHIP` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-TEXT-FIELD` | native partial | native partial | Material `OutlinedTextField` / SwiftUI `TextField` |
| `C-TEXT-AREA` | native partial | native partial | the share screens' multi-line note fields |
| `C-TEXT-INPUT` | native partial | native partial | Material `OutlinedTextField` / SwiftUI `TextField` (link code, names) |
| `C-DATE-GRID` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-SELECTABLE-LIST` | native partial | native partial | the platform lists of C-ANDROID-ENTITY-LIST and C-IOS-ENTITY-LIST (no keyboard stepping) |
| `C-EXIT-LIST` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-TABLE` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full): a document's table is C-DOCUMENT-TABLE's |
| `C-CHIP-NAV` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-SCROLLBAR` | unavailable | native-owned | Android: Compose's lazy lists draw none; iOS: the system's own scroll indicator |
| `C-POPOVER` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-DIALOG-FRAME` | native partial | native partial | the rename dialogs (Material `AlertDialog` with a field) / rename alerts |
| `C-MODAL` | native partial | native partial | Material `AlertDialog` and `ModalBottomSheet` / SwiftUI sheets |
| `C-PLACE-CHOOSER` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-PROMPT` | native partial | native partial | Material `AlertDialog` / SwiftUI alerts |
| `C-MENU` | native partial | native-owned | Android: the action sheets of C-ANDROID-ENTITY-LIST; iOS: SwiftUI `Menu` and context menus, drawn by the system |
| `C-MENU-ITEM` | native partial | native-owned | Android: `WorkspaceActionRow`; iOS: a system menu's items |
| `C-CONFIRM` | native partial | native partial | the delete, start-over, free-up-space, remove and discard confirmations / `confirmationDialog` |
| `C-CHOICE-DIALOG` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-TOOLTIP` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-TOAST` | native-owned | unavailable | Android: the system `Toast` (no action); iOS: no counterpart anywhere in the phone's UI source (read in full) |
| `C-PROGRESS-BAR` | native partial | unavailable | Android: Material `LinearProgressIndicator`; iOS: only the spinner |
| `C-PROGRESS-RING` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-PULSING-DOTS` | native partial | native partial | the pending role is played by Material `CircularProgressIndicator` / SwiftUI `ProgressView` |
| `C-MEASUREMENT-BADGE` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-DIVIDER` | native partial | native partial | Material `HorizontalDivider` / a drawn rule and list separators |
| `C-FILE-ICON` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full): one generic file glyph |
| `C-FONT-PREVIEW` | native partial | unavailable | Android: the font chips set in their own faces (C-ANDROID-SETTINGS); iOS: no counterpart anywhere in the phone's UI source (read in full) |
| `C-HIGHLIGHTED-TEXT` | unavailable | unavailable | CATCHUP *Global search*: no search on a phone at all |
| `C-LOGO` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-TEXT-PARAGRAPH` | native partial | native partial | the platform's wrapping `Text` |
| `C-TEXT-TITLE` | native partial | native partial | the top app bar title / navigation title |
| `H-CLIPBOARD` | native partial | native partial | `ClipboardManager` / `UIPasteboard` |
| `H-CURSOR` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `H-KEYS` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `H-DICTATION` | unavailable | unavailable | CATCHUP *Voice input*: wanted; neither phone has capture |
| `H-DATES` | native partial | native partial | `DateUtils` relative times / `RelativeDateTimeFormatter` |
| `C-SESSION-SURFACE` | unavailable | unavailable | CATCHUP *Watching a run's session*: not on a phone (D15, follows M8) |
| `C-WATCH-BAND` | unavailable | unavailable | CATCHUP *Watching a run's session*: not on a phone (D15, follows M8) |
| `C-AI-CHAT-SURFACE` | unavailable | unavailable | no AI chat in either phone's UI source |
| `C-AI-CONVERSATION` | unavailable | unavailable | no AI chat in either phone's UI source |
| `C-AI-TOOL-CALL-DETAIL` | unavailable | unavailable | no AI chat in either phone's UI source |
| `C-AI-NEW-DIVIDER` | unavailable | unavailable | no AI chat in either phone's UI source |
| `C-AI-COMPOSER` | unavailable | unavailable | no AI chat in either phone's UI source |
| `C-AI-COMPOSER-DOCUMENT` | unavailable | unavailable | no AI chat in either phone's UI source |
| `C-AI-COMPOSER-PLUS-MENU` | unavailable | unavailable | no AI chat in either phone's UI source |
| `C-AI-ATTACHMENT-STRIP` | unavailable | unavailable | no AI chat in either phone's UI source |
| `H-AI-VIEWER-LIFT` | unavailable | unavailable | no AI chat in either phone's UI source |
| `C-AI-MODEL-PICKER` | unavailable | unavailable | no AI chat in either phone's UI source |
| `C-AI-MICROPHONE-PANEL` | unavailable | unavailable | CATCHUP *Voice input*: wanted; neither phone has capture |
| `C-AI-DICTATION-ROW` | unavailable | unavailable | CATCHUP *Voice input*: wanted; neither phone has capture |
| `C-AI-PROVIDER-SETUP` | unavailable | unavailable | no AI chat in either phone's UI source |
| `H-AI-CHAT-STATUS` | unavailable | unavailable | no AI chat in either phone's UI source |
| `H-AI-CHAT-DIAGRAMS` | unavailable | unavailable | no AI chat in either phone's UI source |
| `H-AI-AGENT-PROVIDERS` | unavailable | unavailable | no AI chat in either phone's UI source |
| `C-SHELL-CAPTION` | unavailable | unavailable | no window chrome in either phone's source; PLATFORMS.md still owes the native applicability decision (KoruCaption row), which would make this not applicable |
| `C-SHELL-POPOVER` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-SHELL-ZOOM-LABEL` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `H-SHELL-PANEL` | unavailable | unavailable | the phones show one screen at a time (`NavHost`, `NavigationStack`); no tabs, panes or sidebar in their source |
| `H-SHELL-TABS` | unavailable | unavailable | the phones show one screen at a time (`NavHost`, `NavigationStack`); no tabs, panes or sidebar in their source |
| `H-SHELL-BANNER-STATE` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `H-SHELL-HOST` | not applicable | not applicable | CATCHUP *Deliberately desktop*: the tray, the global shortcut and multiple windows are decided never |
| `H-SHELL-SURFACE` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-EMOJI-COLOR-PANEL` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `C-ARTIFACTS-COLUMN` | unavailable | unavailable | CATCHUP *The Artifacts surface*: wanted, after M7 |
| `C-ARTIFACT-TAB` | unavailable | unavailable | CATCHUP *The Artifacts surface*: wanted, after M7 |
| `C-CANVAS-SURFACE` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full): a canvas block is drawn as its stored picture |
| `H-CANVAS-CAMERA` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-CANVAS-OBJECTS` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full): a canvas block is drawn as its stored picture |
| `C-CHATS-LIST` | unavailable | unavailable | no AI chat in either phone's UI source |
| `H-CHAT-STATUS` | unavailable | unavailable | no AI chat in either phone's UI source |
| `C-DOCUMENT-EDITOR` | native partial | native partial | C-ANDROID-DOCUMENT-VIEWER with C-ANDROID-RICH-EDITOR (text editing only); C-IOS-DOCUMENT-VIEW with C-IOS-RICH-TEXT (reading; its edits are refused) |
| `H-EDITOR-EMBEDDED` | unavailable | unavailable | no AI chat in either phone's UI source |
| `H-EDITOR-POLICY` | unavailable | unavailable | no AI chat in either phone's UI source |
| `C-DOCUMENT-TEXT` | native partial | native partial | `RichTextBlock` / `NativeRichTextEditor`: marks, pills, headings, lists |
| `C-DOCUMENT-IMAGE` | native partial | native partial | `BlobImage` on both phones |
| `C-DOCUMENT-TABLE` | native partial | native partial | `TableBlock` on both phones (one Markdown reader) |
| `C-DOCUMENT-DIAGRAM` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full): an unsupported block's label |
| `C-ROW-GUTTER` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `C-ROW-ATTRIBUTE-CHIPS` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-LINK-CARD` | native partial | native partial | Android's `ReferenceCard`; iOS's labelled link row (CATCHUP *Tables render as a placeholder*) |
| `C-EDITOR-OVERLAY` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `C-COMMAND-MENU` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `C-ENTITY-PICKER` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `C-DATE-PICKER` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `C-EMOJI-LIST` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `H-EMOJI-CATALOG` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `C-LANGUAGE-PICKER` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `C-LINK-PROMPT` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `C-LIST-PICKER` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `C-NAME-PROMPT` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `C-PRIORITY-PICKER` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `C-TAG-PICKER` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `C-ACCENT-COLOR-PICKER` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `C-ATTR-MENU` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `H-EDITOR-ACCESSIBILITY` | unavailable | unavailable | CATCHUP *Native accessibility semantics*: decided not yet |
| `H-EDITOR-KEYMAP` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `H-EDITOR-TRIGGERS` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `H-EDITOR-CLIPBOARD` | native partial | unavailable | Android: the platform field's own paste, split into paragraphs (CATCHUP *Writing in a document body*); iOS: edits are refused |
| `H-EDITOR-DICTATION` | unavailable | unavailable | CATCHUP *Voice input*: wanted; neither phone has capture |
| `H-PILL-INFO` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `H-EDITOR-REMINDERS` | unavailable | unavailable | CATCHUP *Reminders*: wanted, phase 2; neither phone schedules or delivers |
| `H-EDITOR-TASKS` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `H-EDITOR-DATES` | unavailable | unavailable | neither phone's document UI has this kind of editing tool (CATCHUP *Writing in a document body*: Android edits plain text, with no formatting controls or pickers; iOS's only tools are the add-block bar and a block menu) |
| `C-MARKDOWN-VIEW` | unavailable | unavailable | CATCHUP *The other importers*: a phone stores a `.md` as an opaque file |
| `C-MARKDOWN-DOCUMENT` | unavailable | unavailable | CATCHUP *The other importers*: a phone stores a `.md` as an opaque file |
| `C-RUNS-SURFACE` | native partial | unavailable | Android: C-ANDROID-RUN, Overview and Activity without Result or controls (CATCHUP *The run's three views*); iOS: CATCHUP *Delegated work*: iOS is catch-up row I7; no run screen in source |
| `H-RUN-SUMMARY` | native partial | unavailable | Android: `RunPresentation` (glyph, overview, pill label); iOS: CATCHUP *Delegated work*: iOS is catch-up row I7; no run screen in source |
| `C-SEARCH-SURFACE` | unavailable | unavailable | CATCHUP *Global search*: no search on a phone at all |
| `H-ENTITY-SEARCH` | unavailable | unavailable | CATCHUP *Global search*: no search on a phone at all |
| `C-TERMINAL-VIEW` | unavailable | unavailable | CATCHUP *Watching a run's session*: not on a phone (D15, follows M8) |
| `H-TERMINAL-STYLE` | unavailable | unavailable | CATCHUP *Watching a run's session*: not on a phone (D15, follows M8) |
| `C-TERMINAL-CONTENT` | unavailable | unavailable | CATCHUP *Watching a run's session*: not on a phone (D15, follows M8) |
| `C-MANAGED-SESSION-VIEW` | unavailable | unavailable | CATCHUP *Watching a run's session*: not on a phone (D15, follows M8) |
| `C-MANAGED-TRANSCRIPT` | unavailable | unavailable | CATCHUP *Watching a run's session*: not on a phone (D15, follows M8) |
| `C-PENDING-INPUT-VIEW` | unavailable | unavailable | CATCHUP *Watching a run's session*: not on a phone (D15, follows M8) |
| `H-SESSION-INTERACTION` | unavailable | unavailable | CATCHUP *Watching a run's session*: not on a phone (D15, follows M8) |
| `C-SETTINGS-SURFACE` | native partial | native partial | C-ANDROID-SETTINGS, C-IOS-SETTINGS (appearance, language, font, transfers): batch 2a read PLATFORMS.md's sentence about the Settings *controls* as the whole surface's |
| `C-STATUS-CHIP` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-STATUS-MARK` | unavailable | unavailable | no AI chat in either phone's UI source |
| `C-TAB-STRIP` | unavailable | unavailable | the phones show one screen at a time (`NavHost`, `NavigationStack`); no tabs, panes or sidebar in their source |
| `P-PAGE-EMOJI` | native partial | unavailable | Android: a document's emoji tile in C-ANDROID-DOCUMENTS; iOS: documents show none |
| `H-BANNER-SLOTS` | unavailable | unavailable | no window chrome in either phone's source; PLATFORMS.md still owes the native applicability decision (KoruCaption row), which would make this not applicable |
| `H-CAPTION-LAYOUT` | unavailable | unavailable | no window chrome in either phone's source; PLATFORMS.md still owes the native applicability decision (KoruCaption row), which would make this not applicable |
| `H-LANE-LAYOUT` | unavailable | unavailable | the phones show one screen at a time (`NavHost`, `NavigationStack`); no tabs, panes or sidebar in their source |
| `H-WINDOW-REGIONS` | unavailable | unavailable | no window chrome in either phone's source; PLATFORMS.md still owes the native applicability decision (KoruCaption row), which would make this not applicable |
| `H-DRAG-REORDER` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `C-VOICE-SURFACE` | unavailable | unavailable | CATCHUP *Voice input*: wanted; neither phone has capture |
| `C-VOICE-HUD` | unavailable | unavailable | CATCHUP *Voice input*: wanted; neither phone has capture |
| `C-WEB-SURFACE` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full): both phones hand a URL to the browser |
| `H-WINDOW-RUNNERS` | unavailable | unavailable | the phones link no Koru drawing crate (`koru-mobile-*` use none of koru-draw, koru-ui, vello, parley or winit); no counterpart in their UI source |
| `H-WINDOW-ACCESSIBILITY` | not applicable | not applicable | CATCHUP *Native accessibility semantics*: the platform half (`koru_window::accessibility`) must not cross |
| `C-WINDOW-FRAME` | unavailable | unavailable | no window chrome in either phone's source; PLATFORMS.md still owes the native applicability decision (KoruCaption row), which would make this not applicable |
| `C-IME-COMPOSITION` | native-owned | native-owned | the system keyboard composes in the platform field (`EditText`, which C-ANDROID-RICH-EDITOR leaves alone while a composing span is open; `UITextView`); PLATFORMS.md's *Not audited* is about the P1 extension's own adapter, which is still not implemented |
| `H-WINDOW-INPUT` | unavailable | unavailable | the phones link no Koru drawing crate (`koru-mobile-*` use none of koru-draw, koru-ui, vello, parley or winit); no counterpart in their UI source |
| `H-WINDOW-RENDERER` | unavailable | unavailable | the phones link no Koru drawing crate (`koru-mobile-*` use none of koru-draw, koru-ui, vello, parley or winit); no counterpart in their UI source |
| `H-SYSTEM-APPEARANCE` | native partial | native partial | both follow the system's light and dark (`isSystemInDarkTheme`, SwiftUI's color scheme) with an override (T-ANDROID-THEME, C-IOS-SETTINGS) |
| `C-ONBOARDING` | native partial | unavailable | Android: C-ANDROID-ACCOUNT's sign-in and profile enrollment (no profile creation: CATCHUP); iOS: CATCHUP *Hosted account parity*: iOS account surfaces remain I6; none in source |
| `C-ACCOUNT-SWITCHER` | native partial | unavailable | Android: the held accounts of C-ANDROID-ACCOUNT; iOS: CATCHUP *Hosted account parity*: iOS account surfaces remain I6; none in source |
| `H-ACCOUNT-STATE` | native partial | unavailable | Android: `AccountPresentation`'s screen state (services, searched); iOS: CATCHUP *Hosted account parity*: iOS account surfaces remain I6; none in source |
| `H-VIEW-HOST` | unavailable | unavailable | the phones link no Koru drawing crate (`koru-mobile-*` use none of koru-draw, koru-ui, vello, parley or winit); no counterpart in their UI source |
| `H-WORKSPACE-ACCESSIBILITY` | unavailable | unavailable | CATCHUP *Native accessibility semantics*: the vocabulary could cross; decided not yet |
| `H-NATIVE-SURFACE` | unavailable | unavailable | the phones link no Koru drawing crate (`koru-mobile-*` use none of koru-draw, koru-ui, vello, parley or winit); no counterpart in their UI source |
| `C-SYSTEM-MENUS` | unavailable | native-owned | Android: no system menu in source (its sheets are the app's own); iOS: SwiftUI context menus and `Menu`, drawn by the system |
| `C-FILE-DIALOGS` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full): Android's system chooser hands a file out, nothing picks one |
| `H-KEYBOARD-ROUTING` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `T-PROFILE` | unavailable | unavailable | the phones link no Koru drawing crate (`koru-mobile-*` use none of koru-draw, koru-ui, vello, parley or winit); no counterpart in their UI source |
| `T-SPACE` | native partial | native partial | `KoruSpacing` / `KoruSpace`: five of the fifteen steps (4, 8, 16, 24, 32) |
| `T-RADIUS` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full): raw corner radii |
| `T-ICON-SIZE` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `T-METRICS` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `T-ALIASES` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `T-TEXT-ROLES` | native partial | native partial | Material typography overrides / `KoruType` |
| `T-SURFACE` | native partial | native partial | Material color scheme in Koru's values / `KoruColor.surface` |
| `T-INK` | native partial | native partial | Material `onSurface`/`onSurfaceVariant` / `KoruColor.subtle` |
| `T-LINE` | native partial | unavailable | Android: Material `outline` in Koru's values; iOS: a raw opacity |
| `T-ACCENT` | native partial | native partial | Material `primary` / `KoruColor.accent` (they differ: see the defects) |
| `T-THEMES` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `T-CONTRAST` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `T-PALETTE` | native partial | native partial | the light and dark Material schemes / `KoruColor` |
| `T-TYPE-SCALE` | native partial | native partial | Material typography / `KoruType` and the document ramp |
| `T-ACCENT-CHOICES` | native partial | unavailable | Android: the callout accent tokens (`calloutTint`); iOS: no counterpart anywhere in the phone's UI source (read in full) |
| `T-NAMED-COLORS` | native partial | native partial | the workspace color names, light and dark, in `EntityTileColors` / `KoruEntityColor` |
| `T-TERMINAL-PALETTE` | unavailable | unavailable | CATCHUP *Watching a run's session*: not on a phone (D15, follows M8) |
| `T-APPEARANCE` | native partial | native partial | `KoruThemePreference` / `SettingsScreen.Appearance` (system, light, dark) |
| `T-CARET` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `T-FONTS` | native partial | unavailable | Android: `KoruFontChoice`; iOS: no counterpart anywhere in the phone's UI source (read in full) |
| `T-DOCUMENT-TYPE` | native partial | native partial | Android maps to Material's ramp (TECHDEBT 84); iOS pins the desktop's ramp by value (ADR-412) |
| `T-LAYOUT-CONSTANTS` | unavailable | unavailable | the phones link no Koru drawing crate (`koru-mobile-*` use none of koru-draw, koru-ui, vello, parley or winit); no counterpart in their UI source |
| `T-MOTION` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `T-POPOVER-GEOMETRY` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `H-APPEARANCE-PREVIEW` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `P-FLOATING-PANEL` | unavailable | unavailable | the phones link no Koru drawing crate (`koru-mobile-*` use none of koru-draw, koru-ui, vello, parley or winit); no counterpart in their UI source |
| `P-CHECKBOX` | native partial | native partial | Material `Checkbox` / a checkmark symbol (task markers, profile choice) |
| `H-ATTRIBUTE-CHIP` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `P-PILL` | native partial | native partial | the reference pills: Android's annotated text and `ReferenceSpan`, iOS's attachment image |
| `P-IMAGE` | native partial | native partial | `BlobImage` on both phones |
| `P-TILE` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `P-BRAND` | native partial | unavailable | Android: the provider mark on the sign-in button; iOS: no counterpart anywhere in the phone's UI source (read in full) |
| `P-ICONS` | native partial | native partial | Material icons / SF Symbols, not Koru's icon pack |
| `P-DIAGRAM` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `P-SHAPES` | unavailable | unavailable | the phones link no Koru drawing crate (`koru-mobile-*` use none of koru-draw, koru-ui, vello, parley or winit); no counterpart in their UI source |
| `P-TEXT-PAINT` | unavailable | unavailable | the phones link no Koru drawing crate (`koru-mobile-*` use none of koru-draw, koru-ui, vello, parley or winit); no counterpart in their UI source |
| `P-STATUS-BAR` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `P-SMALL-MARKS` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `H-GEOMETRY` | unavailable | unavailable | the phones link no Koru drawing crate (`koru-mobile-*` use none of koru-draw, koru-ui, vello, parley or winit); no counterpart in their UI source |
| `H-TEXT-ENGINE` | unavailable | unavailable | the phones link no Koru drawing crate (`koru-mobile-*` use none of koru-draw, koru-ui, vello, parley or winit); no counterpart in their UI source |
| `H-ANIMATION` | unavailable | unavailable | the phones link no Koru drawing crate (`koru-mobile-*` use none of koru-draw, koru-ui, vello, parley or winit); no counterpart in their UI source |
| `H-INSPECT` | unavailable | unavailable | COMPONENTS.md: inspector enablement on mobile native hosts is outside the coverage claim; no inspector in either phone's source |
| `H-INSTALLED-FONTS` | unavailable | unavailable | no counterpart anywhere in the phone's UI source (read in full) |
| `H-EMOJI` | native partial | native partial | the leading-emoji split the profile and workspace tiles draw (`leadingEmoji`) |
<!-- phones:end -->

## Reconciled against main — 17 November 2026

`origin/main` was merged into the branch (`7237d6c3`, 32 commits). It brought
the reader: reading pictures and scanned files so search finds the words in
them, with its Store side, its Settings card, its line in search, its
right-click entries and a tab for the text it read. Five drift checks failed
on the merge, as they are meant to, and each new thing was read and
classified by hand as every earlier one was.

**New record: 1.** `C-OCR-TAB` (`apps/koru/src/ocr_view.rs`, `OcrView`): the
tab that shows the text read from a picture or scanned file, with Edit, Save,
Cancel, Reset to OCR, Refresh, Save reading to file and Next saved version in
a toolbar over the app's own document view. App-local, a composition, no
story yet. Its toolbar's buttons are the shared `ActionButton` behind an
element leaf of its own; it registers the containers "OCR tab" and "OCR
document" and no component identity.

**Records extended: 4.**

- `C-SEARCH-SURFACE` gained the identity **Reading offer** (the one line
  about pictures and scans, `koru-search-ui/src/lib.rs`) and the symbols
  `LineView`, `LineAction` (TurnOn, NotNow, Stop, TryAgain), `import_summary`
  and `install_line`, all from the new private module `offer`.
- `C-SETTINGS-SURFACE` gained the reader's card: `ReaderRow`,
  `ReaderItemRow`, `ReaderPartlyRow` and the closed states `ReaderStatus`,
  `ReaderBlocked`, `ReaderDownloadStage` and `ReaderDownloadEnd`, from the
  new private module `reader`; and `Request` gained its seven reader
  members (TurnOnReader … ReadAllReaderPages).
- `C-CHAT-FILE-PREVIEW`'s identity is now **FilePreview**: the painter was
  renamed when the same preview began to show extracted readings as well as
  chat files. The record's catalog ID and its translated name still say
  "chat file preview"; see the defects list.
- `H-MAIN-VIEW`'s `LiftedViewer` gained `FileImage`, the workspace picture
  the shared media viewer is lifted over.

**Modules classified: 15.**

| Module | Disposition |
| --- | --- |
| `koru::ocr_view` | record `C-OCR-TAB` |
| `koru::mainview::file_images` | excluded, orchestration: holds the shared `KoruMediaViewer` over a workspace picture |
| `koru::mainview::file_paste` | excluded, orchestration: asks where pasted files go through the shared `koru_ui::Choice` |
| `koru::mainview::files_reader` | excluded, io: the reader's state for the Files menu |
| `koru::mainview::reader_offer` | excluded, orchestration: the offer line on the import summary's shared toast |
| `koru::manage::sync_modes` | `C-MANAGE`: sync choices composed from the shared `CardRow` and action buttons |
| `koru::settings_view::reader_link` | excluded, io: the Settings page's thread to the Store about the reader |
| `koru_store_server::readings` | excluded, io: the Store's side of reading |
| `koru_search_ui::offer` | `C-SEARCH-SURFACE` |
| `koru_search_ui::row_text` | excluded, private helper: the words a result row adds of its own |
| `koru_settings_ui::reader` | `C-SETTINGS-SURFACE` |
| `koru_workspace_host::reader_offer` | excluded, io |
| `koru_workspace_host::reading_details` | excluded, data |
| `koru_workspace_host::reading_file` | excluded, io |
| `koru_workspace_host::reading_requests` | excluded, data |

**Not records, and why.** The new crates `koru-read`, `koru-read-paddle`,
`koru-read-bench` and `koru-models` and the app `koru-read` recognize text
and manage model downloads; none of them draws anything or links a drawing
crate, so they are outside the inventory's scope as the store and transport
crates are. `koru-ui`'s `Toast` gained a second line with answers of its own
(`with_line`, `line_rects`, `set_line_focus` and their neighbours); they are
methods of the `Toast` the record `C-TOAST` already names. The submodules
`app/file_paste.rs`, `app/image_file.rs` and `app/reading_rescan.rs` of the
workspace host's `app` are plumbing inside the one module the inventory
reads at declaration level.

**Reading depth.** The new modules were read at their module documentation,
their public and crate-visible declarations and their inspector scopes.
`ocr_view.rs` was read through its element tree and draw path;
`koru-search-ui/src/offer.rs` and `koru-settings-ui/src/reader.rs` through
their public types. The changes main made inside files already baselined
(`koru-editor`'s draw, the document app's context menu, Manage's profile
pages) were checked only as far as the drift checks reach: their module
lists, identities, recorded symbols and option sets are unchanged or are
listed above.

**Added to the defects list by this reconciliation:**

- `C-CHAT-FILE-PREVIEW` is named for chats in its ID and in all eight
  languages, and the painter it records now also shows extracted readings.
- The OCR tab draws its toolbar buttons through a private element leaf
  round `ActionButton::draw_focused` rather than a shared button element.

## Reconciled against main — 20 November 2026

The UI Lab was merged into main (`0a177dbbc`), and main brought nested task
lists, the sidebar's quick filter, Ctrl+Tab's quick switch, opening a list
or a task from search in Tasks, the task list kept current from the Store's
events, and the search scope chip. The drift checks failed on the merge,
as they are meant to, and each new thing was read and classified by hand
as every earlier one was.

**New records: 2.**

- `H-TREE-DRAG` (`crates/koru-ui/src/tree_drag.rs`, module
  `koru_ui::tree_drag`): the arithmetic of a drag in an indented list —
  where the dragged row lands (`drop_target` over `Row`, answering a
  `Target` in a `Zone`), when a closed row under it springs open
  (`SpringOpen`) and how fast the list scrolls at its edges
  (`edge_scroll`). The document tree and the Lists section of the tasks
  rail share it. Support, in the controls' support group, nonvisual; it
  draws nothing and registers no identity.
- `C-QUICK-SWITCH` (`apps/koru/src/mainview/quick_switch.rs`,
  `QuickSwitch`): the list Ctrl+Tab shows of the window's open tabs, most
  recently used first, with a ✕ on the hovered row. App-local, a
  composition, desktop only, no story yet. Built purely from `Entry` rows
  the container makes; `QuickSwitch::draw` is its painter. It registers no
  component identity.

**Records extended: 4.**

- `C-SEARCH-SURFACE` gained the identity **Search scope chip** (the chip
  that says which workspaces a search covers, `koru-search-ui/src/lib.rs`),
  and its `Target` option set now reads Store, List, Task, Chat: the two
  new members open a task list as its page in Tasks, and a task on the page
  where it is now.
- `C-TOOLTIP`'s placement option set gained **Above**
  (`Tooltip::above`).
- `T-TEXT-ROLES` gained `PAGE_TITLE`, the text role of a page's title.
- `C-TASKS-RAIL`'s `RailRow` option set reads `Compose` where it read
  `NewList`: the row a new list's name is typed on now sits first under
  the heading, or first under its parent for a child list.

**Modules classified: 6.**

| Module | Disposition |
| --- | --- |
| `koru_ui::tree_drag` | record `H-TREE-DRAG` |
| `koru::mainview::quick_switch` | record `C-QUICK-SWITCH` |
| `koru::panes::explore::quick_filter` | `C-EXPLORE`: the sidebar's rows narrowed by name, a projection over the rows the pane holds |
| `koru::mainview::task_events` | excluded, io: one live tail per window on the Store's tasks-changed event, the Tasks surfaces' trigger to read again |
| `koru::tasks_view::landing` | excluded, io: one bounded query for a task opened from search, and the pure choice of the Tasks page it lands on |
| `koru_agent::cloud_reconcile` | excluded, orchestration: re-applies the remembered Cloud sync choices on the Store's catalog watch; no window |

**Source files added to the drift checks: 20.** Besides the six modules'
files, the app's `SOURCES` lacked files that were already on disk when
their modules were classified: the Store's `readings.rs` and its eleven
submodules, `daemon/blob_sync/admission.rs`,
`settings_view/reader_link.rs` and `manage/profiles/sync_modes.rs`. They
are compiled in now, so the checks that read an app file can see them.

**Strings: 5 new keys**, each in all eight languages: the two records'
names and purposes and the option label **Above**.

**Reading depth.** `tree_drag.rs` and `quick_switch.rs` were read in full;
`quick_filter.rs`, `task_events.rs`, `landing.rs` and `cloud_reconcile.rs`
at their module documentation and declarations. The search scope chip was
read at its inspector scope and the text it paints. Changes main made
inside files already baselined were checked only as far as the drift
checks reach.

**Added to the defects list by this reconciliation:**

- The quick switch paints its status words (*Running*, *Needs you*, *New*)
  and *Current* as English literals.
- The search scope chip paints *All workspaces* and *Workspaces on/total*
  in English; this falls under the existing `koru-search-ui` line, a crate
  that does not link `koru-i18n`.

## L4b batch 1 — shared action and choice widgets

Added 16 November 2026 (ROADMAP.final.md's plan L4b, first batch).

### Shared fixes that came first

The user approved fixing the focus state in the shared widgets ("ok, fix
focus state"). Each is additive: the older entry point calls the new one
with `false`, a test in `koru-ui` holds the two to the same paint path for
path, and no inspector record gained a property.

| Widget | What was missing | What was added |
| --- | --- | --- |
| `koru_ui::segmented::Segmented` | no keyboard-focus state | `draw_focused(…, focused, alpha)`: the track's own edge in `focus_ring` |
| `koru_ui::dropdown::Dropdown` | no keyboard-focus state | `draw_focused(…, hovered, focused, alpha)`: the field's border in `focus_ring` |
| `koru_ui::icon_button` | no keyboard-focus state; an on state that was accent plate, accent ink and filled glyph together | `draw_focused(…, metrics, tone, focused)`: the chip ringed; `Tone::Quiet`: on is the filled glyph alone |

All three stroke through one crate-private `button::focus_stroke`, the
mark `Chip` and `Toggle` already drew. With them the Lab's own wrappers
went: the box round each dropdown, the box round a segmented choice, and
the element-built pane toggles in the caption, which are the shared icon
button again (`Tone::Quiet`, `Metrics::INLINE`). The search field keeps
its box: the shared search input is the next batch's.

No caller in the ordinary app passes focus yet; that was left alone on
purpose.

### The batch's records

Thirteen records, all `koru-ui`, all `Kind::Widget`. A story is a `Widget`
in `ui_lab/stories/`, listed in `stories::ALL` beside its recipe; the
window draws, hit-tests and publishes every one of them through the same
few questions (`ui_lab/stories.rs`).

| Record | Story | State |
| --- | --- | --- |
| `C-TOGGLE` | `C-TOGGLE/switch` | runs |
| `C-CHIP` | `C-CHIP/choice` | runs |
| `C-SEGMENTED` | `C-SEGMENTED/choice` | runs |
| `C-DROPDOWN` | `C-DROPDOWN/field` | runs |
| `C-SWATCH` | `C-SWATCH/choice` | runs |
| `C-COLOR-PICKER-ROW` | `C-COLOR-PICKER-ROW/row` | runs |
| `C-STEPPER` | `C-STEPPER/scale` | runs |
| `C-PREVIEW-TILE` | `C-PREVIEW-TILE/choice` | runs |
| `C-ICON-BUTTON` | `C-ICON-BUTTON/button` | runs; six narrower entry points have no story of their own (below) |
| `C-BUTTON-GROUP` | `C-BUTTON-GROUP/cluster` | runs |
| `C-SPLIT-BUTTON` | `C-SPLIT-BUTTON/split` | runs |
| `C-COPY` | `C-COPY/confirm` | runs; its timing and tooltip do not (below) |
| `C-BUTTON` | `C-BUTTON/dialog` (from the first trial) | its other symbols have no story yet |

**Left out of this batch by decision**, and still "no story yet":
`C-CHOICE-DIALOG` is a floating dialog and belongs to the floating-surfaces
batch; the display chips `C-BADGE`, `C-LABEL-CHIP` and `C-CONTEXT-CHIP`
belong to the feedback batch.

### What each story offers, and what it leaves out

Inputs are the production call's own. Sample content (three densities, the
eight languages by their own names) is synthetic and in the display
language. Hover and focus are left to the real pointer and keyboard; no
story forces them.

- **Toggle** (`Toggle::new(on).enabled(..).pending(..).icon(..)`): pressed
  to flip; *Disabled*, *Pending* and *Leading* are its three builders. The
  leading glyph is the cloud pair its first caller passes, so on and off
  differ by glyph as well as position. Disabled or pending, the story's
  switch takes no press. That rule is the story's: the production
  component has no hit test of its own and leaves it to each caller, and
  the callers were not audited for it.
- **Chip** (`Chip::new(label, selected).leading(w)`): three chips, one
  chosen by a press. *Label* is the first chip's; *Leading* reserves the
  16 px slot and the story draws a plain star in it, as a caller draws its
  own mark. `chip::wrap` stays nonvisual: it is the row arithmetic, and
  does not count a leading slot, so the story repeats it with the slot
  counted.
- **Segmented control** (`Segmented::layout` and `draw_focused`): three
  segments. One Tab stop; the arrows choose, as in Settings. Its `alpha`
  is a number the caller picks for a read-only surface and has no input.
- **Dropdown** (`Dropdown::new(value).open(..).enabled(..)`,
  `Dropdown::menu`): the closed field and its own list, which is the
  shared menu hung under the field by `Dropdown::menu`. `open` is real: it
  is set while the list shows. *Disabled* is `enabled(false)`. `in_face`
  (a font picker's per-row face) has no input: it takes a font stack, not
  a choice the type lists.
- **Swatch** (`Swatch::draw`, `Swatch::row`): four in a row, one chosen.
  The colors are sample content: none (the hollow "automatic" value), then
  the theme's own `accent`, `success` and `notice_fg`, named as the palette
  spells them. The slot keeps `Swatch::RING_REACH` clear on every side, so
  the selection and focus rings are drawn inside it. `alpha` has no input.
- **Color picker row** (`ColorPickerRow::new(colors).label(..)` with
  `selected`, `hovered`, `focused`): the same four colors. *Label* is the
  caller's text, opening as "Color"; emptied, the row is built without
  `label` and is just its swatches. `alpha` has no input.
- **Stepper** (`Stepper::layout`, `draw`): a value from 1 to 5, opening in
  the middle. `can_less` and `can_more` are real: each button is dimmed,
  and takes no press, at its end. The readout is published as a value and
  is never pressed. **The stepper draws no keyboard-focus state**, so the
  window rings the button the keyboard is on; `alpha` has no input.
- **Preview tile** (`PreviewTile::new(label, selected)`, `row`, `draw`):
  two tiles sharing a 320 px row. Each paints its picture with a built-in
  theme's ground, as the appearance settings' tiles do. **The tile has no
  keyboard-focus state** either (its border lights under the pointer
  only), so the window rings it; `alpha` has no input.
- **Icon button** (`icon_button::draw_focused`, the entry point that takes
  every input): *Tone* is `Tone` (Default, Danger, Quiet), *Size* is
  `Metrics` (STANDARD in a caption button's slot, INLINE in a 28 px
  square, COMPACT at its own measured size), *Selected* is `State::active`
  and *Disabled* `State::disabled`. The glyph is a pin. `draw`,
  `draw_toned`, `draw_with`, `draw_with_in` and the `IconButton` builder
  call the same painter with fewer inputs and stay "no story yet": there
  is no disposition that says "shown by another symbol's story", and one
  record still runs one story (see *What the machinery does not do*).
  `IconButton` is run inside the button group's story. `draw_label_with`
  (a word such as `Aa` in the button's chip) has no story yet.
  `hover_chip` is geometry and is now recorded as nonvisual.
- **Button group** (`ButtonGroup::new`, `slots_right`, `action_at`): the
  cluster the window caption's three controls are: minimize, maximize and
  a danger-toned close, each an `IconButton` whose typed action is its
  index. A press is answered by the group's own `action_at`, and the line
  under it names the button. *Disabled* sets every button's
  `State::disabled`. **`IconButton::draw` takes no focus**, so the window
  rings the button the keyboard is on.
- **Split button** (`SplitButton { .. }.draw(glyph, badge)`): *Tone* is
  `SplitTone`; *Disabled* is `action_enabled: false`, which leaves the
  menu its own target. The two halves are pressed, hovered and focused
  apart and each has its count; the menu target stays down while "its"
  menu would be open — the menu itself is the caller's and is not shown.
  **An example, not the motion:** `reveal` and `chevron_turn` are numbers
  the caller animates; the story draws each at rest or at its end.
- **Copy affordance** (`CopyAffordance::confirm`, `draw_icon`): *Text* is
  what a copy would copy. **The Lab never touches the clipboard**:
  `CopyAffordance::click` writes the system clipboard, so a press runs
  `confirm` — the half of `click` that shows the check — and the line
  under the icon says what would have been copied. **An example, not the
  timing:** in the app the check holds 0.8 s and turns back on the host's
  maintenance clock (`maintain`), and a tooltip rises on hover
  (`set_hover`, `draw_tooltip`); the Lab runs no clock, so the check holds
  until Reset and the tooltip is not shown. The affordance draws no
  keyboard-focus state, so the window rings it.

### What the machinery does not do yet

- **One story a record.** A record's page runs its first story; the
  registry's tests hold `Entry::story_id` to that. A record whose symbols
  need different stories (the Button's eight other symbols, the icon
  button's narrower entry points) needs a way to choose between recipes
  on its page before each can have one.
- **No clock.** Nothing in the Lab advances time, so a component that
  animates or times out (the copy affordance's return, a tooltip's delay,
  a split button's reveal) shows its states and not its motion.
- **A choice that is too wide is a list.** A choice is the segmented
  control while it has four options or fewer *and* they fit the properties
  column in the display language; the split button's four tones do not in
  French and are a dropdown there.

## Keeping the inventory true

The inventory is closed; these are the rules for whoever changes a UI owner
next, and for the stories that will read these records.

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
  `lib.rs` baseline in `CRATES` there. An app crate's go in `app/`, its
  files in `tests/drift/app.rs`'s `SOURCES` and its root file in `ROOTS`.
- A native screen goes in `native/<platform>.rs` with its file in
  `tests/drift/native.rs`'s `SOURCES`; a new manifest component, route,
  Gradle module, Xcode target or packaged Swift file goes in that file's
  baselines. A new Kotlin or Swift file fails nothing on its own: it is
  found by reading the platform's `ui` folder or target against `SOURCES`.
- A phone cell names its counterpart in *Reconciliation* when it says
  native partial, and its page when it says not applicable; a desktop
  record's phone cell is never `Existing`.
