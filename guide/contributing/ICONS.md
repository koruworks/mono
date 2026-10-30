# Compiled icon packs

Desktop chrome uses named identities from `koru_draw::icons`, rendered as vector
paths. Phosphor 2.1.2, Tabler 3.31.0 and Lucide 0.477.0 are bundled together.
Settings → Appearance → Interface → Icon style selects the in-app pack live;
Phosphor is the initial preference. The roaming `icon_pack` key uses the
existing typed Global settings patch and `preferences.conf` format, with no
version bump or migration. Old/missing values fall back to Phosphor.
The 151 canonical identities and four aliases retain their existing PUA values.
Changing a pack does not change stored documents or require a schema migration.

## Regenerate artwork

From the repository root, with Python and Pillow installed:

```sh
python scripts/icons/sync-icon-pack.py --pack tabler
python scripts/icons/sync-icon-pack.py --pack lucide --stroke-width 1.85
python scripts/icons/sync-icon-pack.py --pack phosphor
cargo build -p koru -p koru-ai-chat
```

The command verifies the archives pinned in `third-party/fetch.manifest`, using
the standard fetcher if absent. It updates the compiled Rust catalog, bundled
license notices, `assets/document.ico`, and all `assets/koru-ai-chat*.ico` and
`.png` variants. Commit those outputs together. Ordinary builds use committed
paths and images and need neither Python, Pillow, archives nor an icon font.
Every regeneration compiles all three packs. `--pack` selects the packaged
Windows document/taskbar assets and the renderer's bootstrap choice; saved
settings take precedence for in-app icons. OS assets remain build-time artwork.

Regeneration additionally requires `fonttools` and the regular/fill
fonts from the standard font fetcher. Their hashes are pinned in
`scripts/setup/fonts.manifest`. The generator exports the original glyph
outlines into the same 24-unit slots; the app still renders compiled paths
without needing a font. Phosphor uses its original regular and filled weights,
so `--stroke-width` does not change its outlines.

`scripts/icons/icon-map.json` maps each canonical identity to both packs.
Lucide's Windows, Apple and social brand gaps use explicitly attributed Tabler
paths. In Tabler/Lucide, Linux Tux and the Return keycap are original Koru centerline artwork.
The Return keycap means a keyboard key; ENTER remains a navigation arrow.
The licenses are preserved in the generated source and in the release's
bundled notices, including Lucide's Feather-derived MIT portions. Provider
artwork such as Google's multicolor sign-in logo uses its existing brand path.

## Layout, state and inspection

Call `TextEngine::icon_layout` or `icon_layout_filled`. Known identities produce
one tagged Parley inline box with a square slot; the shared scene painter draws
it in the caller's resolved color. Centering uses that slot rather than font
ink bounds. The painter caches each (pack, icon) separately through `OnceLock`;
subsequent frames reuse the paths. Selection is resolved at paint time, so even
an existing layout switches without reshaping or document reflow. A filled request uses a genuine pack variant where one
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
artwork. It supports absolute and relative M/L/H/V/C/A/Z, implicit repeats,
compact numbers and subpaths. Curves are sampled within 0.25 supersampled pixels
and painted with round caps/joins at 4x resolution. Unsupported geometry fails
regeneration instead of silently dropping part of an icon.
Phosphor quadratic curves are converted to cubics during export. Its filled
outlines use nonzero winding scan conversion so interior holes stay transparent.
