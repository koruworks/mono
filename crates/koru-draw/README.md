# koru-draw

**Primitives: colour, type, geometry, and putting marks on a scene.**

The palette and its two themes, the type scale and the four font roles, the
Phosphor glyph table, `Rect` and the layout arithmetic, and the `scene` helpers
that turn all of that into vello draw calls. It knows vello and parley; it is
where those two libraries stop being visible to the rest of the tree.

## The rule

> **Nothing here knows what a node is.**

Enforced by the manifest: this crate does not depend on `koru-editor`, so it
*cannot* reach for a `Paragraph`. Calendar and kanban are the next two views and
neither is a text editor, but both want the palette, the glyph pipeline and the
geometry — without inheriting an editor to get them.

## Why this crate and not `koru-ui`

`koru-draw` draws *shapes*. `koru-ui` draws *things*.

`scene::floating_panel` will give you a panel with a shadow and a border; it does
not know whether that panel is a menu, a tooltip or a confirmation dialog. The
moment something starts remembering — a scroll offset, which row is hovered,
when it appeared — it has become a widget and belongs one crate up.

The dividing question is: *does it have state, or is it a function of its
arguments?* Everything here is the second kind.

A useful corollary: **nothing here should have a `mouse_down`.** Hit testing
implies a thing being hit.

## What is in it

| module | what |
| --- | --- |
| `theme.rs` | The palette (both themes), the type scale, density, the four font roles |
| `text.rs` | `TextEngine` — parley layout, the bundled font cache |
| `scene.rs` | Fills, strokes, rounded rects, clip layers, `Panel` and `floating_panel` |
| `geometry.rs` | `Rect` and the shared column arithmetic |
| `icons.rs` | Phosphor codepoints, by name, with two invariants pinned by tests |
| `image.rs`, `tile.rs`, `pill.rs` | Decoded-image cache, the workspace tile, the pill shape |
| `face.rs` | A Bot's face: 24 faces drawn from one index and a name, still unless a `FaceClock` says its Bot is working |

## Three things that will surprise you

**Every floating surface goes through `scene::floating_panel`.** `panel_shadow`
is `pub(crate)` so there is no second way. Thirteen call sites once drew the
shadow-fill-border recipe by hand and drifted apart three times.

**A `TextEngine` is per view, not per process** — one per open tab. Anything
added to `register_bundled_fonts` is paid on every tab open unless it comes from
the `text::bundled()` cache.

**Asking a static font family for a non-400 weight silently turns on faux bold.**
Which is why weights are typed and travel beside the font stack rather than being
bare numbers at the call site. See [`docs/features/design/TYPOGRAPHY.md`](../../docs/features/design/TYPOGRAPHY.md).

## Adding an icon

`icons.rs` is a hand-maintained table of Private Use Area codepoints lifted from
the pinned `@phosphor-icons/web` stylesheet (`scripts/setup/fetch-fonts.sh` pins the
version — currently 2.1.2). **Take the codepoint from that stylesheet; do not
guess.** Add the constant, add the `ALL` entry, and bump `ALL`'s length in its
type, or the two invariant tests will not cover it. There is a note about
generating this instead in [`docs/contributing/techdebt/README.md`](../../docs/contributing/techdebt/README.md) (#6).

## What has not moved in yet

`apps/koru-document/src/menu.rs` — the popover menu panel — belongs in
`koru-ui` and would move nearly free (it imports only this crate and vello). It
has not, because nothing outside `koru-document` has wanted a menu yet. See
[`docs/contributing/techdebt/README.md`](../../docs/contributing/techdebt/README.md) (#8).
