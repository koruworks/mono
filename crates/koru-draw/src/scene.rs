// SPDX-License-Identifier: MPL-2.0

//! Putting marks on a vello `Scene`.
//!
//! The window layer: rectangles, glyph runs, decorations, and the status bar
//! every view shares. **Nothing here knows what a node is** — that is the
//! property that makes it reusable by a calendar or a kanban, neither of which
//! is a text editor, and it is why this is its own module rather than part of
//! the editor.
//!
//! The claim is load-bearing, so keep it literally true: if something here needs
//! a `Paragraph` or an `Editor`, it belongs one layer up. The status bar takes
//! two strings for exactly this reason.
//!
//! Everything is in logical pixels; `root` carries the DPI scale, and every draw
//! call is transformed by it. parley hands over glyph positions and decoration
//! *geometry* — it never rasterizes anything, so filling the underline and
//! strikethrough rects is the caller's job.

use parley::Layout;
use parley::editing::{Cursor, Selection};
use parley::layout::{Affinity, Glyph as PGlyph, PositionedLayoutItem};
use peniko::{Color, Fill, StyleRef};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Rect, RoundedRect, Stroke};

use crate::text::TextEngine;
use crate::theme::{self, Brush};

/// Paint a raster image and record the rectangle actually transformed by Vello.
/// Pixel bytes stay in the renderer; inspection contains dimensions only.
#[track_caller]
pub fn draw_image(scene: &mut Scene, transform: Affine, image: &peniko::ImageBrush) {
    if crate::inspect::enabled() {
        crate::inspect::leaf(
            "Image",
            transform,
            Rect::new(
                0.0,
                0.0,
                image.image.width as f64,
                image.image.height as f64,
            ),
            |record| {
                record.property(
                    "raster width",
                    image.image.width,
                    "decoded cached tier; physical pixels",
                );
                record.property(
                    "raster height",
                    image.image.height,
                    "decoded cached tier; physical pixels",
                );
            },
        );
    }
    scene.draw_image(image, transform);
}

/// Fill the rects a byte range of `layout` covers, offset by (`ox`, `oy`).
///
/// Geometry comes from parley's `Selection`, which already turns a byte range
/// into rects that follow line wrapping — a highlight spanning a break is two
/// rects, both correct. Drawn before the glyphs so the text sits on top.
pub fn fill_text_range(
    scene: &mut Scene,
    root: Affine,
    layout: &Layout<Brush>,
    range: std::ops::Range<usize>,
    ox: f32,
    oy: f32,
    color: Color,
) {
    if range.start >= range.end {
        return;
    }
    let selection = Selection::from_byte_index(layout, range.start, Affinity::Downstream).extend(
        Cursor::from_byte_index(layout, range.end, Affinity::Upstream),
    );
    selection.geometry_with(layout, |bb, _| {
        let r = Rect::new(
            bb.x0 + ox as f64,
            bb.y0 + oy as f64,
            bb.x1 + ox as f64,
            bb.y1 + oy as f64,
        );
        scene.fill(Fill::NonZero, root, color, None, &r);
    });
}

#[track_caller]
pub fn fill(scene: &mut Scene, root: Affine, color: Color, shape: &impl vello::kurbo::Shape) {
    if crate::inspect::enabled() && color.components[3] > 0.0 {
        crate::inspect::leaf("fill", root, shape.bounding_box(), |record| {
            record.property(
                "color",
                crate::inspect::color(color),
                "resolved paint; token origin not exposed",
            );
        });
    }
    scene.fill(Fill::NonZero, root, color, None, shape);
}

/// Fill a caret parallelogram (upright or italic-slanted).
pub fn fill_caret_quad(scene: &mut Scene, root: Affine, color: Color, quad: [(f64, f64); 4]) {
    let mut path = BezPath::new();
    path.move_to(quad[0]);
    path.line_to(quad[1]);
    path.line_to(quad[2]);
    path.line_to(quad[3]);
    path.close_path();
    fill(scene, root, color, &path);
}

#[track_caller]
pub fn stroke(
    scene: &mut Scene,
    root: Affine,
    color: Color,
    width: f64,
    shape: &impl vello::kurbo::Shape,
) {
    // Recorded like a fill, so a hairline is evidence too: a panel's border
    // and a badge's outline were the paints the inspector could not see.
    if crate::inspect::enabled() && color.components[3] > 0.0 {
        crate::inspect::leaf("stroke", root, shape.bounding_box(), |record| {
            record.property(
                "color",
                crate::inspect::color(color),
                "resolved paint; token origin not exposed",
            );
            record.property("width", width, "scene::stroke");
        });
    }
    scene.stroke(&Stroke::new(width), root, color, None, shape);
}

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::new(x as f64, y as f64, (x + w) as f64, (y + h) as f64)
}

pub fn rrect(x: f32, y: f32, w: f32, h: f32, r: f32) -> RoundedRect {
    rect(x, y, w, h).to_rounded_rect(r as f64)
}

/// A circle from its centre, in the same `f32` units the rest of this module
/// takes — the switch knob and the Cloud card's status dot, which would
/// otherwise each carry their own `as f64` casts at the call site.
pub fn circle(cx: f32, cy: f32, r: f32) -> vello::kurbo::Circle {
    vello::kurbo::Circle::new((cx as f64, cy as f64), r as f64)
}

/// Draw a standalone layout's glyphs (no inline boxes), optionally recoloring.
#[track_caller]
pub fn draw_layout(
    scene: &mut Scene,
    root: Affine,
    layout: &Layout<Brush>,
    ox: f32,
    oy: f32,
    color: Option<Color>,
) {
    for line in layout.lines() {
        let lm = line.metrics();
        let _inspection_line = crate::inspect::scope(
            "text line",
            "part",
            root,
            rect(
                ox + lm.offset,
                oy + lm.baseline - lm.ascent - lm.leading * 0.5,
                lm.advance,
                lm.line_height,
            ),
            |r| {
                r.property("line height", lm.line_height, "actual Parley line box");
                r.property("baseline", oy + lm.baseline, "local layout coordinates");
                r.property("ink bounds", "Not exposed", "line box is not glyph ink");
            },
        );
        draw_line_items(scene, root, &line, ox, oy, color);
    }
}

/// Paint one laid-out line's glyph runs and decorations at (`ox`, `oy`).
///
/// The scope-free half of line painting: [`draw_layout`] and
/// [`draw_layout_first_line`] each record their own line box and then call
/// here, and chrome that records a typography scope of its own (koru-ui's
/// single-line paint) draws through it too — the glyphs under a line are
/// painted one way only.
pub fn draw_line_items(
    scene: &mut Scene,
    root: Affine,
    line: &parley::layout::Line<'_, Brush>,
    ox: f32,
    oy: f32,
    color: Option<Color>,
) {
    for item in line.items() {
        if let PositionedLayoutItem::GlyphRun(gr) = item {
            let c = color.unwrap_or(gr.style().brush.0);
            draw_glyph_run(scene, root, &gr, ox, oy, c);
            draw_decorations(scene, root, &gr, ox, oy);
        } else if let PositionedLayoutItem::InlineBox(b) = item {
            if let Some((icon, filled)) = crate::icons::unpack_box_id(b.id) {
                crate::icons::draw_vector_icon(
                    scene,
                    root,
                    icon,
                    filled,
                    crate::geometry::Rect::new(ox + b.x, oy + b.y, b.width, b.height),
                    color.unwrap_or(crate::theme::p().text),
                );
            }
        }
    }
}

/// A launch product's mark, fitted to the square of `r`'s shorter side and
/// centred — one colour, the same tinting contract as a font glyph.
///
/// The one draw path for [`BrandIcon`](crate::brand::BrandIcon)s, so the `+`
/// menu's rows and the tab strip cannot scale or place the same mark two
/// ways. Path data that failed to parse draws nothing; the brand tests keep
/// that from ever being a shipped mark.
pub fn draw_brand_icon(
    scene: &mut Scene,
    root: Affine,
    r: crate::geometry::Rect,
    color: Color,
    icon: &crate::brand::BrandIcon,
) {
    let Some(path) = icon.bez() else { return };
    let side = r.w.min(r.h) as f64;
    if side <= 0.0 {
        return;
    }
    let transform = Affine::translate((
        r.x as f64 + (r.w as f64 - side) * 0.5,
        r.y as f64 + (r.h as f64 - side) * 0.5,
    )) * Affine::scale(side / crate::brand::VIEW_BOX);
    scene.fill(Fill::NonZero, root * transform, color, None, path);
}

/// Three dots in a row, centred in a rect.
///
/// Drawn rather than set, like the drag grip's six: Phosphor's `ph-dots-three`
/// is not in the generated [`icons`](crate::icons) table, and `docs/features/design/DESIGN.md`
/// forbids typing a codepoint in by hand. Shared because both the title row's
/// page-settings button and the selection bubble's overflow are this shape.
pub fn dots_three(scene: &mut Scene, root: Affine, r: crate::geometry::Rect, color: Color) {
    let cy = (r.y + r.h * 0.5) as f64;
    let cx = r.x + r.w * 0.5;
    for n in -1..=1 {
        fill(
            scene,
            root,
            color,
            &vello::kurbo::Circle::new(((cx + n as f32 * 4.5) as f64, cy), 1.6),
        );
    }
}

/// Cell `n` of a row of evenly spaced, equally sized cells.
///
/// The arithmetic behind every swatch strip in the app — the block menu's
/// highlight colours, the workspace picker's palette. Trivial, and worth having
/// once: each of those has a hit-test *and* a draw, and the way they go wrong is
/// one of the pair rounding differently from the other.
///
/// Where the strip starts is the caller's business, because they do not agree:
/// the menu's is right-aligned in its row, the picker's is left-aligned under a
/// heading.
pub fn cell_rect(x0: f32, y: f32, size: f32, gap: f32, n: usize) -> crate::geometry::Rect {
    crate::geometry::Rect::new(x0 + n as f32 * (size + gap), y, size, size)
}

/// How wide a strip of `count` cells is, so a caller can right-align one.
pub fn cells_width(count: usize, size: f32, gap: f32) -> f32 {
    count as f32 * size + count.saturating_sub(1) as f32 * gap
}

/// Which cell `x` is in, if any. The hit-test half of [`cell_rect`], sharing its
/// arithmetic rather than repeating it.
pub fn cell_at(x0: f32, size: f32, gap: f32, count: usize, x: f32) -> Option<usize> {
    (0..count).find(|&n| {
        let r = cell_rect(x0, 0.0, size, gap, n);
        x >= r.x && x < r.x + r.w
    })
}

/// The line box height of a layout's first line, or zero when it has none.
///
/// Every piece of chrome that centres a label in a row needs this, and each of
/// them had written the `lines().next().map_or(..)` dance out again — including
/// the three that then rounded it differently.
pub fn first_line_height(layout: &Layout<Brush>) -> f32 {
    layout
        .lines()
        .next()
        .map_or(0.0, |l| l.metrics().line_height)
}

/// Where to draw a one-line layout so it is **optically** centred in `r`.
///
/// Returns the top-left to hand [`draw_layout`], with the x centred too.
///
/// **Centred on the ink** — where the pixels land — rather than on any box the
/// font describes. Two wrong answers came before it, and the second is the
/// instructive one:
///
/// 1. `(r.h - layout.height()) * 0.5`, which centres the *line box*. A line is
///    much taller above the baseline than below, and carries leading besides,
///    so this centres the empty space.
/// 2. `(ascent - descent) * 0.5`, which centres the span a line **reserves**.
///    Right for Latin text to a hundredth of a pixel — and wrong for a colour
///    emoji by about 8% of its size, because an emoji's ink does not fill the
///    box its metrics claim. At 20 px, Segoe UI Emoji reports ascent 14.6 and
///    descent 4.2, while its ink runs from 3.5 *below* the baseline to 17.1
///    above. That gap is what read as "more space below than above".
///
/// So [`ink_centre`] asks the font where the glyph actually is, and the metrics
/// are only the fallback for a face that will not say.
pub fn centre_in(layout: &Layout<Brush>, r: crate::geometry::Rect) -> (f32, f32) {
    // The ink, when the font will say where it is.
    if let Some((cx, cy)) = ink_centre(layout) {
        return (r.x + r.w * 0.5 - cx, r.y + r.h * 0.5 - cy);
    }
    // Otherwise the font's own metrics, which for ordinary text land within a
    // hundredth of a pixel of the ink — measured, not assumed. See the tests.
    let x = r.x + (r.w - layout.width()) * 0.5;
    let Some(m) = layout.lines().next().map(|l| *l.metrics()) else {
        return (x, r.y);
    };
    let baseline = r.y + r.h * 0.5 + (m.ascent - m.descent) * 0.5;
    (x, baseline - m.baseline)
}

/// Where the drawn pixels actually centre, in the layout's own coordinates.
///
/// `None` when no glyph will report a bounding box — a font with no outlines to
/// measure, or an empty layout — and then [`centre_in`] falls back to metrics.
///
/// **Why this is worth asking the font.** Ascent and descent describe the box a
/// line reserves, not the box a glyph fills, and for a colour emoji the two are
/// very different: at 20 px, Segoe UI Emoji reports ascent 14.6 and descent 4.2
/// — a span centred 5.2 above the baseline — while the ink runs from 3.5 *below*
/// the baseline to 17.1 above, centred 6.8. Centring by metrics therefore sits
/// an emoji about 8% of its size too high, which is exactly the "more padding
/// below than above" this replaced. For Latin text the two agree to a hundredth
/// of a pixel, which is why only emoji ever looked wrong.
///
/// One `FontRef::from_index` per run, which is a table-directory parse rather
/// than a font load. A tile is a handful of glyphs; do not reach for this to
/// centre a paragraph.
fn ink_centre(layout: &Layout<Brush>) -> Option<(f32, f32)> {
    use skrifa::MetadataProvider;
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for line in layout.lines() {
        for item in line.items() {
            if let PositionedLayoutItem::InlineBox(b) = &item {
                if crate::icons::unpack_box_id(b.id)
                    .is_some_and(|(icon, _)| crate::icons::has_vector_icon(icon))
                {
                    x0 = x0.min(b.x);
                    x1 = x1.max(b.x + b.width);
                    y0 = y0.min(b.y);
                    y1 = y1.max(b.y + b.height);
                }
            }
            let PositionedLayoutItem::GlyphRun(gr) = item else {
                continue;
            };
            let run = gr.run();
            let font = run.font();
            let Ok(font_ref) = skrifa::FontRef::from_index(font.data.as_ref(), font.index) else {
                continue;
            };
            let metrics = font_ref.glyph_metrics(
                skrifa::instance::Size::new(run.font_size()),
                skrifa::instance::LocationRef::default(),
            );
            for g in gr.positioned_glyphs() {
                let Some(b) = metrics.bounds(skrifa::GlyphId::new(g.id as u32)) else {
                    continue;
                };
                // skrifa measures y **up** from the baseline; the scene's y runs
                // down. `positioned_glyphs` has already folded in the baseline.
                x0 = x0.min(g.x + b.x_min);
                x1 = x1.max(g.x + b.x_max);
                y0 = y0.min(g.y - b.y_max);
                y1 = y1.max(g.y - b.y_min);
            }
        }
    }
    (x0 <= x1 && y0 <= y1).then(|| ((x0 + x1) * 0.5, (y0 + y1) * 0.5))
}

/// The same, for a caller that only wants the y — a label whose x is decided by
/// a column rather than by centring.
pub fn centre_y_in(layout: &Layout<Brush>, r: crate::geometry::Rect) -> f32 {
    centre_in(layout, r).1
}

/// Draw only a layout's first line.
///
/// How chrome truncates. A label laid out wrapped at the width available and
/// then drawn one line deep breaks at a word boundary for free, which is both
/// cheaper and better-looking than binary-searching for a character to cut at.
/// A tab or a tree row that will not fit simply shows as much as it can.
#[track_caller]
pub fn draw_layout_first_line(
    scene: &mut Scene,
    root: Affine,
    layout: &Layout<Brush>,
    ox: f32,
    oy: f32,
    color: Option<Color>,
) {
    let Some(line) = layout.lines().next() else {
        return;
    };
    let lm = line.metrics();
    let _inspection_line = crate::inspect::scope(
        "label line",
        "part",
        root,
        rect(
            ox + lm.offset,
            oy + lm.baseline - lm.ascent - lm.leading * 0.5,
            lm.advance,
            lm.line_height,
        ),
        |r| {
            r.property(
                "line height",
                lm.line_height,
                "actual Parley line box; first-line draw",
            );
            r.property("baseline", oy + lm.baseline, "local layout coordinates");
            r.property("ink bounds", "Not exposed", "line box is not glyph ink");
        },
    );
    draw_line_items(scene, root, &line, ox, oy, color);
}

#[track_caller]
pub fn draw_glyph_run(
    scene: &mut Scene,
    root: Affine,
    gr: &parley::layout::GlyphRun<'_, Brush>,
    ox: f32,
    oy: f32,
    color: Color,
) {
    if color.components[3] <= 0.0 {
        return; // fully transparent: the pill anchor character
    }
    if crate::inspect::enabled() {
        crate::inspect::text_run(root, gr, ox, oy, color);
    }
    let synthesis = gr.run().synthesis();
    let base = root * Affine::translate((ox as f64, oy as f64));

    // When the requested style has no real face, parley asks for a faux one
    // rather than silently ignoring it. Every bundled family registers its
    // italic file, so this fires for the *system* faces `FontChoice::Mono`
    // resolves to, and for a bundled family whose italic was not fetched. The
    // skew is a per-glyph transform; glyph tops sit at negative y, so a negative
    // coefficient is what leans them to the right.
    let skew = synthesis
        .skew()
        .map(|degrees| Affine::skew(-(degrees.to_radians().tan() as f64), 0.0));

    glyph_pass(scene, gr, base, skew, color, Fill::NonZero.into());

    // Faux bold: stroke over the fill so glyphs thicken instead of turning into
    // outlines. Variable fonts get real weight via `normalized_coords`, so this
    // only fires for a static face with no bold sibling.
    if synthesis.embolden() {
        let pen = Stroke::new((gr.run().font_size() * 0.03) as f64);
        glyph_pass(scene, gr, base, skew, color, (&pen).into());
    }
}

/// One `draw_glyphs` pass over a run. Split out so faux bold can run the same
/// geometry twice with a different paint style.
pub fn glyph_pass(
    scene: &mut Scene,
    gr: &parley::layout::GlyphRun<'_, Brush>,
    base: Affine,
    skew: Option<Affine>,
    color: Color,
    style: StyleRef<'_>,
) {
    let run = gr.run();
    scene
        .draw_glyphs(run.font())
        .transform(base)
        .glyph_transform(skew)
        .font_size(run.font_size())
        .normalized_coords(run.normalized_coords())
        .brush(color)
        .hint(true)
        .draw(
            style,
            // `positioned_glyphs` already folds in the run offset and baseline.
            gr.positioned_glyphs().map(|g: PGlyph| vello::Glyph {
                id: g.id,
                x: g.x,
                y: g.y,
            }),
        );
}

/// parley computes decoration offset and thickness; drawing them is ours.
pub fn draw_decorations(
    scene: &mut Scene,
    root: Affine,
    gr: &parley::layout::GlyphRun<'_, Brush>,
    ox: f32,
    oy: f32,
) {
    let style = gr.style();
    let metrics = gr.run().metrics();
    let x0 = ox + gr.offset();
    let x1 = x0 + gr.advance();

    let mut band = |offset: f32, size: f32, color: Color| {
        // `offset` is measured up from the baseline to the decoration's top.
        let top = oy + gr.baseline() - offset;
        fill(scene, root, color, &rect(x0, top, x1 - x0, size.max(1.0)));
    };

    if let Some(d) = &style.underline {
        band(
            d.offset.unwrap_or(metrics.underline_offset),
            d.size.unwrap_or(metrics.underline_size),
            d.brush.0,
        );
    }
    if let Some(d) = &style.strikethrough {
        band(
            d.offset.unwrap_or(metrics.strikethrough_offset),
            d.size.unwrap_or(metrics.strikethrough_size),
            d.brush.0,
        );
    }
}

/// A wavy underline from `x0` to `x1`, with its crests touching `top` — the
/// spelling squiggle (spell-check plan D10).
///
/// A stroked sine-ish path built from quadratic arcs rather than parley's
/// straight decoration band, because the wave *is* the meaning: a straight
/// red line under a word reads as emphasis, the wave reads as "look again".
/// The period is fixed in logical pixels, not glyph-derived — every squiggle
/// on a page waves at the same frequency, and the phase starts at `x0` so a
/// word re-checked in place never appears to crawl.
pub fn squiggle(scene: &mut Scene, root: Affine, color: Color, x0: f32, x1: f32, top: f32) {
    const HALF_PERIOD: f64 = 3.0;
    const AMPLITUDE: f64 = 1.4;
    const WIDTH: f64 = 1.1;
    if x1 <= x0 {
        return;
    }
    let (x0, x1, top) = (x0 as f64, x1 as f64, top as f64 + AMPLITUDE / 2.0);
    let mut path = vello::kurbo::BezPath::new();
    path.move_to((x0, top));
    let mut x = x0;
    let mut down = true;
    while x < x1 {
        let next = (x + HALF_PERIOD).min(x1);
        // A shortened final arc keeps its slope rather than flattening: the
        // control point stays at the half-period midpoint's height.
        let mid_y = if down {
            top + AMPLITUDE
        } else {
            top - AMPLITUDE
        };
        path.quad_to(((x + next) / 2.0, mid_y), (next, top));
        down = !down;
        x = next;
    }
    stroke(scene, root, color, WIDTH, &path);
}

/// The footer: two strings, one at each end.
///
/// It takes the strings rather than the view that knows them, which is what
/// keeps this file free of nodes — the caller composes what its view has to say
/// and this puts it on screen. Both views use it; only the words differ.
pub fn draw_status_bar(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    viewport: (f32, f32),
    left: &str,
    right: &str,
) {
    let (vw, vh) = viewport;
    let h = theme::STATUS_H;
    let y = vh - h;

    fill(scene, root, theme::p().status_bg, &rect(0.0, y, vw, h));
    // Hairline along the top edge to separate it from the document.
    fill(scene, root, theme::p().panel_border, &rect(0.0, y, vw, 1.0));

    let l = engine.simple_layout(left, theme::role::hint());
    let ly = y + (h - l.height()) * 0.5;
    draw_layout(
        scene,
        root,
        &l,
        theme::MARGIN_X,
        ly,
        Some(theme::p().row_hint),
    );

    let r = engine.simple_layout(right, theme::role::hint());
    let rx = (vw - theme::MARGIN_X - r.width()).max(theme::MARGIN_X + l.width() + 16.0);
    let ry = y + (h - r.height()) * 0.5;
    draw_layout(scene, root, &r, rx, ry, Some(theme::p().row_hint));
}

pub fn draw_row_bg(scene: &mut Scene, root: Affine, px: f32, ry: f32, pw: f32, selected: bool) {
    if selected {
        fill(
            scene,
            root,
            theme::p().row_selected,
            &rrect(px + 4.0, ry, pw - 8.0, theme::ROW_H, 5.0),
        );
    }
}

/// Begin a group that draws at `alpha`, clipped to `shape`. **Every caller must
/// pair it with `scene.pop_layer()`.**
///
/// **One group, not an alpha on each colour.** A panel is a shadow, a fill, a
/// border and its rows stacked on top of each other; multiplying each brush's
/// alpha separately makes the shadow show *through* the panel it is behind and
/// the border show through the fill, so a half-faded menu looks like an X-ray of
/// itself rather than a faint menu. A real opacity group composites the stack
/// first and fades the result, which is what a browser does for `opacity`.
///
/// `transform` maps the clip shape, and the caller draws its contents under the
/// same transform — that is how a fade can also move.
///
/// It is not free: vello allocates a texture for the group. Fine for something
/// that appears for a tenth of a second, and the reason nothing at rest uses it.
pub fn fade_layer(
    scene: &mut Scene,
    transform: Affine,
    alpha: f32,
    shape: &impl vello::kurbo::Shape,
) {
    scene.push_layer(
        Fill::NonZero,
        peniko::Mix::Normal,
        alpha.clamp(0.0, 1.0),
        transform,
        shape,
    );
}

/// How far above the page a floating surface sits.
///
/// **Two, and there is no third.** Every floating thing in the app is one of
/// these, and which one it is follows from what it is *for* rather than from
/// how it looks:
///
/// - `Raised` — anything you act on: popovers, menus, modals, the selection
///   bubble, the pointer-following indicators. The shadow lifts it clear of
///   whatever it is covering, which is the thing that says "this, not that".
/// - `Flat` — tooltips. A tooltip tells you something and goes away; it is
///   frequently standing next to a popover, and two shadows at once is two
///   claims to being the front-most thing. It separates by being a dark chip
///   instead.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Elevation {
    Raised,
    Flat,
}

/// A floating surface: shadow, body, border.
///
/// **This exists because thirteen call sites drew the same three shapes by
/// hand and drifted apart three times.** The last drift was the visible one:
/// the `+`'s tooltip wore the soft shadow while the rename prompt and the
/// slash menu — the two surfaces that most need to read as lifted — still wore
/// the hard-edged slab it replaced. Nothing was wrong with any one call site;
/// what was missing was a place for the decision to live. It lives here, and
/// [`panel_shadow`] is `pub(crate)` so a fourteenth recipe cannot be written.
///
/// Build one with [`Panel::popover`] or [`Panel::tooltip`] and adjust from
/// there — the two constructors are the defaults, and a surface that overrides
/// something is saying so out loud.
#[derive(Clone, Copy, Debug)]
pub struct Panel {
    pub fill: Color,
    /// `None` for a surface whose own colour separates it: the bubble, the
    /// tear pill, every tooltip.
    pub border: Option<Color>,
    pub radius: f32,
    pub elevation: Elevation,
    /// Multiplies all three colours, shadow included.
    ///
    /// Only the zoom indicator sets anything but 1.0. It matters that the
    /// shadow is in here: fading a panel out while its shadow stayed put left
    /// the shadow as the last thing on screen, cast by nothing.
    pub alpha: f32,
}

impl Panel {
    /// Every popover, menu, modal and the bubble: `panel_bg`, a hairline, lift.
    pub fn popover(radius: f32) -> Self {
        let p = theme::p();
        Self {
            fill: p.panel_bg,
            border: Some(p.panel_border),
            radius,
            elevation: Elevation::Raised,
            alpha: 1.0,
        }
    }

    /// Every tooltip: the dark chip, no border, no lift.
    pub fn tooltip(radius: f32) -> Self {
        Self {
            fill: theme::p().bubble_bg,
            border: None,
            radius,
            elevation: Elevation::Flat,
            alpha: 1.0,
        }
    }

    pub fn fill(mut self, c: Color) -> Self {
        self.fill = c;
        self
    }

    pub fn borderless(mut self) -> Self {
        self.border = None;
        self
    }

    pub fn border(mut self, color: Color) -> Self {
        self.border = Some(color);
        self
    }

    pub fn faded(mut self, a: f32) -> Self {
        self.alpha = a;
        self
    }
}

/// Draw a floating surface — shadow, body, border, in that order.
///
/// Loose `x, y, w, h` rather than a [`Rect`](crate::geometry::Rect) to match
/// [`panel_shadow`], whose arguments every caller already builds, and to keep
/// `kurbo::Rect` and `geometry::Rect` from meeting in one signature.
#[track_caller]
pub fn floating_panel(
    scene: &mut Scene,
    root: Affine,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    panel: &Panel,
) {
    let _panelled = floating_panel_scope(scene, root, x, y, w, h, panel);
}

/// [`floating_panel`], with its inspector record left open.
///
/// **For a surface whose contents are the panel's.** `floating_panel` closes
/// its record as soon as the body is painted, so whatever the caller draws
/// next lands beside the `FloatingPanel`, not in it. Hold the returned guard
/// while drawing the contents and they nest under it instead.
#[track_caller]
#[must_use = "the contents nest under the panel only while the guard is held"]
pub fn floating_panel_scope(
    scene: &mut Scene,
    root: Affine,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    panel: &Panel,
) -> crate::inspect::Scope {
    // The panel is one piece of chrome however many paints it takes: named
    // here, at the one door every floating surface comes through, so no
    // caller's shadow or body ever shows up as anonymous fills.
    let _panelled =
        crate::inspect::scope("FloatingPanel", "part", root, rect(x, y, w, h), |record| {
            record.property(
                "fill",
                crate::inspect::color(panel.fill),
                "scene::Panel::fill",
            );
            record.property(
                "elevation",
                format!("{:?}", panel.elevation),
                "scene::Panel::elevation",
            );
            record.property("radius", panel.radius, "scene::Panel::radius");
        });
    let a = panel.alpha.clamp(0.0, 1.0);
    let dim = |c: Color| c.multiply_alpha(a);
    if panel.elevation == Elevation::Raised {
        panel_shadow(
            scene,
            root,
            dim(theme::p().panel_shadow),
            x,
            y,
            w,
            h,
            panel.radius,
        );
    }
    let shape = rrect(x, y, w, h, panel.radius);
    fill(scene, root, dim(panel.fill), &shape);
    if let Some(border) = panel.border {
        stroke(scene, root, dim(border), theme::BORDER_W, &shape);
    }
    _panelled
}

/// The box a floating surface occupies, its shadow included.
///
/// **For anything that pushes a layer around one.** A fade-in group clips to
/// its own shape, so a rect drawn to the panel's own edge cuts the shadow off
/// square — which was invisible while the spread was 7px and is not at 20.
///
/// A `Flat` surface has no shadow, so its bounds are its own rect: a fading
/// tooltip stops allocating a texture 23px larger on every side than the thing
/// it is drawing.
pub fn panel_bounds(x: f32, y: f32, w: f32, h: f32, elevation: Elevation) -> crate::geometry::Rect {
    match elevation {
        Elevation::Flat => crate::geometry::Rect::new(x, y, w, h),
        Elevation::Raised => {
            let m = SHADOW_SPREAD + SHADOW_DY;
            crate::geometry::Rect::new(x - m, y - m, w + m * 2.0, h + m * 2.0)
        }
    }
}

/// A soft drop shadow under a rounded panel.
///
/// **Rings, not one offset rectangle.** A single `panel_shadow` rrect behind a
/// panel gives a hard-edged slab with a visible border a few pixels out — which
/// reads as a second panel rather than as depth. This draws `SHADOW_RINGS`
/// concentric rounded rects, each a pixel further out and fainter than the last,
/// which is a blur cheap enough to do every frame: a handful of filled rrects
/// against a panel that is already costing text layout.
///
/// **Wide and faint, not tight and dark.** It was six rings over 7px at a gain
/// of 0.55, which put roughly a quarter of the shadow's alpha in the first
/// pixel and a half of it in the first two — a dark band hugging the panel that
/// read as part of the border rather than as depth, and made every popover look
/// like it had a heavier edge than the explore column's hairline. Sixteen rings
/// over 20px at a gain of 0.09 spread the same idea over ten times the area at a
/// tenth the strength: the darkest point is around 10% alpha and nothing about
/// it is an edge.
///
/// The falloff stays **quadratic** rather than linear, because the shadow
/// should still be densest nearest the panel — diffuse is not the same as flat.
///
/// Sixteen fills per panel rather than six. They are rrects with no text and no
/// clip, and at most two panels float at once; this is nowhere near the frame
/// budget the row loop spends.
///
/// `dy` pushes it down, so the light is above.
///
/// **`pub(crate)`, and that is the point.** Thirteen call sites used to decide
/// for themselves whether to call this, and three times they drifted apart —
/// most recently leaving the `+`'s tooltip lifted while the rename prompt sat
/// flat. Reaching it now means going through [`floating_panel`], where the
/// decision is an [`Elevation`] somebody has to name.
pub(crate) fn panel_shadow(
    scene: &mut Scene,
    root: Affine,
    colour: Color,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
) {
    // One shadow, one record: the rings are the paint recipe, not sixteen
    // things a person can point at. They are suspended below.
    let _shadow = crate::inspect::scope(
        "Shadow",
        "part",
        root,
        rect(
            x - SHADOW_SPREAD,
            y - SHADOW_SPREAD + SHADOW_DY,
            w + SHADOW_SPREAD * 2.0,
            h + SHADOW_SPREAD * 2.0,
        ),
        |record| {
            record.property(
                "rings",
                SHADOW_RINGS,
                "one soft shadow drawn as expanding fills; vello has no blur",
            );
            record.property(
                "spread / dy",
                format!("{SHADOW_SPREAD} / {SHADOW_DY}"),
                "SHADOW_SPREAD / SHADOW_DY",
            );
        },
    );
    let _rings = crate::inspect::suspend();
    let base = colour.components[3];
    for i in (0..SHADOW_RINGS).rev() {
        // 0 at the panel's own edge, 1 at the outside of the spread. The old
        // form started at `1/RINGS` and ended at `1`, so the outermost ring was
        // drawn fully transparent and the innermost was already a pixel out —
        // which is where the band came from.
        let t = i as f32 / (SHADOW_RINGS - 1).max(1) as f32;
        let grow = t * SHADOW_SPREAD;
        let alpha = base * (1.0 - t) * (1.0 - t) * SHADOW_GAIN;
        fill(
            scene,
            root,
            colour.with_alpha(alpha.clamp(0.0, 1.0)),
            &rrect(
                x - grow,
                y - grow + SHADOW_DY,
                w + grow * 2.0,
                h + grow * 2.0,
                radius + grow,
            ),
        );
    }
}

/// How many rings the soft shadow is made of, and how far the outermost sits.
///
/// Sixteen over 20px: enough rings that the steps are invisible at 1× *and* the
/// gradient is smooth over a spread this wide. Six over 7px banded — the steps
/// were 1.2px apart and each one carried a tenth of the whole shadow.
const SHADOW_RINGS: usize = 16;
const SHADOW_SPREAD: f32 = 20.0;
/// Downward offset: the light is above. Larger with the wider spread, or the
/// shadow reads as a halo centred on the panel rather than as something cast.
const SHADOW_DY: f32 = 3.0;
/// Scales the whole stack.
///
/// Sixteen overlapping fills accumulate, so this is what sets the *composite*:
/// roughly `base * GAIN * RINGS / 3` at the panel's edge, which for the palette's
/// `panel_shadow` works out near 10% alpha. Raising it is how the band comes
/// back.
const SHADOW_GAIN: f32 = 0.09;

#[cfg(test)]
mod tests {
    use super::*;

    /// **A flat surface's bounds are its own rect.** The fade layer a tooltip
    /// pushes clips to whatever this returns, so a raised answer here is a
    /// texture 23px larger on every side than the thing being drawn — and the
    /// only reason the margin existed was a shadow a tooltip no longer casts.
    #[test]
    fn a_flat_panel_occupies_only_itself() {
        let flat = panel_bounds(10.0, 20.0, 100.0, 40.0, Elevation::Flat);
        assert_eq!((flat.x, flat.y, flat.w, flat.h), (10.0, 20.0, 100.0, 40.0));
    }

    /// A raised one has to contain its shadow, or the fade clips it off square.
    #[test]
    fn a_raised_panel_contains_its_shadow() {
        let (x, y, w, h) = (10.0, 20.0, 100.0, 40.0);
        let r = panel_bounds(x, y, w, h, Elevation::Raised);
        assert!(r.x < x && r.y < y, "it starts outside the panel");
        assert!(
            r.x + r.w > x + w && r.y + r.h > y + h + SHADOW_DY,
            "and ends outside it, the downward offset included"
        );
    }

    /// The shadow is **one** record however many rings paint it — sixteen
    /// anonymous fills per panel was the whole of a modal's inspector tree.
    #[test]
    fn a_floating_panel_records_one_shadow_not_sixteen_rings() {
        let mut scene = Scene::new();
        let root = Affine::IDENTITY;
        let capture = crate::inspect::Frame::begin(1, root, rect(0.0, 0.0, 800.0, 600.0));
        floating_panel(
            &mut scene,
            root,
            10.0,
            20.0,
            100.0,
            40.0,
            &Panel::popover(8.0),
        );
        let records = capture.finish().records;
        assert_eq!(
            records.iter().filter(|r| r.name == "FloatingPanel").count(),
            1
        );
        assert_eq!(records.iter().filter(|r| r.name == "Shadow").count(), 1);
        assert_eq!(
            records.iter().filter(|r| r.name == "fill").count(),
            1,
            "the panel's body is the only fill; the rings are suspended"
        );
    }

    /// The two constructors are the whole rule: a popover lifts and wears a
    /// border, a tooltip does neither. Everything else in the app is one of
    /// these two with a colour swapped.
    #[test]
    fn the_two_defaults_say_what_they_are() {
        let popover = Panel::popover(8.0);
        assert_eq!(popover.elevation, Elevation::Raised);
        assert!(popover.border.is_some());

        let tooltip = Panel::tooltip(6.0);
        assert_eq!(tooltip.elevation, Elevation::Flat);
        assert!(tooltip.border.is_none());
        assert_eq!(
            tooltip.fill,
            theme::p().bubble_bg,
            "the chip, not the page's own colour"
        );

        let outlined = tooltip.border(theme::p().panel_border);
        assert_eq!(outlined.border, Some(theme::p().panel_border));
    }
}

#[cfg(test)]
mod centring_tests {
    // `Rect` here is the layout one, not kurbo's — `super::*` brings the latter.
    use super::{centre_in, ink_centre};
    use crate::theme::{FontChoice, FontWeight, TextMetrics, TextStyle};

    /// A tile is sized by its own square, not by a text tier — so these tests
    /// name a pixel size the way `tile.rs` does.
    fn at(size: f32) -> TextStyle {
        TextStyle::new(TextMetrics::new(size, size), FontWeight::Medium)
    }
    use crate::geometry::Rect;
    use crate::text::TextEngine;
    use crate::theme::Brush;

    /// **The bug this replaced, measured.** Centring by font metrics puts a
    /// colour emoji visibly high, because ascent and descent describe the box a
    /// line reserves rather than the box the glyph fills.
    ///
    /// Asserted against the *ink*: whatever `centre_in` returns, the drawn
    /// pixels must straddle the box's middle. Half a pixel of slack, because
    /// the answer lands on a pixel grid anyway.
    #[test]
    fn a_glyph_is_centred_on_its_ink_not_on_its_line_box() {
        let mut e = TextEngine::new();
        let box_ = Rect::new(0.0, 0.0, 40.0, 40.0);
        for text in ["W", "x", "\u{1F52C}", "\u{1F92A}"] {
            let l = e.simple_layout(text, at(20.0));
            let (ox, oy) = centre_in(&l, box_);
            let (cx, cy) = ink_centre(&l).expect("these faces report bounds");
            assert!(
                (ox + cx - (box_.x + box_.w * 0.5)).abs() < 0.5,
                "{text}: ink is off-centre horizontally"
            );
            assert!(
                (oy + cy - (box_.y + box_.h * 0.5)).abs() < 0.5,
                "{text}: ink is off-centre vertically"
            );
        }
    }

    /// **Why only emoji ever looked wrong.** For Latin text a font's metrics and
    /// its ink agree to a fraction of a pixel, so the old formula was right
    /// there; for an emoji they disagree by about 8% of the size, which is what
    /// the eye caught as "more space below than above".
    #[test]
    fn metrics_and_ink_agree_for_text_and_disagree_for_emoji() {
        let mut e = TextEngine::new();
        let box_ = Rect::new(0.0, 0.0, 40.0, 40.0);
        let by_metrics = |l: &parley::Layout<Brush>| {
            let m = *l.lines().next().unwrap().metrics();
            box_.y + box_.h * 0.5 + (m.ascent - m.descent) * 0.5 - m.baseline
        };

        // Pinned to Inter rather than the UI font: the claim is about text
        // versus emoji, and the UI default is the *system* face on macOS now,
        // whose metrics agree with its ink less tightly than a bundled
        // variable font's do.
        let w = e.simple_layout_in("W", at(20.0), FontChoice::Inter.stack());
        assert!(
            (centre_in(&w, box_).1 - by_metrics(&w)).abs() < 0.1,
            "metrics were already right for text"
        );

        let emoji = e.simple_layout_in("\u{1F52C}", at(20.0), FontChoice::Inter.stack());
        let drift = centre_in(&emoji, box_).1 - by_metrics(&emoji);
        assert!(
            drift > 1.0,
            "the correction moves an emoji *down* by a visible amount, not {drift:.2}px"
        );
    }

    /// It centres horizontally too, and follows the box wherever it is.
    #[test]
    fn centring_follows_the_box() {
        let mut e = TextEngine::new();
        let l = e.simple_layout("W", at(18.0));
        let a = centre_in(&l, Rect::new(0.0, 0.0, 40.0, 40.0));
        let b = centre_in(&l, Rect::new(100.0, 200.0, 40.0, 40.0));
        assert!((b.0 - a.0 - 100.0).abs() < 0.01);
        assert!((b.1 - a.1 - 200.0).abs() < 0.01);
        // And the glyph is inside the box it was centred in.
        assert!(a.0 >= 0.0 && a.0 + l.width() <= 40.0);
    }

    /// A tile is square at every size, so the same text centres the same way at
    /// 18 px and at 48 — which is what lets one component serve five callers.
    #[test]
    fn centring_holds_at_every_tile_size() {
        let mut e = TextEngine::new();
        for size in [18.0f32, 20.0, 26.0, 46.0, 48.0] {
            let l = e.simple_layout("\u{1F52C}", at(size * 0.46));
            let box_ = Rect::new(0.0, 0.0, size, size);
            let (x, y) = centre_in(&l, box_);
            let slack = 0.51;
            assert!(
                ((x + l.width() * 0.5) - size * 0.5).abs() < slack,
                "size {size}: not horizontally centred"
            );
            assert!(
                y >= -slack && y + l.height() <= size + l.height(),
                "size {size}"
            );
        }
    }
}
