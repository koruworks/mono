# UI Lab inventory — source manifest

What the registry's records were written from, how the sources were searched,
what was left out and why, and how many of each thing there are. The records
themselves are the Rust in this folder (`foundations.rs`, `controls.rs`,
`compositions.rs`, and the Button in `../registry.rs`); this page is the audit
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
| records | 120 |
| records: foundations | 58 |
| records: controls | 57 |
| records: application compositions | 5 |
| kind: token | 25 |
| kind: primitive | 23 |
| kind: widget | 53 |
| kind: support | 19 |
| token values listed | 174 |
| symbols | 318 |
| symbols with typed recipes | 2 |
| inspector identities | 37 |
| inspector identity registration sites | 39 |
| disposition: story exists | 1 |
| disposition: no story yet | 96 |
| disposition: nonvisual | 20 |
| disposition: platform-only | 0 |
| disposition: boundary | 3 |
| symbol story: available | 1 |
| symbol story: no story yet | 181 |
| symbol story: nonvisual | 119 |
| symbol story: boundary | 17 |
| A2UI: component | 6 |
| A2UI: host-styled | 1 |
| A2UI: declarable values | 9 |
| A2UI: host only | 3 |
| A2UI: not supported | 299 |
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
identity, or anything outside `koru-ui` and `koru-draw`.

## Batch 2 — domain crates (not started)

Candidate owners from the inventory reconnaissance, with cheap counts taken at
`3bd19e56`. "Identity sites" counts only the two-line
`"Name",` / `"component",` pattern `koru-ui` uses; a crate that registers
through `scope_at`, the element layer or another shape shows 0 here and still
needs reading.

| Crate | `pub mod` in `lib.rs` | Non-test `.rs` files | Identity sites | Distinct identities |
| --- | --- | --- | --- | --- |
| `koru-ai-ui` | 0 | 59 | 1 | 1 |
| `koru-agents-ui` | 1 | 4 | 0 | 0 |
| `koru-appearance-ui` | 0 | 1 | 1 | 1 |
| `koru-artifacts-ui` | 2 | 12 | 0 | 0 |
| `koru-canvas-ui` | 0 | 11 | 0 | 0 |
| `koru-chats-ui` | 3 | 9 | 0 | 0 |
| `koru-runs-ui` | 0 | 11 | 0 | 0 |
| `koru-search-ui` | 1 | 12 | 1 | 1 |
| `koru-settings-ui` | 3 | 9 | 0 | 0 |
| `koru-shell-chrome` | 10 | 11 | 1 | 1 |
| `koru-socials-ui` | 1 | 5 | 0 | 0 |
| `koru-voice-ui` | 0 | 2 | 0 | 0 |
| `koru-workspaces-ui` | 4 | 10 | 0 | 0 |
| `koru-editor` (with `src/components`) | 24 | 49 | 4 | 3 |
| `koru-markdown-view` | 1 | 4 | 0 | 0 |
| `koru-session-view` | 0 | 22 | 0 | 0 |
| `koru-app-shell` | 14 | 15 | 0 | 0 |
| `koru-workspace-host` | 31 | 77 | 7 | 7 |
| `koru-webview` | 0 | 5 | 0 | 0 |

Known records owed: `SearchModal` (`C-SEARCH-MODAL`, family `C-OVERLAY`),
`KoruBranchChip` (`C-BRANCH-CHIP`, `C-CHROME`, Android and iOS not
applicable), `EmojiList`/`Emoji` (`C-CHOICE`), the Socials navigator, the
Settings page builder, and the canvas and rich-text boundaries the inventory
names.

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
retained editor through `Leaf`), the workspace and profile switcher rows
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
  Lab must never use, and a record's string literals are part of what it
  scans. `koru_settings`, `koru_workspace::`, `spelling`,
  `maintenance_deadline`, `themes::pin`, `std::fs` and any line mentioning
  `koru_workspace_host` are refused there, so a record naming
  `koru_settings_ui::…` or `koru_workspace_host::…` fails that check as
  written. That check needs a deliberate change, reviewed, before those
  records land.
