# Compiled icon packs

Desktop chrome uses named identities from `koru_draw::icons`, rendered as vector
paths. Tabler 3.31.0 is the default; Lucide 0.477.0 is available for comparison.
The 151 canonical identities and four aliases retain their existing PUA values.
Changing a pack does not change stored documents or require a schema migration.

## Regenerate or switch packs

From the repository root, with Python and Pillow installed:

```sh
python scripts/icons/sync-icon-pack.py --pack tabler
python scripts/icons/sync-icon-pack.py --pack lucide --stroke-width 1.85
cargo build -p koru -p koru-ai-chat
```

The command verifies the archives pinned in `third-party/fetch.manifest`, using
the standard fetcher if absent. It updates the compiled Rust catalog, bundled
license notices, `assets/document.ico`, and all `assets/koru-ai-chat*.ico` and
`.png` variants. Commit those outputs together. Ordinary builds use committed
paths and images and need neither Python, Pillow, archives nor an icon font.
This is a build-time choice; there is no user-facing pack setting.

`scripts/icons/icon-map.json` maps each canonical identity to both packs.
Lucide's Windows, Apple and social brand gaps use explicitly attributed Tabler
paths. Linux Tux and the Return keycap are original Koru centerline artwork.
The Return keycap means a keyboard key; ENTER remains a navigation arrow.
The licenses are preserved in the generated source and in the release's
bundled notices, including Lucide's Feather-derived MIT portions. Provider
artwork such as Google's multicolor sign-in logo uses its existing brand path.

## Layout, state and inspection

Call `TextEngine::icon_layout` or `icon_layout_filled`. Known identities produce
one tagged Parley inline box with a square slot; the shared scene painter draws
it in the caller's resolved color. Centering uses that slot rather than font
ink bounds. The painter parses each icon once through `OnceLock`; subsequent
frames reuse the paths. A filled request uses a genuine pack variant where one
exists, otherwise it adds 0.50 units to the selected stroke width. Unknown
characters retain the legacy Phosphor font fallback.

Keep the shared icon-button semantics, hit target, focus, tooltip and disabled
color. A shape alone is not an accessible control. Icons inherit the root
transform, so DPI and rotation apply once. Directional mirroring remains the
caller's documented decision.

The UI Inspector records an `icon` leaf, canonical name, stable codepoint,
requested variant, size, color, selected pack and actual artwork source. A
Tabler brand fallback in a Lucide build reports both facts. It does not invent
a rendered font family.

## Add or verify an icon

Add a documented stable PUA constant, one canonical `ALL` row and entries for
both packs in `icon-map.json`; an alternate name belongs in `ALIASES` instead.
The normalizer rejects unsupported SVG structure. It converts primitives and
inherits monochrome paints without keeping runtime XML. Extend its tests before
admitting new structure. Do not edit `icon_pack_data.rs` by hand.

```sh
python -m unittest discover -s scripts/icons -p "test_*.py"
python scripts/icons/check-icon-packs.py
cargo test -p koru-draw -p koru-ui -p koru-workspace-host -p koru-release
```

The round-trip check compiles each pack, checks notices and image sizes, compares
all 14 generated artifacts against a captured Tabler baseline, and restores the
original artifacts even on failure. It leaves unrelated edits alone.

The taskbar rasterizer is deliberately bounded to the document and sparkle
centerlines. It supports absolute and relative M/L/H/V/C/A/Z, implicit repeats,
compact numbers and subpaths. Curves are sampled within 0.25 supersampled pixels
and painted with round caps/joins at 4x resolution. Unsupported geometry fails
regeneration instead of silently dropping part of an icon.
