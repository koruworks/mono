# parley

## Non-text rows must bypass the text accessibility exporter

An image or divider receives a geometry/shaping stamp in `build_row`, but that
function returns before shaping text. Its `Layout` is therefore default or left
from an earlier text row. Passing it to `LayoutAccessibility::build_nodes` with
`shaped_text(row)` supplies unrelated byte ranges. The 2026-09-13 image insertion
crash reached Parley 0.11.1 `accessibility.rs:263`: byte index 1 was inside the
three-byte U+200B layout strut. It occurred in normal accessibility publication
while the UI inspector was open, not in the inspector's geometry collector.

`DocumentAccessibility::project` now clears cached text evidence for these rows
and skips Parley's text exporter. Body publication uses `RowText::Absent`, with
no text selection pointing to nonexistent runs. Geometry and row identity remain
available. The regression beside projection uses in-memory image/divider rows
with a geometry stamp and no fonts, image decoding, filesystem or store.

Inspector weight/style come from `Run::font_attrs()` on the shaped run, rather
than interpreting normalized variation coordinates as numeric font weights.
The face's metadata remains separately labeled because variable axes and
synthetic italic can change the rendered result.

Text layout: turning a string plus styles into positioned glyphs. `parley` 0.11.

New to Rust syntax? [RUST.md](../RUST.md) first.

---

## What "text layout" actually involves

Drawing text is not drawing characters. Between a `String` and pixels sits work
most people never think about:

- **Shaping** — which *glyph* does each character become? In many scripts that
  depends on neighbours: Arabic letters join, `fi` may become one glyph.
- **Font fallback** — the font has no Hangul. Which font does?
- **Bidirectional ordering** — Hebrew inside English runs the other way.
- **Line breaking** — where may a line wrap? Not simply "at spaces"; Korean and
  Thai have their own rules.
- **Cluster boundaries** — one arrow key should step over `é` even when it is
  stored as `e` + combining accent.

parley does all of it. Koru would not otherwise render `보상 약관` and
`Проверить условия` in the same line as Latin text without either shipping every
font or getting it subtly wrong.

## Layout, and the one thing parley does not do

You build a `Layout`, then read positioned glyphs out of it:

```rust
// crates/koru-editor/src/rowlayout.rs
let mut b = layout_cx.ranged_builder(font_cx, &row.text, 1.0, true);
b.push_default(StyleProperty::FontSize(theme::FONT_SIZE));
b.push(StyleProperty::FontWeight(FontWeight::BOLD), 0..6);   // a range
b.build_into(&mut row.layout, &row.text);
row.layout.break_all_lines(Some(available_width));
row.layout.align(Alignment::Center, AlignmentOptions::default());
```

`push_default` applies to everything; `push` applies to a byte range — which is
exactly how a `TextRun` from Loro becomes a style.

**parley never rasterises anything.** It computes geometry; drawing is the
caller's job. That includes underlines and strikethroughs: parley works out
where the rect goes, `render.rs` fills it. Miss that and decorations silently do
not appear.

## Inline boxes — why pills exist at all

`RangedBuilder::push_inline_box` reserves a rectangle in the text flow. The text
wraps around it, and afterwards you ask where it ended up:

```rust
b.push_inline_box(InlineBox {
    id: attr_box_id(attr),      // your own id, to match it up later
    kind: InlineBoxKind::InFlow,
    index: run.start.get(),     // byte offset
    width, height,
});
```

Two things about this shaped the whole editor:

- **`PlainEditor` cannot reach it.** parley ships a ready-made editor, and its
  surface is `set_text` / `driver()` / `layout()` — no inline boxes. Pills are
  therefore impossible through it, which is why this editor owns its own buffer
  and builds each `Layout` itself
  (ADR-14). `PlainEditor`
  is still used for the single-line prompt fields, where it fits.
- **parley puts a box's *bottom* on the baseline** — CSS `vertical-align:
  baseline` for an inline-block. Position a pill from `bx.y` and its label
  floats above the surrounding text. `rowlayout::visit_pills` derives the top
  from the line's baseline instead.

An inline box also needs a real character behind it, or the cursor cannot reach
it — hence the `U+2060` anchor
(ADR-9).

**A zero-width inline box still sets the line's ascent — and only the ascent.**
Parley puts a box's bottom on the baseline, so its full height becomes ascent
and it never contributes descent. Under absolute line-height the baseline is
`½(line_height + ascent − descent)`; a box-only strut therefore leaves baseline
at the mercy of whichever fallback font supplied descent. Colour-emoji-only
lines then jump when the first Latin letter introduces the body font.

Body rows use both:

1. A zero-width **inline box** the height of the line box — full-height empty
   carets and the same baseline as the title placeholder.
2. A **layout-only trailing U+200B** shaped in the primary document font (not
   stored in `row.text`) — primary ascent *and* descent on every line, so
   emoji-only and mixed lines share one baseline. Transparent; selection
   indices clamp to `text.len()`.

`TextEngine::placeholder_layout` keeps the box strut alone (the hint is already
primary-font text).

## Cursor and selection come free

The part that makes owning the buffer affordable: `parley::editing::{Cursor,
Selection}` work on a `Layout`, not on `PlainEditor`.

```rust
// crates/koru-editor/src/editor.rs
row.selection = Selection::from_point(&row.layout, lx, ly);        // hit-test
row.selection = sel.next_visual_word(&row.layout, extend);         // word motion
row.selection = row.selection.refresh(&row.layout);                // after an edit
```

So cursor movement, word and line navigation, hit-testing and selection geometry
are all parley's. What we write is a buffer and an event loop, not a text engine.

Two consequences worth knowing:

- **Word and cluster boundaries both come from parley**, so a pill anchor is one
  unit either way — one arrow key steps over it, one backspace deletes it, with
  no special-casing in the key map.
- `Selection::geometry_with` yields the rects for a range *including wrapping*,
  which is how the highlight background is drawn without `render.rs` knowing
  anything about lines.

## Font fallback is a list, resolved per character

```rust
// crates/koru-draw/src/theme.rs
"Inter, system-ui, sans-serif, Noto Sans KR"
```

parley resolves this **per character**, not per string. Trailing Noto Sans KR
gives Hangul and Cyrillic coverage the Latin faces lack, without affecting a
single Latin glyph — which is why one stack renders a mixed-script line.

Registering a font under a stable name:

```rust
// crates/koru-draw/src/text.rs
font_cx.collection.register_fonts(blob, Some(FontInfoOverride {
    family_name: Some(family), …
}));
```

Known desktop icons instead use tagged inline boxes on an empty string. The
square slot carries the stable PUA identity and filled-state bit; the shared
scene painter resolves the compiled vector path. No font shaping is involved.
The icon tag mask excludes the filled bit, and the painter ignores unrelated
inline boxes. Optical centering includes the icon slot. Unknown icon characters
retain the legacy Phosphor fallback. See [Compiled icons](../ICONS.md) for
regeneration and the fontless tests.

## Synthesis: faux bold and faux italic

Ask for italic from a font that has no italic face and parley does not give up —
it reports a *synthesis*: a skew, or a stroke width to fake weight. Applying it
is the caller's job:

```rust
// crates/koru-editor/src/draw.rs
let skew = synthesis.skew().map(|deg| Affine::skew(-(deg.to_radians().tan()), 0.0));
```

Negative because glyph tops sit at negative y.

**This is the path you fall into by accident.** `FontInfo::synthesis` gives a
real axis position only when the face *has* one — otherwise a heavier request
becomes `embolden` (a stroked second pass over the fill) and an italic request
becomes a 14° skew. Neither is an error and nothing warns. Every bundled family
therefore registers **two files**, roman and italic, under one family name; the
system faces `FontChoice::Mono` resolves to have neither axis, which is why it
asks for weight 400 rather than the 500 everything else uses.

## Styles used here

| | |
| --- | --- |
| `FontFamily` / `FontSize` / `FontWeight` / `FontStyle` | the obvious ones |
| `FontVariations` | variable-font axes by tag, e.g. `opsz` |
| `Brush` | colour. **Baked into the layout**, so a theme flip invalidates every one |
| `Underline` + `UnderlineBrush` | geometry only; you fill the rect |
| `Strikethrough` + `StrikethroughBrush` | same |
| `LineHeight` | `FontSizeRelative(1.5)` here |
| `FontFeatures` | OpenType features by tag — `"sups"` for superscript |

**There is no baseline-shift property.** Superscript is therefore
`FontFeatures("sups")`, asking the font for its own designed glyphs — better
typography than scaling and nudging, and a font without the feature renders
normally rather than breaking.

**Synthesis only drives `wght`, `wdth` and `ital`.** Any other axis a variable
font carries sits at its default however the style is described, so `opsz` has
to be pushed by hand:

```rust
// crates/koru-draw/src/text.rs — push_axes
b.push_default(StyleProperty::FontVariations(FontVariations::List(
    Cow::Owned(vec![FontVariation::new(Tag::new(b"opsz"), size)]),
)));
```

Safe unconditionally: the shaper clamps an out-of-range value, and a font
without the axis ignores it. `Tag` is `parley::setting::Tag`, not
`parley::style::Tag`.

## Newlines

parley treats `\n` as a **mandatory break inside one `Layout`**, and
`Cursor`/`Selection` already understand `BreakReason::Explicit`. That is what
makes the planned multi-line code block one `Row` with a taller layout rather
than a second rendering path.

Today no text node contains a newline — one line per node
(ADR-6) — so every layout is one
logical line that may *wrap* into several visual ones.

## One layout per row

Not one for the whole document. Per-row depth, checkbox and alignment become
trivial, and a keystroke re-lays-out one short line rather than everything.

Parley's selection is confined to one layout. Koru must coordinate selections
that span multiple rows.

## `peniko`, and why glyphs need no conversion

`parley::FontData` *is* `peniko::FontData` — both re-export
`linebender_resource_handle`. A glyph run comes out of parley and goes into vello
unchanged. `peniko` is a shared vocabulary crate, not a third rendering library.

## It also ships an AccessKit projection, and we use it

The `accesskit` feature (on in the workspace manifest) adds
`LayoutAccessibility::build_nodes`, which turns a `Layout` into AccessKit text
runs — one per style span per line, bidi-ordered, with per-character geometry,
word starts, font attributes and **stable node ids across passes**. It depends
on `accesskit 0.24`, the version already pinned, so no second accesskit arrives.

Two things to know before using it:

- **It slices the string it was given**, which for a Koru row is
  `rowlayout::shaped_text` — `row.text` *plus the layout strut* — so the strut
  lands in the last run and has to be taken back out.
- **Its clusters are shaping clusters, not grapheme clusters.** `a👍🏻e\u{301}b`
  comes out as `1, 4, 4, 1, 2, 1` bytes, while Koru's Left/Right step
  `1, 8, 3, 1`. That is why `Cursor::to_access_position` and
  `Selection::from_access_selection` are **not** used, even though they exist
  and look exactly right.

Both, with the reasons, are in [ACCESSKIT.md](ACCESSKIT.md).

## Where to look

- `crates/koru-editor/src/rowlayout.rs` — building a layout from marks
- `crates/koru-draw/src/text.rs` — font registration, measuring
- `crates/koru-editor/src/draw.rs` — glyph runs, decorations, synthesis
- `crates/koru-editor/src/accessibility/` — the layout as a screen reader sees it
- [parley docs](https://docs.rs/parley) — upstream
